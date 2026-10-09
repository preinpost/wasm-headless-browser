//! `wasm-headless-browser` — a CDP-speaking headless browser.
//!
//! M0 serves the discovery endpoints and a browser-level WebSocket. The page
//! it hands out comes from `engine-fake`, so this binary proves the protocol
//! path end to end before any HTML is parsed.

use std::io::{self, BufRead, IsTerminal, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use cdp_wire::{serve, Connection, Handler, Request, Response};
use browser_core::{Browser, EngineFactory};
use engine::secrets::Secrets;
use engine::HtmlEngine;
use engine_fake::FakeEngine;
use serde_json::{json, Value};

#[derive(Clone)]
struct CdpHandler {
    browser: Arc<Mutex<Browser>>,
}

impl CdpHandler {
    fn ws_url(&self, req: &Request) -> String {
        let host = req
            .header("host")
            .map(str::to_string)
            .unwrap_or_else(|| "127.0.0.1:9222".to_string());
        let browser = self.browser.lock().unwrap();
        format!("ws://{host}/devtools/browser/{}", browser.browser_id)
    }
}

impl Handler for CdpHandler {
    fn http(&self, req: &Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/json/version") | ("GET", "/json/version/") => {
                let ws_url = self.ws_url(req);
                let browser = self.browser.lock().unwrap();
                Response::json(
                    serde_json::to_vec(&json!({
                        "Browser": browser.product,
                        "Protocol-Version": "1.3",
                        "User-Agent": browser.user_agent,
                        "V8-Version": "0.0.0",
                        "WebKit-Version": "0.0.0 (@wasm-headless-browser)",
                        "webSocketDebuggerUrl": ws_url,
                    }))
                    .unwrap_or_default(),
                )
            }

            ("GET", "/json/list") | ("GET", "/json/list/") | ("GET", "/json") => {
                let ws_url = self.ws_url(req);
                let browser = self.browser.lock().unwrap();
                Response::json(
                    serde_json::to_vec(&json!([{
                        "id": browser.browser_id,
                        "type": "page",
                        "title": "",
                        "url": "about:blank",
                        "description": "wasm-headless-browser",
                        "webSocketDebuggerUrl": ws_url,
                        "devtoolsFrontendUrl": "",
                    }]))
                    .unwrap_or_default(),
                )
            }

            ("GET", "/json/protocol") => Response::json(b"{}".to_vec()),

            (_, "/json/new") => {
                let ws_url = self.ws_url(req);
                let mut browser = self.browser.lock().unwrap();
                let target_id = browser.add_target();
                Response::json(
                    serde_json::to_vec(&json!({
                        "id": target_id,
                        "type": "page",
                        "title": "",
                        "url": "about:blank",
                        "webSocketDebuggerUrl": ws_url,
                    }))
                    .unwrap_or_default(),
                )
            }

            _ => Response::text(404, "not found"),
        }
    }

    fn websocket(&self, conn: &mut Connection, _path: &str) -> io::Result<()> {
        loop {
            let text = match conn.recv_text()? {
                Some(t) => t,
                None => return Ok(()),
            };

            let msg: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("dropping malformed CDP message: {e}");
                    continue;
                }
            };

            // Events are collected first so the browser lock is not held while
            // writing to the socket.
            let mut events: Vec<(Option<String>, String, Value)> = Vec::new();
            let response = {
                let mut browser = self.browser.lock().unwrap();
                let mut emit = |session: Option<&str>, method: &str, params: Value| {
                    events.push((session.map(str::to_string), method.to_string(), params));
                };
                browser.handle(&msg, &mut emit)
            };

            for (session, method, params) in events {
                let mut event = json!({ "method": method, "params": params });
                if let Some(session) = session {
                    event["sessionId"] = json!(session);
                }
                conn.send_json(&event)?;
            }

            if let Some(response) = response {
                conn.send_json(&response)?;
            }
        }
    }
}

/// Which page model to serve. `fake` stays the default so that the M0
/// acceptance test keeps testing what it was written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineChoice {
    Fake,
    Html,
}

impl EngineChoice {
    fn factory(self, secrets: Secrets) -> EngineFactory {
        match self {
            EngineChoice::Fake => FakeEngine::factory(),
            EngineChoice::Html => HtmlEngine::factory_with_secrets(Arc::new(secrets)),
        }
    }

