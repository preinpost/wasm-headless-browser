//! `whb`: a test client that drives wasm-headless-browser over CDP.
//!
//! Not a release artefact, and not what pi invokes. pi runs host
//! `agent-browser --cdp` against the engine. This binary exists so
//! `verify-client.sh` and harness E7 can drive the engine without the patched
//! agent-browser module.
//!
//! Each invocation is one command: connect, attach to this session's tab, do
//! the one thing, print, exit. The commands and their output follow a subset
//! of agent-browser's (`open`, `snapshot -i`, `click @e3`, ...). Unlike
//! agent-browser there is no daemon: the engine itself keeps the tab between
//! commands, and this keeps only a small state file (`state.rs`).
//!
//! It is built for the same WASIX sandbox as the engine. It uses blocking std
//! sockets only, which work under Wasmer where async ones do not.

mod cdp;
mod snapshot;
mod state;

use base64::Engine as _;
use serde_json::{json, Value};

use cdp::{Cdp, Endpoint};
use state::State;

const USAGE: &str = "whb - drive wasm-headless-browser over CDP

Usage: whb [--cdp <url>] [--session <name>] <command> [args]

Commands:
  open <url>                 Load a page in this session's tab
  snapshot [-i]              The accessibility tree; -i lists only elements with refs
  click <target>             Click an element at the centre of its box
  fill <target> <text>       Replace a field's text ({{secret:name}} placeholders allowed)
  press <key>                Press a key at the focus: Enter, Tab, Escape, Control+a, ...
  eval <js>                  Evaluate JavaScript in the page and print the result as JSON
  get url|title              The page's URL or title
  get text|value <target>    An element's visible text, or a field's value
  screenshot [path] [--full] Save a PNG of the viewport, or of the whole page
  close                      Close this session's tab

A <target> is a ref from the last snapshot (@e3) or a CSS selector.

Options:
  --cdp <url>       The engine: a port, host:port, http://... or ws://...
                    (default $WHB_CDP, else 9222)
  --session <name>  Which tab to drive (default $WHB_SESSION, else \"default\")

Environment:
  WHB_STATE_DIR     Where sessions are kept (default $HOME/.whb)
  WHB_TIMEOUT_MS    How long to wait for the engine (default 60000)";

/// A per-document identity, kept on the window as a non-enumerable value. A
/// ref is only good for the document whose snapshot made it: after a
/// navigation the same node id names some other element.
const DOCUMENT_ID: &str = "(() => { const k = '__whbDocument'; \
    if (!Object.prototype.hasOwnProperty.call(window, k)) Object.defineProperty(window, k, \
    { value: Date.now().toString(36) + Math.random().toString(36).slice(2), enumerable: false }); \
    return window[k]; })()";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
        }
        Err(e) => {
            eprintln!("\u{2717} {e}");
            std::process::exit(1);
        }
    }
}

struct Options {
    cdp: String,
    session: String,
    rest: Vec<String>,
}

fn options(args: Vec<String>) -> Result<Options, String> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut cdp = env("WHB_CDP").unwrap_or_else(|| "9222".into());
    let mut session = env("WHB_SESSION").unwrap_or_else(|| "default".into());
    let mut rest = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cdp" => cdp = args.next().ok_or("--cdp needs a value")?,
            "--session" => session = args.next().ok_or("--session needs a value")?,
            _ if arg.starts_with("--cdp=") => cdp = arg["--cdp=".len()..].to_string(),
            _ if arg.starts_with("--session=") => session = arg["--session=".len()..].to_string(),
            _ => rest.push(arg),
        }
    }
    state::check_session(&session)?;
    Ok(Options { cdp, session, rest })
}

fn run(args: Vec<String>) -> Result<String, String> {
    let options = options(args)?;
    let Some(command) = options.rest.first().cloned() else {
        return Ok(USAGE.to_string());
    };
    let args = &options.rest[1..];
    match command.as_str() {
        "help" | "--help" | "-h" => return Ok(USAGE.to_string()),
        "--version" | "-V" | "version" => return Ok(format!("whb {}", env!("CARGO_PKG_VERSION"))),
        _ => {}
    }
    let endpoint = Endpoint::parse(&options.cdp)?;
    let mut state = State::load(&options.session)?;
    let mut cdp = Cdp::connect(&endpoint)?;
    let page = page(&mut cdp, &mut state);
    // Whatever happens next, a tab that was made is remembered.
    let result = page.and_then(|session| {
        let mut ctx = Context { cdp: &mut cdp, session, state: &mut state };
        ctx.command(&command, args)
    });
    if command == "close" && result.is_ok() {
        State::remove(&options.session);
    } else {
        state.save(&options.session)?;
    }
    result
}