    fn label(self) -> &'static str {
        match self {
            EngineChoice::Fake => "M0, fake engine",
            EngineChoice::Html => "real engine (HTML, CSS, layout, JavaScript, cookies; no rasterizer)",
        }
    }
}

/// Reads one value per `--secret`, in order, a line each, from stdin.
///
/// Values come only this way: not as arguments, which `ps` shows, nor from
/// the environment, which child processes and crash reports inherit, nor
/// from a file, which would put them on a disk. A terminal is refused,
/// because it would echo what is typed; pipe them in instead, for example
/// from the macOS keychain with `security find-generic-password -w`.
fn read_secrets(specs: &[String]) -> Result<Secrets, String> {
    let mut secrets = Secrets::new();
    if specs.is_empty() {
        return Ok(secrets);
    }
    let stdin = io::stdin();
    // Under Wasmer a guest's stdin always looks like a terminal, piped or
    // not, so the check would refuse every run. scripts/wasix-run.sh makes
    // it on the host instead, where the answer is true.
    if !cfg!(target_os = "wasi") && stdin.is_terminal() {
        return Err("--secret reads its value from stdin, and stdin is a terminal, which would show it as it \
                    is typed. Pipe the values in, one line per --secret, in order."
            .into());
    }
    let mut lines = stdin.lock();
    for spec in specs {
        let mut value = String::new();
        let read = lines.read_line(&mut value).map_err(|e| format!("reading the value for {spec}: {e}"))?;
        if read == 0 {
            return Err(format!("stdin ended before the value for --secret {spec}"));
        }
        while value.ends_with('\n') || value.ends_with('\r') {
            value.pop();
        }
        secrets.add(spec, value).map_err(|e| e.to_string())?;
    }
    Ok(secrets)
}

fn usage() -> ! {
    eprintln!(
        "usage: wasm-headless-browser [--engine fake|html] [--secret NAME@ORIGIN]... [addr]\n\n  \
         --engine fake  fixed page model, no parsing (default)\n  \
         --engine html  fetches and runs real pages; no rasterizer yet\n  \
         --secret NAME@ORIGIN\n                \
         a credential the client types as {{{{secret:NAME}}}}, only into pages from ORIGIN.\n                \
         Values are read from stdin, one line per --secret, in order. They are never\n                \
         sent back over CDP: the placeholder is, wherever the value would appear."
    );
    std::process::exit(2)
}

fn parse_args() -> (EngineChoice, String, Vec<String>) {
    let mut engine = EngineChoice::Fake;
    let mut addr: Option<String> = None;
    let mut secrets = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--engine" => {
                engine = match args.next().as_deref() {
                    Some("fake") => EngineChoice::Fake,
                    Some("html") => EngineChoice::Html,
                    _ => usage(),
                }
            }
            "--secret" => match args.next() {
                Some(spec) => secrets.push(spec),
                None => usage(),
            },
            "-h" | "--help" => usage(),
            other if other.starts_with('-') => usage(),
            other => addr = Some(other.to_string()),
        }
    }
    (engine, addr.unwrap_or_else(|| "127.0.0.1:9222".to_string()), secrets)
}

fn main() -> io::Result<()> {
    let (engine, addr, secret_specs) = parse_args();
    if !secret_specs.is_empty() && engine != EngineChoice::Html {
        eprintln!("--secret needs --engine html");
        std::process::exit(2);
    }
    let secrets = match read_secrets(&secret_specs) {
        Ok(secrets) => secrets,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let held = secrets.describe();

    let listener = TcpListener::bind(&addr)?;
    let local: SocketAddr = listener.local_addr()?;

    let handler = CdpHandler { browser: Arc::new(Mutex::new(Browser::new(engine.factory(secrets)))) };
    let ws_url = {
        let browser = handler.browser.lock().unwrap();
        format!("ws://{local}/devtools/browser/{}", browser.browser_id)
    };

    println!("wasm-headless-browser ({})", engine.label());
    println!("  discovery : http://{local}/json/version");
    println!("  websocket : {ws_url}");
    for secret in &held {
        println!("  secret    : {secret}, type it as {{{{secret:{}}}}}", secret.split(' ').next().unwrap_or(""));
    }
    println!();
    println!("  agent-browser --cdp http://{local} open https://example.com");
    println!("  agent-browser --cdp http://{local} snapshot");
    // Flush explicitly: when stdout is a pipe rather than a terminal it may be
    // block-buffered, and a caller waiting on this banner would hang.
    io::stdout().flush()?;

    serve(listener, handler)
}