/// Attaches to this session's tab, or opens one. Returns the CDP session.
fn page(cdp: &mut Cdp, state: &mut State) -> Result<String, String> {
    if let Some(target) = state.target.clone() {
        if let Ok(attached) = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true })) {
            if let Some(session) = attached["sessionId"].as_str() {
                return Ok(session.to_string());
            }
        }
        // The engine restarted or the tab was closed elsewhere.
        state.target = None;
        state.forget_refs();
    }
    let created = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
    let target = created["targetId"].as_str().ok_or("Target.createTarget gave no targetId")?.to_string();
    let attached = cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true }))?;
    let session = attached["sessionId"].as_str().ok_or("Target.attachToTarget gave no sessionId")?.to_string();
    state.target = Some(target);
    state.forget_refs();
    Ok(session)
}

/// An element to act on.
enum Element {
    Backend(u64),
    Object(String),
}

impl Element {
    fn params(&self) -> Value {
        match self {
            Element::Backend(id) => json!({ "backendNodeId": id }),
            Element::Object(id) => json!({ "objectId": id }),
        }
    }
}

struct Context<'a> {
    cdp: &'a mut Cdp,
    session: String,
    state: &'a mut State,
}

fn arg<'a>(args: &'a [String], at: usize, what: &str) -> Result<&'a str, String> {
    args.get(at).map(String::as_str).ok_or_else(|| format!("missing {what}; see whb --help"))
}

impl Context<'_> {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.cdp.call(Some(&self.session), method, params)
    }

    fn command(&mut self, command: &str, args: &[String]) -> Result<String, String> {
        match command {
            "open" | "goto" | "navigate" => self.open(arg(args, 0, "the URL")?),
            "snapshot" => self.snapshot(args.iter().any(|a| a == "-i" || a == "--interactive")),
            "click" => {
                let element = self.element(arg(args, 0, "the element")?)?;
                self.click(&element)
            }
            "fill" => {
                let element = self.element(arg(args, 0, "the element")?)?;
                self.fill(&element, arg(args, 1, "the text")?)
            }
            "press" | "key" => self.press(arg(args, 0, "the key")?),
            "eval" => self.eval(&args.join(" ")),
            "get" => self.get(arg(args, 0, "what to get")?, args.get(1).map(String::as_str)),
            "screenshot" => {
                let full = args.iter().any(|a| a == "--full" || a == "-f");
                let path = args.iter().find(|a| !a.starts_with('-')).cloned();
                self.screenshot(path, full)
            }
            "close" => {
                let target = self.state.target.clone().unwrap_or_default();
                self.cdp.call(None, "Target.closeTarget", json!({ "targetId": target }))?;
                Ok("\u{2713} Closed".into())
            }
            other => Err(format!("unknown command {other:?}; see whb --help")),
        }
    }

    /// Evaluates an expression and returns its value, raising page errors.
    fn value(&mut self, expression: &str) -> Result<Value, String> {
        let reply = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
        )?;
        if let Some(exception) = reply.get("exceptionDetails") {
            let detail = exception["exception"]["description"].as_str().or(exception["text"].as_str()).unwrap_or("exception");
            return Err(detail.to_string());
        }
        Ok(reply["result"].get("value").cloned().unwrap_or(Value::Null))
    }

    fn string(&mut self, expression: &str) -> Result<String, String> {
        Ok(match self.value(expression)? {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        })
    }

    fn open(&mut self, url: &str) -> Result<String, String> {
        let has_scheme = url.contains("://") || ["about:", "data:", "javascript:", "file:"].iter().any(|s| url.starts_with(s));
        let url = if has_scheme { url.to_string() } else { format!("https://{url}") };
        let navigated = self.call("Page.navigate", json!({ "url": url }))?;
        self.state.forget_refs();
        if let Some(error) = navigated.get("errorText").and_then(Value::as_str) {
            return Err(format!("{url}: {error}"));
        }
        let title = self.string("document.title")?;
        let href = self.string("location.href")?;
        Ok(format!("\u{2713} {title}\n  {href}"))
    }

    fn snapshot(&mut self, interactive_only: bool) -> Result<String, String> {
        let tree = self.call("Accessibility.getFullAXTree", json!({}))?;
        let document = self.string(DOCUMENT_ID)?;
        if self.state.document.as_deref() != Some(document.as_str()) {
            self.state.forget_refs();
            self.state.document = Some(document);
        }
        // An element keeps its ref across snapshots of one document.
        let state = &mut *self.state;
        let mut known: std::collections::HashMap<u64, String> = state.refs.iter().map(|(name, id)| (*id, name.clone())).collect();
        let mut assign = |backend: u64| -> String {
            if let Some(name) = known.get(&backend) {
                return name.clone();
            }
            state.last_ref += 1;
            let name = format!("e{}", state.last_ref);
            state.refs.insert(name.clone(), backend);
            known.insert(backend, name.clone());
            name
        };
        let text = snapshot::render(&tree, interactive_only, &mut assign);
        Ok(if text.is_empty() {
            (if interactive_only { "(no interactive elements)" } else { "(empty page)" }).to_string()
        } else {
            text
        })
    }

    /// A ref from the last snapshot of this document (`@e3`, or `e3`), or a
    /// CSS selector. Anything starting with `@` must be a ref.
    fn element(&mut self, target: &str) -> Result<Element, String> {
        let name = target.strip_prefix('@').unwrap_or(target);
        let is_ref = name.len() > 1 && name.starts_with('e') && name[1..].chars().all(|c| c.is_ascii_digit());
        if target.starts_with('@') && !is_ref {
            return Err(format!("{target}: not a ref; refs look like @e3"));
        }
        if is_ref {
            let backend = *self.state.refs.get(name).ok_or_else(|| format!("unknown ref @{name}; take a snapshot first"))?;
            let document = self.string(DOCUMENT_ID)?;
            if self.state.document.as_deref() != Some(document.as_str()) {
                return Err(format!("the page changed since the snapshot that made @{name}; take a new snapshot"));
            }
            return Ok(Element::Backend(backend));
        }
        let selector = serde_json::to_string(target).unwrap_or_default();
        let reply = self.call("Runtime.evaluate", json!({ "expression": format!("document.querySelector({selector})") }))?;
        if reply.get("exceptionDetails").is_some() {
            return Err(format!("{target:?} is not a valid selector"));
        }
        match reply["result"]["objectId"].as_str() {
            Some(id) => Ok(Element::Object(id.to_string())),
            None => Err(format!("no element matches {target:?}")),
        }
    }

    fn object_id(&mut self, element: &Element) -> Result<String, String> {
        match element {
            Element::Object(id) => Ok(id.clone()),
            Element::Backend(_) => {
                let resolved = self.call("DOM.resolveNode", element.params())?;
                resolved["object"]["objectId"].as_str().map(str::to_string).ok_or_else(|| "the element is gone".into())
            }
        }
    }

    fn on(&mut self, element: &Element, function: &str) -> Result<Value, String> {
        let object = self.object_id(element)?;
        let reply = self.call(
            "Runtime.callFunctionOn",
            json!({ "objectId": object, "functionDeclaration": function, "returnByValue": true }),
        )?;
        if let Some(exception) = reply.get("exceptionDetails") {
            return Err(exception["exception"]["description"].as_str().unwrap_or("exception").to_string());
        }
        Ok(reply["result"].get("value").cloned().unwrap_or(Value::Null))
    }

    fn click(&mut self, element: &Element) -> Result<String, String> {
        self.call("DOM.scrollIntoViewIfNeeded", element.params())?;
        let quads = self.call("DOM.getContentQuads", element.params())?;
        let quad: Vec<f64> = quads["quads"][0].as_array().map(|q| q.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        if quad.len() != 8 {
            return Err("the element has no box to click; it may be hidden".into());
        }
        let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
        let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
        for (kind, button, buttons) in [("mouseMoved", "none", 0), ("mousePressed", "left", 1), ("mouseReleased", "left", 0)] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": 1 }),
            )?;
        }
        Ok("\u{2713} Done".into())
    }

    fn fill(&mut self, element: &Element, text: &str) -> Result<String, String> {
        // Focus, select what is there, and type over it: the engine routes
        // typing through the page's events, so frameworks see a real edit,
        // and it alone substitutes {{secret:name}} placeholders.
        self.on(
            element,
            "function () { this.focus(); \
             if (typeof this.setSelectionRange === 'function') { try { this.setSelectionRange(0, String(this.value ?? '').length); return; } catch (e) {} } \
             if (typeof this.select === 'function') this.select(); }",
        )?;
        if text.is_empty() {
            self.key("Backspace")?;
        } else {
            self.call("Input.insertText", json!({ "text": text }))?;
        }
        Ok("\u{2713} Done".into())
    }

    fn press(&mut self, key: &str) -> Result<String, String> {
        self.key(key)?;
        Ok("\u{2713} Done".into())
    }

    fn key(&mut self, spec: &str) -> Result<(), String> {
        let key = Key::parse(spec)?;
        let mut down = json!({
            "type": if key.text.is_empty() { "rawKeyDown" } else { "keyDown" },
            "key": key.key, "code": key.code, "modifiers": key.modifiers,
            "windowsVirtualKeyCode": key.virtual_code, "nativeVirtualKeyCode": key.virtual_code,
        });
        if !key.text.is_empty() {
            down["text"] = json!(key.text);
            down["unmodifiedText"] = json!(key.text);
        }
        self.call("Input.dispatchKeyEvent", down)?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp", "key": key.key, "code": key.code, "modifiers": key.modifiers,
                "windowsVirtualKeyCode": key.virtual_code, "nativeVirtualKeyCode": key.virtual_code,
            }),
        )?;
        Ok(())
    }

    fn eval(&mut self, expression: &str) -> Result<String, String> {
        if expression.trim().is_empty() {
            return Err("missing the expression; see whb --help".into());
        }
        let reply = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
        )?;
        if let Some(exception) = reply.get("exceptionDetails") {
            let detail = exception["exception"]["description"].as_str().or(exception["text"].as_str()).unwrap_or("exception");
            return Err(detail.to_string());
        }
        let result = &reply["result"];
        Ok(match result.get("value") {
            Some(value) => serde_json::to_string(value).unwrap_or_default(),
            None => result["type"].as_str().unwrap_or("undefined").to_string(),
        })
    }

    fn get(&mut self, what: &str, target: Option<&str>) -> Result<String, String> {
        match what {
            "url" => self.string("location.href"),
            "title" => self.string("document.title"),
            "text" | "value" => {
                let element = self.element(target.ok_or_else(|| format!("get {what} needs an element"))?)?;
                let function = if what == "text" {
                    "function () { return this.innerText ?? this.textContent ?? ''; }"
                } else {
                    "function () { return this.value ?? ''; }"
                };
                Ok(match self.on(&element, function)? {
                    Value::String(s) => s,
                    other => other.to_string(),
                })
            }
            other => Err(format!("get {other:?}: expected url, title, text or value")),
        }
    }

    fn screenshot(&mut self, path: Option<String>, full: bool) -> Result<String, String> {
        let mut params = json!({ "format": "png" });
        if full {
            let metrics = self.call("Page.getLayoutMetrics", json!({}))?;
            let size = &metrics["cssContentSize"];
            let (w, h) = (size["width"].as_f64().unwrap_or(0.0), size["height"].as_f64().unwrap_or(0.0));
            if w > 0.0 && h > 0.0 {
                params["clip"] = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
                params["captureBeyondViewport"] = json!(true);
            }
        }
        let shot = self.call("Page.captureScreenshot", params)?;
        let data = shot["data"].as_str().ok_or("Page.captureScreenshot gave no data")?;
        let png = base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?;
        let path = path.unwrap_or_else(|| {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            format!("screenshot-{stamp}.png")
        });
        std::fs::write(&path, png).map_err(|e| format!("{path}: {e}"))?;
        Ok(format!("\u{2713} Screenshot saved to {path}"))
    }
}

/// A key as `Input.dispatchKeyEvent` wants it.
#[derive(Debug, PartialEq)]
struct Key {
    key: String,
    code: String,
    text: String,
    virtual_code: u32,
    modifiers: u32,
}

impl Key {
    /// `Enter`, `a`, `Control+a`, `Shift+Tab`: modifiers joined by `+`.
    fn parse(spec: &str) -> Result<Key, String> {
        let parts: Vec<&str> = spec.split('+').collect();
        let (name, held) = match parts.split_last() {
            // `+` itself, or a combination ending in it.
            Some((last, held)) if last.is_empty() && !held.is_empty() => ("+", &held[..held.len() - 1]),
            Some((last, held)) => (*last, held),
            None => return Err("missing the key".into()),
        };
        let mut modifiers = 0;
        for m in held {
            modifiers |= match m.to_ascii_lowercase().as_str() {
                "alt" | "option" => 1,
                "control" | "ctrl" => 2,
                "meta" | "cmd" | "command" | "super" => 4,
                "shift" => 8,
                other => return Err(format!("unknown modifier {other:?} in {spec:?}")),
            };
        }
        let named = |key: &str, code: &str, text: &str, virtual_code: u32| (key.to_string(), code.to_string(), text.to_string(), virtual_code);
        let (key, code, mut text, virtual_code) = match name {
            "Enter" | "Return" => named("Enter", "Enter", "\r", 13),
            "Tab" => named("Tab", "Tab", "", 9),
            "Escape" | "Esc" => named("Escape", "Escape", "", 27),
            "Backspace" => named("Backspace", "Backspace", "", 8),
            "Delete" => named("Delete", "Delete", "", 46),
            "Space" | " " => named(" ", "Space", " ", 32),
            "ArrowUp" => named("ArrowUp", "ArrowUp", "", 38),
            "ArrowDown" => named("ArrowDown", "ArrowDown", "", 40),
            "ArrowLeft" => named("ArrowLeft", "ArrowLeft", "", 37),
            "ArrowRight" => named("ArrowRight", "ArrowRight", "", 39),
            "Home" => named("Home", "Home", "", 36),
            "End" => named("End", "End", "", 35),
            "PageUp" => named("PageUp", "PageUp", "", 33),
            "PageDown" => named("PageDown", "PageDown", "", 34),
            single if single.chars().count() == 1 => {
                let c = single.chars().next().unwrap_or(' ');
                let code = if c.is_ascii_alphabetic() {
                    format!("Key{}", c.to_ascii_uppercase())
                } else if c.is_ascii_digit() {
                    format!("Digit{c}")
                } else {
                    String::new()
                };
                let virtual_code = if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() as u32 } else { 0 };
                (c.to_string(), code, c.to_string(), virtual_code)
            }
            other => return Err(format!("unknown key {other:?}")),
        };
        // With Control, Alt or Meta held a key is a shortcut, not typing.
        if modifiers & (1 | 2 | 4) != 0 {
            text.clear();
        }
        Ok(Key { key, code, text, virtual_code, modifiers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_with_their_codes_and_modifiers() {
        let enter = Key::parse("Enter").unwrap();
        assert_eq!((enter.key.as_str(), enter.code.as_str(), enter.text.as_str(), enter.virtual_code), ("Enter", "Enter", "\r", 13));
        let a = Key::parse("a").unwrap();
        assert_eq!((a.code.as_str(), a.text.as_str(), a.virtual_code, a.modifiers), ("KeyA", "a", 65, 0));
        let select_all = Key::parse("Control+a").unwrap();
        assert_eq!((select_all.text.as_str(), select_all.modifiers), ("", 2));
        let back_tab = Key::parse("Shift+Tab").unwrap();
        assert_eq!((back_tab.key.as_str(), back_tab.modifiers), ("Tab", 8));
        assert_eq!(Key::parse("Control++").unwrap().key, "+");
        assert_eq!(Key::parse("7").unwrap().code, "Digit7");
        assert!(Key::parse("Hyper+a").is_err());
        assert!(Key::parse("NoSuchKey").is_err());
    }

    #[test]
    fn options_come_before_or_after_the_command() {
        let parsed = options(vec!["--cdp".into(), "9333".into(), "open".into(), "x.com".into(), "--session=s1".into()]).unwrap();
        assert_eq!((parsed.cdp.as_str(), parsed.session.as_str()), ("9333", "s1"));
        assert_eq!(parsed.rest, vec!["open".to_string(), "x.com".to_string()]);
        assert!(options(vec!["--session".into(), "../etc".into()]).is_err());
        assert!(options(vec!["--cdp".into()]).is_err());
    }
}
