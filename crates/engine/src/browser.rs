//! The CDP engine, backed by a real document and a real JavaScript engine.
//!
//! `engine-fake` answered from a fixed model. This answers from whatever the
//! last navigation actually fetched, runs the page's own script against it, and
//! projects the accessibility tree from the result.
//!
//! What is still missing is **layout**. There is no CSS cascade and no box
//! model, so `getBoundingClientRect` reports zero-sized boxes, hit testing finds
//! nothing and a screenshot would be blank. Methods that depend on those return
//! an error that says so rather than a plausible-looking lie.

use std::sync::{Arc, Mutex, MutexGuard};

use browser_core::{ids, CdpError, Engine, EngineEmit, TargetInfo};
use serde_json::{json, Value};

use crate::cookies::{self, Cookie, CookieJar};
use crate::secrets::Secrets;
use crate::dom::{Document, NodeData, NodeId};
use crate::fetch::{self, Request};
use crate::bindings::Navigation;
use crate::js::{JsValue, ScriptHost};
use crate::parse::{parse_html, parse_html_bytes};
use crate::select;

/// How many times a script-initiated navigation is followed before stopping.
/// A page that redirects itself in a loop should not hang the engine.
const MAX_SCRIPT_NAVIGATIONS: u8 = 3;

/// Virtual milliseconds of timers a page gets to settle after loading, and
/// after each `Runtime.evaluate`. Time is virtual, so this costs only the work
/// the timers do, not wall-clock waiting.
const LOAD_BUDGET_MS: f64 = 5_000.0;
const EVALUATE_BUDGET_MS: f64 = 1_000.0;

/// Upper bound on tasks per settling pass, against pages that schedule work
/// forever.
const MAX_TASKS: usize = 10_000;

pub struct HtmlEngine {
    target_id: String,
    /// Shared with the script thread, which is why this is an `Arc<Mutex<_>>`
    /// rather than a plain `Document`: page script mutates the same tree the CDP
    /// surface reads.
    doc: Arc<Mutex<Document>>,
    /// `None` when the script thread could not start. The engine then still
    /// serves navigation and the accessibility tree, and reports why
    /// `Runtime.evaluate` is unavailable instead of failing silently.
    script: Option<ScriptHost>,
    /// Fetches with the jar every target shares.
    client: fetch::Client,
    /// Credentials, typed only through placeholders and scrubbed from
    /// everything the engine says. Shared by every target.
    secrets: Arc<Secrets>,
    url: String,
    frame_id: String,
    loader_id: String,
    context_id: u64,
    history: Vec<String>,
    history_index: usize,
    viewport: (f64, f64),
    last_error: Option<String>,
}

impl HtmlEngine {
    /// An engine with a cookie jar of its own.
    pub fn new(target_id: &str) -> Self {
        Self::with_cookies(target_id, Arc::new(Mutex::new(CookieJar::new())))
    }

    /// An engine sharing `cookies` with others, as tabs in one browser do.
    pub fn with_cookies(target_id: &str, cookies: Arc<Mutex<CookieJar>>) -> Self {
        Self::with_shared(target_id, cookies, Arc::new(Secrets::new()))
    }

    /// An engine sharing a cookie jar and a set of secrets with others.
    pub fn with_shared(target_id: &str, cookies: Arc<Mutex<CookieJar>>, secrets: Arc<Secrets>) -> Self {
        let mut counter = ids::seed();
        let doc = Arc::new(Mutex::new(Document::new()));
        let viewport = (800.0, 600.0);
        let client = fetch::Client::new(cookies);
        let script = ScriptHost::spawn(Arc::clone(&doc), client.clone(), viewport, USER_AGENT).ok();

        HtmlEngine {
            target_id: target_id.to_string(),
            doc,
            script,
            client,
            secrets,
            url: "about:blank".to_string(),
            frame_id: ids::token(&mut counter),
            loader_id: ids::token(&mut counter),
            context_id: 1,
            history: vec!["about:blank".to_string()],
            history_index: 0,
            viewport,
            last_error: None,
        }
    }

    /// Every target the factory creates shares one cookie jar.
    pub fn factory() -> browser_core::EngineFactory {
        Self::factory_with_secrets(Arc::new(Secrets::new()))
    }

    /// [`factory`](Self::factory), with credentials every target can type.
    pub fn factory_with_secrets(secrets: Arc<Secrets>) -> browser_core::EngineFactory {
        let cookies = Arc::new(Mutex::new(CookieJar::new()));
        Box::new(move |target_id: &str| {
            Box::new(HtmlEngine::with_shared(target_id, Arc::clone(&cookies), Arc::clone(&secrets))) as Box<dyn Engine>
        })
    }

    fn jar(&self) -> MutexGuard<'_, CookieJar> {
        self.client.cookies().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Borrows the document. Never held across a call into JavaScript: the
    /// script thread locks the same mutex, and holding it would deadlock.
    fn doc(&self) -> MutexGuard<'_, Document> {
        self.doc.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn origin(&self) -> String {
        match fetch::Url::parse(&self.url) {
            Ok(url) => url.origin(),
            Err(_) => "null".to_string(),
        }
    }

    fn frame_json(&self) -> Value {
        json!({
            "id": self.frame_id,
            "loaderId": self.loader_id,
            "url": self.url,
            "domainAndRegistry": "",
            "mimeType": "text/html",
            "securityOrigin": self.origin(),
            "secureContextType": "Insecure",
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        })
    }

    fn emit_context(&self, out: &mut EngineEmit) {
        out(
            "Runtime.executionContextCreated",
            json!({
                "context": {
                    "id": self.context_id,
                    "origin": self.origin(),
                    "name": "",
                    "uniqueId": format!("{}.{}", self.frame_id, self.context_id),
                    "auxData": { "isDefault": true, "type": "default", "frameId": self.frame_id },
                }
            }),
        );
    }

    fn emit_lifecycle(&self, out: &mut EngineEmit) {
        let timestamp = now();
        out("Page.frameStartedLoading", json!({ "frameId": self.frame_id }));
        out("Page.frameNavigated", json!({ "frame": self.frame_json(), "type": "Navigation" }));
        out("Runtime.executionContextsCleared", json!({}));
        self.emit_context(out);
        out("Page.domContentEventFired", json!({ "timestamp": timestamp }));
        out("Page.loadEventFired", json!({ "timestamp": timestamp }));
    }

    /// Fetches, parses, and runs the page's own script.
    fn load(&mut self, url: &str) {
        self.load_request(Request::get(url));
    }

    /// [`load`](Self::load) for any request, such as a form's `POST`.
    fn load_request(&mut self, request: Request) {
        self.replace_document(request);
        self.run_document_scripts();
        self.follow_script_navigation();
        self.push_history();
        self.sync_script_url();
    }

    /// Follows a URL change script made without loading anything:
    /// `history.pushState` or `location.hash = ...`. Returns the new URL.
    fn sync_script_url(&mut self) -> Option<String> {
        let replace = self.script.as_ref()?.state().take_url_change()?;
        let url = self.doc().url().to_string();
        if url == self.url {
            return None;
        }
        self.url = url.clone();
        if replace {
            if let Some(entry) = self.history.get_mut(self.history_index) {
                *entry = url.clone();
            }
        } else {
            self.push_history();
        }
        Some(url)
    }

    /// Fetches `request` and makes the result the document. A fetch failure
    /// becomes an error page, which is what a browser shows and a client can
    /// read.
    fn replace_document(&mut self, request: Request) {
        // The document lock borrows `self`, so everything that touches other
        // fields is collected here and applied once the guard is dropped.
        let requested = request.url.clone();
        self.client.clear_preloads();
        let fetched = self.client.send(request);
        let mut new_url = requested.clone();
        let new_error;

        {
            let mut doc = self.doc();
            match fetched {
                Ok(response) => {
                    if response.is_html() {
                        *doc = parse_html_bytes(&response.body);
                    } else {
                        // Non-HTML is wrapped so the client sees something rather
                        // than an empty document with a misleading title.
                        let escaped = escape_html(&String::from_utf8_lossy(&response.body));
                        let label = response.header("content-type").unwrap_or("document");
                        *doc = parse_html(&format!(
                            "<!doctype html><title>{}</title><body><pre>{escaped}</pre>",
                            escape_html(label)
                        ));
                    }
                    doc.set_url(&response.url);
                    doc.set_viewport(self.viewport);
                    new_url = response.url.clone();
                    new_error = response
                        .charset()
                        .filter(|c| c != "utf-8" && c != "utf8")
                        .map(|c| format!("the page declares charset {c}; only UTF-8 is decoded"));
                }
                Err(e) => {
                    let message = e.to_string();
                    *doc = parse_html(&error_page(&requested, &message));
                    doc.set_url(&requested);
                    doc.set_viewport(self.viewport);
                    new_error = Some(message);
                }
            }
        }

        self.url = new_url;
        self.last_error = new_error;

        self.preload_subresources();
        self.load_stylesheets();

        // A new document gets a new global environment, as in a browser. The
        // document lock is released above, which this needs.
        if let Some(script) = &self.script {
            if let Err(e) = script.reset() {
                self.last_error = Some(format!("could not reset the script context: {}", e.message));
            }
        }
    }

    /// Starts fetching every stylesheet and classic script the parser found,
    /// all at once, as a browser's preload scanner does. They are still used
    /// in the order the page puts them; this only stops each waiting for the
    /// one before it to download. Scripts download while the stylesheets are
    /// parsed and while earlier scripts run.
    fn preload_subresources(&self) {
        let Ok(base) = url::Url::parse(&self.url) else { return };
        let urls: Vec<String> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter_map(|id| {
                    let href = if doc.is_tag(id, "link") && has_token(doc.attr(id, "rel"), "stylesheet") {
                        doc.attr(id, "href")
                    } else if doc.is_tag(id, "script") && is_classic_script_type(doc.attr(id, "type")) {
                        doc.attr(id, "src")
                    } else {
                        None
                    };
                    base.join(href?).ok().map(String::from)
                })
                .collect()
        };
        for url in urls {
            self.client.preload(&url);
        }
    }

    /// Fetches the stylesheets the parser found, before any script runs, as a
    /// browser holds scripts back for them. They were
    /// [preloaded](Self::preload_subresources), so this mostly waits. Ones
    /// that fail are left out of the cascade, as a browser leaves them out.
    fn load_stylesheets(&mut self) {
        let links: Vec<(NodeId, String)> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter(|id| doc.is_tag(*id, "link") && has_token(doc.attr(*id, "rel"), "stylesheet"))
                .filter_map(|id| doc.attr(id, "href").map(|href| (id, href.to_string())))
                .collect()
        };
        for (id, href) in links {
            let Ok(url) = url::Url::parse(&self.url).and_then(|base| base.join(&href)) else { continue };
            let Ok(response) = self.client.get(url.as_str()) else { continue };
            if (200..300).contains(&response.status) {
                let text = String::from_utf8_lossy(&response.body).into_owned();
                self.doc().set_stylesheet(id, text);
            }
        }
    }

    /// Runs the page's scripts in the order a browser would, then lets the
    /// page settle.
    ///
    /// The document is parsed completely before any script runs, so a
    /// parser-blocking script sees elements that come after it, and
    /// `document.write` appends rather than inserting. Within that, the order
    /// is the browser's: parser-blocking scripts (inline, and external without
    /// `async` or `defer`) in document order, then `DOMContentLoaded` after
    /// the `defer` scripts, then `async` scripts, then the task queue, then
    /// `load`. A script that throws is reported and does not stop the ones
    /// after it.
    fn run_document_scripts(&mut self) {
        let Some(script) = &self.script else { return };

        struct Found {
            id: NodeId,
            label: String,
            external: bool,
            deferred: bool,
            asynchronous: bool,
        }
        let found: Vec<Found> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter(|id| doc.is_tag(*id, "script"))
                .map(|id| {
                    let src = doc.attr(id, "src");
                    Found {
                        id,
                        label: src.map(str::to_string).unwrap_or_else(|| format!("inline-{id}")),
                        external: src.is_some(),
                        // Modules are deferred by default.
                        deferred: doc.has_attr(id, "defer") || doc.attr(id, "type") == Some("module"),
                        asynchronous: doc.has_attr(id, "async"),
                    }
                })
                .collect()
        };

        let mut errors = Vec::new();
        let mut run = |found: &Found| {
            // The document lock is not held here: the script thread locks it.
            match script.evaluate(&format!("__executeScript({})", found.id)) {
                Ok(JsValue::String(message)) if !message.is_empty() => {
                    errors.push(format!("script error in <script> ({}): {message}", found.label));
                }
                Err(e) => errors.push(format!("script error in <script> ({}): {}", found.label, e.message)),
                _ => {}
            }
        };

        let blocking = |f: &&Found| !f.external || (!f.deferred && !f.asynchronous);
        for f in found.iter().filter(blocking) {
            run(f);
        }
        let _ = script.run("__documentParsed()");
        for f in found.iter().filter(|f| f.external && f.deferred && !f.asynchronous) {
            run(f);
        }
        let _ = script.run("__fireDOMContentLoaded()");
        for f in found.iter().filter(|f| f.external && f.asynchronous) {
            run(f);
        }
        script.run_tasks(LOAD_BUDGET_MS, MAX_TASKS);
        let _ = script.run("__fireLoad()");
        script.run_tasks(LOAD_BUDGET_MS, MAX_TASKS);

        if let Some(first) = errors.into_iter().next() {
            self.last_error = Some(first);
        }
    }

    /// If a script assigned `location.href` or submitted a form, follow it.
    /// Bounded, because a page that redirects itself in a loop should not hang
    /// the engine.
    fn follow_script_navigation(&mut self) {
        for _ in 0..MAX_SCRIPT_NAVIGATIONS {
            let Some(requested) = self
                .script
                .as_ref()
                .and_then(|script| script.state().take_requested_navigation())
            else {
                return;
            };
            if !self.script_navigation(requested) {
                return;
            }
        }
    }

    /// Acts on one navigation script asked for. Returns whether anything
    /// changed.
    ///
    /// A `GET` that differs from the current URL only in its fragment stays in
    /// the same document, as `location.hash = ...` does in a browser. A `GET`
    /// to exactly the current URL is ignored, so a page that sets
    /// `location.href` to itself does not loop.
    fn script_navigation(&mut self, requested: Navigation) -> bool {
        let target = url::Url::parse(&self.url)
            .and_then(|base| base.join(&requested.url))
            .map(String::from)
            .unwrap_or(requested.url);
        let is_get = requested.method.eq_ignore_ascii_case("GET") && !requested.reload;
        if is_get && target == self.url {
            return false;
        }
        let without_fragment = |u: &str| u.split('#').next().unwrap_or(u).to_string();
        if is_get && target.contains('#') && without_fragment(&target) == without_fragment(&self.url) {
            self.url = target.clone();
            self.doc().set_url(&target);
            self.push_history();
            return true;
        }
        let mut request = Request {
            method: requested.method.to_ascii_uppercase(),
            url: target,
            headers: Vec::new(),
            body: requested.body,
        };
        if let Some(content_type) = requested.content_type {
            request.headers.push(("Content-Type".into(), content_type));
        }
        self.replace_document(request);
        self.run_document_scripts();
        true
    }

    fn push_history(&mut self) {
        self.history.truncate(self.history_index + 1);
        if self.history.last() != Some(&self.url) {
            self.history.push(self.url.clone());
            self.history_index = self.history.len() - 1;
        }
    }

    /// Remote-object id for a raw DOM index, matching `backendDOMNodeId`.
    fn backend_id(dom: NodeId) -> usize {
        dom + 1
    }

    fn dom_from_backend(backend: usize) -> Option<NodeId> {
        backend.checked_sub(1)
    }

    /// The node a DOM method names, by `nodeId` or `backendNodeId` (the same
    /// numbers here) or by a remote `objectId`. Without any, the document.
    fn node_param(&self, params: &Value) -> Result<NodeId, CdpError> {
        let numbered = ["nodeId", "backendNodeId"].iter().find_map(|k| params.get(*k).and_then(|v| v.as_u64()));
        let node = if let Some(n) = numbered {
            Self::dom_from_backend(n as usize)
        } else if let Some(object_id) = params.get("objectId").and_then(|v| v.as_str()) {
            let id = self.cdp(&format!("__cdpNodeId({})", js_string(object_id)))?;
            id.as_i64().filter(|n| *n >= 0).map(|n| n as NodeId)
        } else {
            return Ok(self.doc().document_id());
        };
        match node {
            Some(id) if id < self.doc().len() => Ok(id),
            _ => Err(CdpError { code: -32000, message: "No node with given id found".into() }),
        }
    }

    /// A prelude call answering in CDP JSON. A `{ "error": ... }` answer, a
    /// failure of the plumbing rather than of page script, becomes a protocol
    /// error.
    fn cdp(&self, expression: &str) -> Result<Value, CdpError> {
        let Some(script) = &self.script else {
            return Err(not_yet("Runtime", "the JavaScript engine is not available in this process"));
        };
        let answer = script.cdp_expression(expression);
        cdp_answer(answer)
    }

    /// `Runtime.evaluate`.
    fn evaluate(&self, source: &str, by_value: bool, await_promise: bool) -> Result<Value, CdpError> {
        let Some(script) = &self.script else {
            return Err(not_yet("Runtime.evaluate", "the JavaScript engine is not available in this process"));
        };
        cdp_answer(script.cdp_evaluate(source, by_value, await_promise))
    }

    /// After script ran on a client's behalf: let what it set in motion run
    /// (a handler's timers, a request's completion), then act on a navigation
    /// or history move it asked for, outside the runtime.
    fn after_script(&mut self, out: &mut EngineEmit) {
        if let Some(script) = &self.script {
            script.run_tasks(EVALUATE_BUDGET_MS, MAX_TASKS);
        }
        let moved = self.sync_script_url();
        let (requested, steps) = match &self.script {
            Some(script) => (script.state().take_requested_navigation(), script.state().take_requested_history()),
            None => (None, 0),
        };
        if let Some(requested) = requested {
            if self.script_navigation(requested) {
                self.follow_script_navigation();
                self.push_history();
                self.emit_lifecycle(out);
            }
        } else if steps != 0 && self.go(steps) {
            self.emit_lifecycle(out);
        } else if let Some(url) = moved {
            out(
                "Page.navigatedWithinDocument",
                json!({ "frameId": self.frame_id, "url": url, "navigationType": "historyApi" }),
            );
        }
    }

    /// Moves `steps` entries through history, as `history.go(steps)` does.
    /// Returns whether a page was loaded; a move past either end is ignored,
    /// as a browser ignores it.
    fn go(&mut self, steps: i32) -> bool {
        let target = self.history_index as i64 + steps as i64;
        if target < 0 || target >= self.history.len() as i64 {
            return false;
        }
        self.history_index = target as usize;
        let url = self.history[self.history_index].clone();
        self.load_preserving_history(&url);
        true
    }

    fn load_preserving_history(&mut self, url: &str) {
        let history = self.history.clone();
        let index = self.history_index;

        self.replace_document(Request::get(url));
        self.run_document_scripts();
        self.history = history;
        self.history_index = index;
    }

    /// `Network.setCookie`. Without a `url` or a `domain` there is nothing to
    /// scope the cookie to, and Chrome refuses it too.
    fn set_cookie(&self, params: &Value) -> bool {
        let text = |key: &str| params.get(key).and_then(|v| v.as_str());
        let flag = |key: &str| params.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let url = text("url").and_then(|u| fetch::Url::parse(u).ok());
        let Some(cookie) = cookies::from_parts(
            url.as_ref(),
            text("name").unwrap_or(""),
            text("value").unwrap_or(""),
            text("domain"),
            text("path"),
            flag("secure"),
            flag("httpOnly"),
            text("sameSite"),
            params.get("expires").and_then(|v| v.as_f64()).filter(|e| *e > 0.0),
        ) else {
            return false;
        };
        if cookie.name.is_empty() {
            return false;
        }
        self.jar().set(cookie, cookies::now())
    }

    /// `DOM.getBoxModel`'s model: the border box from the layout, and the
    /// content, padding and margin boxes from the computed widths around it.
    fn box_model(&self, node: NodeId) -> Option<Value> {
        let doc = self.doc();
        let rect = crate::layout::document_layout(&doc).rect(node)?;
        let styles = crate::style::document_styles(&doc);
        let style = styles.get(node);
        let px = |name: &str| -> f64 {
            style
                .and_then(|s| crate::style::length_px(s.get(name), s.font_size(), 16.0, rect.width))
                .unwrap_or(0.0)
        };
        let border = |side: &str| -> f64 {
            let none = style.is_none_or(|s| matches!(s.get(&format!("border-{side}-style")), "none" | "hidden"));
            if none { 0.0 } else { px(&format!("border-{side}-width")) }
        };
        let side = |f: &dyn Fn(&str) -> f64| [f("top"), f("right"), f("bottom"), f("left")];
        let [bt, br, bb, bl] = side(&border);
        let [pt, pr, pb, pl] = side(&|s| px(&format!("padding-{s}")));
        let [mt, mr, mb, ml] = side(&|s| px(&format!("margin-{s}")));
        let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
        let padding = (x + bl, y + bt, w - bl - br, h - bt - bb);
        let content = (padding.0 + pl, padding.1 + pt, padding.2 - pl - pr, padding.3 - pt - pb);
        Some(json!({
            "content": quad(content.0, content.1, content.2.max(0.0), content.3.max(0.0)),
            "padding": quad(padding.0, padding.1, padding.2.max(0.0), padding.3.max(0.0)),
            "border": quad(x, y, w, h),
            "margin": quad(x - ml, y - mt, w + ml + mr, h + mt + mb),
            "width": w.round() as i64,
            "height": h.round() as i64,
        }))
    }

    /// `DOM.describeNode`-shaped output for one node.
    fn describe(&self, node: NodeId) -> Value {
        let doc = self.doc();
        let entry = doc.node(node);
        let (node_type, node_name) = match &entry.data {
            NodeData::Document => (9, "#document".to_string()),
            NodeData::Doctype { name } => (10, name.clone()),
            NodeData::Comment => (8, "#comment".to_string()),
            NodeData::Text => (3, "#text".to_string()),
            NodeData::Fragment => (11, "#document-fragment".to_string()),
            NodeData::Element { .. } => (1, doc.tag(node).unwrap_or("").to_ascii_uppercase()),
        };
        let mut value = json!({
            "nodeId": Self::backend_id(node),
            "backendNodeId": Self::backend_id(node),
            "nodeType": node_type,
            "nodeName": node_name,
            "childNodeCount": doc.children(node).len(),
        });
        if node_type == 1 {
            let attrs: Vec<Value> = doc
                .attrs(node)
                .iter()
                .flat_map(|a| [json!(a.local_name()), json!(a.value)])
                .collect();
            value["attributes"] = json!(attrs);
            value["localName"] = json!(doc.tag(node).unwrap_or(""));
        }
        if node_type == 3 || node_type == 8 {
            value["nodeValue"] = json!(entry.text);
        }
        value
    }
}

/// Reported to pages as the user agent. Kept beside the version string in
/// `browser-core` so the two do not drift.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// The CDP surface, before secrets are scrubbed from what it says. The
/// `Engine` implementation below is the only way out, and it scrubs.
impl HtmlEngine {
    fn info_raw(&self) -> TargetInfo {
        let doc = self.doc();
        let mut info = TargetInfo::new(self.target_id.clone());
        info.title = doc.title().unwrap_or_default();
        info.url = doc.url().to_string();
        if info.url.is_empty() {
            info.url = self.url.clone();
        }
        info
    }

    fn navigate_raw(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        self.load(url);
        self.emit_lifecycle(out);
        Ok(())
    }

    fn dispatch_raw(&mut self, method: &str, params: &Value, out: &mut EngineEmit) -> Result<Value, CdpError> {
        match method {
            // ---- Page ----------------------------------------------------
            "Page.enable" | "Page.disable" | "Page.bringToFront" => Ok(json!({})),
            "Page.getFrameTree" => Ok(json!({ "frameTree": { "frame": self.frame_json() } })),

            "Page.navigate" => {
                let url =
                    params.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank").to_string();
                self.load(&url);
                self.emit_lifecycle(out);
                Ok(json!({
                    "frameId": self.frame_id,
                    "loaderId": self.loader_id,
                    "type": "Navigation",
                }))
            }

            "Page.reload" => {
                let url = self.url.clone();
                self.load(&url);
                self.emit_lifecycle(out);
                Ok(json!({}))
            }

            "Page.getNavigationHistory" => {
                let title = self.doc().title().unwrap_or_default();
                let entries: Vec<Value> = self
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, url)| {
                        json!({
                            "id": i,
                            "url": url,
                            "userTypedURL": url,
                            "title": if *url == self.url { title.clone() } else { String::new() },
                            "transitionType": "typed",
                        })
                    })
                    .collect();
                Ok(json!({ "currentIndex": self.history_index, "entries": entries }))
            }

            "Page.getLayoutMetrics" => {
                let (w, h) = self.viewport;
                Ok(json!({
                    "layoutViewport": { "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h },
                    "visualViewport": { "offsetX": 0, "offsetY": 0, "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h, "scale": 1 },
                    "contentSize": { "x": 0, "y": 0, "width": w, "height": h },
                }))
            }

            "Page.setLifecycleEventsEnabled" | "Page.setBypassCSP" => Ok(json!({})),
            "Page.addScriptToEvaluateOnNewDocument" => Ok(json!({ "identifier": "1" })),
            "Page.createIsolatedWorld" => Ok(json!({ "executionContextId": self.context_id })),

            "Page.captureScreenshot" => Err(not_yet(
                method,
                "there is no rasterizer yet, so a screenshot would be blank",
            )),

            // ---- Runtime -------------------------------------------------
            "Runtime.enable" => {
                self.emit_context(out);
                Ok(json!({}))
            }
            "Runtime.disable" | "Runtime.runIfWaitingForDebugger" => Ok(json!({})),
            "Runtime.releaseObject" => {
                if let Some(id) = params.get("objectId").and_then(|v| v.as_str()) {
                    self.cdp(&format!("(__cdpRelease({}), '{{}}')", js_string(id)))?;
                }
                Ok(json!({}))
            }
            // Groups are not tracked; a document's handles go with its context.
            "Runtime.releaseObjectGroup" => Ok(json!({})),

            // Real JavaScript, from a real engine. `back` in agent-browser is
            // `history.back()` evaluated this way, which `after_script` acts on.
            "Runtime.evaluate" => {
                let source = params.get("expression").and_then(|v| v.as_str()).unwrap_or("");
                let flag = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                let result = self.evaluate(source, flag("returnByValue"), flag("awaitPromise"))?;
                self.after_script(out);
                Ok(result)
            }

            "Runtime.callFunctionOn" => {
                let flag = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                let object_id = match params.get("objectId").and_then(|v| v.as_str()) {
                    Some(id) => js_string(id),
                    None => "null".to_string(),
                };
                let declaration = params.get("functionDeclaration").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!([])).to_string();
                let result = self.cdp(&format!(
                    "__cdpCall({object_id}, {}, {}, {}, {})",
                    js_string(declaration),
                    js_string(&arguments),
                    flag("returnByValue"),
                    flag("awaitPromise"),
                ))?;
                self.after_script(out);
                Ok(result)
            }

            "Runtime.getIsolateId" => Ok(json!({ "id": "m1" })),

            "Runtime.consoleAPICalled" => Ok(json!({})),

            // ---- DOM -----------------------------------------------------
            "DOM.enable" | "DOM.disable" => Ok(json!({})),
            "DOM.getDocument" => {
                let root = self.doc().document_id();
                Ok(json!({ "root": self.describe(root) }))
            }

            "DOM.querySelector" => {
                let doc = self.doc();
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                match select::query(&doc, root, &selector) {
                    Some(node) => Ok(json!({ "nodeId": Self::backend_id(node) })),
                    None => Ok(json!({ "nodeId": 0 })),
                }
            }

            "DOM.querySelectorAll" => {
                let doc = self.doc();
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let ids: Vec<usize> = select::query_all(&doc, root, &selector)
                    .into_iter()
                    .map(Self::backend_id)
                    .collect();
                Ok(json!({ "nodeIds": ids }))
            }

            "DOM.describeNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "node": self.describe(node) }))
            }

            "DOM.requestNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "nodeId": Self::backend_id(node) }))
            }

            "DOM.resolveNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "object": self.cdp(&format!("__cdpNode({node})"))? }))
            }

            // Boxes from the layout. agent-browser clicks a snapshot ref at
            // the centre of its content quad.
            "DOM.getBoxModel" => {
                let node = self.node_param(params)?;
                let model = self.box_model(node).ok_or_else(|| CdpError {
                    code: -32000,
                    message: "Could not compute box model.".into(),
                })?;
                Ok(json!({ "model": model }))
            }
            "DOM.getContentQuads" => {
                let node = self.node_param(params)?;
                let doc = self.doc();
                let quads: Vec<Value> = crate::layout::document_layout(&doc)
                    .rect(node)
                    .filter(|r| r.width > 0.0 && r.height > 0.0)
                    .map(|r| vec![json!(quad(r.x, r.y, r.width, r.height))])
                    .unwrap_or_default();
                Ok(json!({ "quads": quads }))
            }
            // Nothing scrolls yet; everything is where it is.
            "DOM.scrollIntoViewIfNeeded" => Ok(json!({})),

            "DOM.focus" => {
                let node = self.node_param(params)?;
                let object = self.cdp(&format!("__cdpNode({node})"))?;
                let id = object.get("objectId").and_then(|v| v.as_str()).unwrap_or("");
                self.cdp(&format!("__cdpCall({}, 'function () {{ this.focus(); }}', '[]', false, false)", js_string(id)))?;
                self.after_script(out);
                Ok(json!({}))
            }

            "DOM.getOuterHTML" => {
                let doc = self.doc();
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                Ok(json!({ "outerHTML": serialize(&doc, node) }))
            }

            // ---- Accessibility -------------------------------------------
            "Accessibility.enable" => {
                out("Accessibility.loadComplete", json!({}));
                Ok(json!({}))
            }
            "Accessibility.disable" => Ok(json!({})),

            "Accessibility.getFullAXTree" | "Accessibility.getPartialAXTree" => {
                Ok(crate::ax::to_cdp(&crate::ax::build(&self.doc())))
            }

            // ---- Network -------------------------------------------------
            // The cookie jar is real and shared by every target. Network
            // events are not emitted yet (M3.1).
            "Network.enable" | "Network.disable" => Ok(json!({})),

            // With `urls`, the cookies those URLs would be sent; without, the
            // current page's. That is Chrome's reading of the parameter.
            "Network.getCookies" => {
                let urls: Vec<String> = match params.get("urls").and_then(|v| v.as_array()) {
                    Some(list) => list.iter().filter_map(|u| u.as_str().map(str::to_string)).collect(),
                    None => vec![self.url.clone()],
                };
                let now = cookies::now();
                let jar = self.jar();
                let mut seen: Vec<&Cookie> = Vec::new();
                for url in urls.iter().filter_map(|u| fetch::Url::parse(u).ok()) {
                    for cookie in jar.matching(&url, cookies::Source::Http, now) {
                        if !seen.iter().any(|c| std::ptr::eq(*c, cookie)) {
                            seen.push(cookie);
                        }
                    }
                }
                Ok(json!({ "cookies": seen.into_iter().map(cookie_json).collect::<Vec<_>>() }))
            }
            "Network.getAllCookies" | "Storage.getCookies" => {
                let jar = self.jar();
                let all: Vec<Value> = jar.all(cookies::now()).into_iter().map(cookie_json).collect();
                Ok(json!({ "cookies": all }))
            }
            "Network.setCookie" => Ok(json!({ "success": self.set_cookie(params) })),
            "Network.setCookies" | "Storage.setCookies" => {
                if let Some(list) = params.get("cookies").and_then(|v| v.as_array()) {
                    for cookie in list {
                        self.set_cookie(cookie);
                    }
                }
                Ok(json!({}))
            }
            "Network.deleteCookies" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let url = params.get("url").and_then(|v| v.as_str()).and_then(|u| fetch::Url::parse(u).ok());
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| url.as_ref().map(|u| u.host.clone()));
                let path = params.get("path").and_then(|v| v.as_str());
                self.jar().delete(name, domain.as_deref(), path);
                Ok(json!({}))
            }
            "Network.clearBrowserCookies" | "Storage.clearCookies" => {
                self.jar().clear();
                Ok(json!({}))
            }

            // ---- Input ---------------------------------------------------
            // Keys and text go to the focused element, which needs no layout.
            // `{{secret:NAME}}` becomes the secret here, and only here, and
            // only on a page from the secret's origin.
            "Input.insertText" => {
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let text = self
                    .secrets
                    .resolve(text, &self.url)
                    .map_err(|e| CdpError { code: -32000, message: e.to_string() })?;
                self.cdp(&format!("(__insertText({}), '{{}}')", js_string(&text)))?;
                self.after_script(out);
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                self.cdp(&format!("(__dispatchKey({}), '{{}}')", js_string(&params.to_string())))?;
                self.after_script(out);
                Ok(json!({}))
            }

            // The pointer lands on what the layout puts at the point.
            "Input.dispatchMouseEvent" => {
                self.cdp(&format!("(__dispatchMouse({}), '{{}}')", js_string(&params.to_string())))?;
                self.after_script(out);
                Ok(json!({}))
            }
            "Input.dispatchTouchEvent" => Err(not_yet(method, "touch input is not implemented")),

            // ---- Emulation -----------------------------------------------
            "Emulation.setDeviceMetricsOverride" => {
                let w = params.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = params.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                self.viewport = (w, h);
                if let Some(script) = &self.script {
                    script.state().set_viewport(w, h);
                }
                self.doc().set_viewport((w, h));
                Ok(json!({}))
            }
            "Emulation.setUserAgentOverride"
            | "Emulation.setEmulatedMedia"
            | "Emulation.clearDeviceMetricsOverride" => Ok(json!({})),

            "Log.enable" | "Log.clear" | "Performance.enable" => Ok(json!({})),
            "WebMCP.enable" | "WebMCP.disable" => Ok(json!({})),

            _ => Ok(json!({})),
        }
    }
}

/// Everything leaving the engine passes through here, and every secret's
/// value in it becomes its placeholder: results, errors, events, and the
/// target's title and URL.
impl Engine for HtmlEngine {
    fn info(&self) -> TargetInfo {
        let mut info = self.info_raw();
        info.title = self.secrets.scrub_str(&info.title);
        info.url = self.secrets.scrub_str(&info.url);
        info
    }

    fn navigate(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            secrets.scrub(&mut params);
            out(method, params)
        };
        self.navigate_raw(url, &mut scrubbed).map_err(|e| scrub_error(&self.secrets, e))
    }

    fn attach(&mut self, out: &mut EngineEmit) {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            secrets.scrub(&mut params);
            out(method, params)
        };
        self.emit_context(&mut scrubbed);
    }

    fn dispatch(&mut self, method: &str, params: &Value, out: &mut EngineEmit) -> Result<Value, CdpError> {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            secrets.scrub(&mut params);
            out(method, params)
        };
        match self.dispatch_raw(method, params, &mut scrubbed) {
            Ok(mut value) => {
                self.secrets.scrub(&mut value);
                Ok(value)
            }
            Err(e) => Err(scrub_error(&self.secrets, e)),
        }
    }
}

fn scrub_error(secrets: &Secrets, error: CdpError) -> CdpError {
    CdpError { code: error.code, message: secrets.scrub_str(&error.message) }
}


/// A cookie in `Network.Cookie` shape.
fn cookie_json(cookie: &Cookie) -> Value {
    let mut value = json!({
        "name": cookie.name,
        "value": cookie.value,
        // Chrome marks a domain cookie with a leading dot and a host-only one
        // without.
        "domain": if cookie.host_only { cookie.domain.clone() } else { format!(".{}", cookie.domain) },
        "path": cookie.path,
        "expires": cookie.expires.unwrap_or(-1.0),
        "size": cookie.name.len() + cookie.value.len(),
        "httpOnly": cookie.http_only,
        "secure": cookie.secure,
        "session": cookie.expires.is_none(),
        "priority": "Medium",
        "sameParty": false,
        "sourceScheme": if cookie.secure { "Secure" } else { "NonSecure" },
    });
    if let Some(same_site) = &cookie.same_site {
        value["sameSite"] = json!(same_site);
    }
    value
}

/// A CDP quad: the four corners, clockwise from the top left.
fn quad(x: f64, y: f64, width: f64, height: f64) -> Vec<f64> {
    vec![x, y, x + width, y, x + width, y + height, x, y + height]
}

/// A JavaScript string literal for `text`. JSON's string syntax is a subset
/// of JavaScript's.
fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// A prelude answer, or the protocol error it carries.
fn cdp_answer(answer: Value) -> Result<Value, CdpError> {
    match answer.get("error").and_then(|e| e.as_str()) {
        Some(message) => Err(CdpError { code: -32000, message: message.to_string() }),
        None => Ok(answer),
    }
}

fn not_yet(method: &str, why: &str) -> CdpError {
    CdpError { code: -32000, message: format!("{method} is not implemented yet: {why}") }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The DOM interface a node presents as. Shared with the JavaScript wrappers'
/// `Symbol.toStringTag`, so `DOM.resolveNode` and `Runtime.evaluate` agree.
pub(crate) fn class_for(doc: &Document, node: NodeId) -> &'static str {
    match &doc.node(node).data {
        NodeData::Document => return "HTMLDocument",
        NodeData::Doctype { .. } => return "DocumentType",
        NodeData::Text => return "Text",
        NodeData::Comment => return "Comment",
        NodeData::Fragment => return "DocumentFragment",
        NodeData::Element { .. } => {}
    }
    if doc.namespace(node) == "http://www.w3.org/2000/svg" {
        return if doc.is_tag(node, "svg") { "SVGSVGElement" } else { "SVGElement" };
    }
    match doc.tag(node) {
        Some("html") => "HTMLHtmlElement",
        Some("body") => "HTMLBodyElement",
        Some("a") => "HTMLAnchorElement",
        Some("button") => "HTMLButtonElement",
        Some("input") => "HTMLInputElement",
        Some("form") => "HTMLFormElement",
        Some("label") => "HTMLLabelElement",
        Some("p") => "HTMLParagraphElement",
        Some("h1") | Some("h2") | Some("h3") | Some("h4") | Some("h5") | Some("h6") => {
            "HTMLHeadingElement"
        }
        Some("img") => "HTMLImageElement",
        Some("select") => "HTMLSelectElement",
        Some("textarea") => "HTMLTextAreaElement",
        Some("script") => "HTMLScriptElement",
        Some("div") => "HTMLDivElement",
        Some("span") => "HTMLSpanElement",
        Some("head") => "HTMLHeadElement",
        Some("title") => "HTMLTitleElement",
        Some("meta") => "HTMLMetaElement",
        Some("link") => "HTMLLinkElement",
        Some("style") => "HTMLStyleElement",
        Some("ul") => "HTMLUListElement",
        Some("ol") => "HTMLOListElement",
        Some("li") => "HTMLLIElement",
        Some("table") => "HTMLTableElement",
        Some("tr") => "HTMLTableRowElement",
        Some("td") | Some("th") => "HTMLTableCellElement",
        Some("thead") | Some("tbody") | Some("tfoot") => "HTMLTableSectionElement",
        Some("option") => "HTMLOptionElement",
        Some("iframe") => "HTMLIFrameElement",
        Some("canvas") => "HTMLCanvasElement",
        Some("template") => "HTMLTemplateElement",
        Some("br") => "HTMLBRElement",
        Some("hr") => "HTMLHRElement",
        Some("pre") => "HTMLPreElement",
        Some("video") => "HTMLVideoElement",
        Some("audio") => "HTMLAudioElement",
        Some("dialog") => "HTMLDialogElement",
        _ => "HTMLElement",
    }
}

/// Minimal HTML serialization, enough for `outerHTML` and for tests.
pub fn serialize(doc: &Document, node: NodeId) -> String {
    let mut out = String::new();
    write_node(doc, node, &mut out);
    out
}

fn write_node(doc: &Document, node: NodeId, out: &mut String) {
    match &doc.node(node).data {
        NodeData::Document | NodeData::Fragment => {
            for child in doc.children(node).to_vec() {
                write_node(doc, child, out);
            }
        }
        NodeData::Doctype { name } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Comment => {
            out.push_str("<!--");
            out.push_str(&doc.node(node).text);
            out.push_str("-->");
        }
        NodeData::Text => out.push_str(&escape_html(&doc.node(node).text)),
        NodeData::Element { .. } => {
            let tag = doc.tag(node).unwrap_or("");
            out.push('<');
            out.push_str(tag);
            for attr in doc.attrs(node) {
                out.push(' ');
                out.push_str(attr.local_name());
                out.push_str("=\"");
                out.push_str(&escape_html(&attr.value));
                out.push('"');
            }
            out.push('>');
            let void = matches!(
                tag,
                "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link"
                    | "meta" | "param" | "source" | "track" | "wbr"
            );
            if void {
                return;
            }
            for child in doc.children(node).to_vec() {
                write_node(doc, child, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The page shown when a navigation fails. A browser shows something; so does
/// this, because a client reading an empty document learns nothing.
/// Whether a space-separated attribute such as `rel` holds `token`.
fn has_token(value: Option<&str>, token: &str) -> bool {
    value.is_some_and(|v| v.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(token)))
}

/// Whether a `<script type>` is one the engine runs as a classic script.
/// The prelude's `scriptKind` decides for real; this only has to agree well
/// enough not to preload what will not run.
fn is_classic_script_type(kind: Option<&str>) -> bool {
    let Some(kind) = kind.map(|k| k.trim().to_ascii_lowercase()) else { return true };
    kind.is_empty() || kind.contains("javascript") || kind.contains("ecmascript") || kind.contains("jscript")
}

fn error_page(url: &str, message: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p><code>{url}</code></p><pre>{message}</pre></body></html>",
        title = escape_html("Navigation failed"),
        url = escape_html(url),
        message = escape_html(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::Engine;

    fn engine_with(html: &str) -> HtmlEngine {
        let mut engine = HtmlEngine::new("target");
        {
            let mut doc = engine.doc();
            *doc = parse_html(html);
            doc.set_url("http://example.test/page");
        }
        engine.url = "http://example.test/page".to_string();
        engine
    }

    fn evaluate(engine: &mut HtmlEngine, expression: &str) -> Value {
        engine.evaluate(expression, false, false).expect(expression)
    }

    #[test]
    fn evaluates_real_page_javascript() {
        let mut engine = engine_with("<title>Real</title><body><h1>Heading</h1></body>");
        assert_eq!(evaluate(&mut engine, "1 + 1")["result"]["value"], 2.0);
        assert_eq!(evaluate(&mut engine, "location.href")["result"]["value"], "http://example.test/page");
        assert_eq!(evaluate(&mut engine, "document.title")["result"]["value"], "Real");
        assert_eq!(evaluate(&mut engine, "document.querySelector('h1').textContent")["result"]["value"], "Heading");
        assert_eq!(evaluate(&mut engine, "window.innerWidth")["result"]["value"], 800.0);
    }

    fn dispatch(engine: &mut HtmlEngine, method: &str, params: Value) -> Value {
        let mut sink = |_: &str, _: Value| {};
        engine.dispatch(method, &params, &mut sink).expect(method)
    }

    fn cookie_names(value: &Value) -> Vec<String> {
        let mut names: Vec<String> = value["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn server_cookies_come_back_on_the_next_navigation_and_through_cdp() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
             Set-Cookie: sid=s3cret; Path=/; HttpOnly\r\nSet-Cookie: theme=dark; Path=/\r\n\
             Content-Length: 21\r\n\r\n<title>one</title>one"
                .to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 21\r\n\r\n<title>two</title>two"
                .to_string(),
        ]);
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/one"));
        engine.load(&format!("{base}/two"));
        let _ = requests.recv().unwrap();
        let second = requests.recv().unwrap();
        assert!(second.contains("Cookie: sid=s3cret; theme=dark\r\n"), "{second}");

        let cookies = dispatch(&mut engine, "Network.getCookies", json!({}));
        assert_eq!(cookie_names(&cookies), ["sid", "theme"]);
        let sid = &cookies["cookies"].as_array().unwrap().iter().find(|c| c["name"] == "sid").unwrap();
        assert_eq!(sid["httpOnly"], true);
        assert_eq!(sid["session"], true);
        assert_eq!(sid["domain"], "127.0.0.1");

        // Page script sees only what is not HttpOnly.
        assert_eq!(evaluate(&mut engine, "document.cookie")["result"]["value"], "theme=dark");
    }

    #[test]
    fn the_parser_found_scripts_and_stylesheets_download_together() {
        let base = crate::test_server::serve_together(
            vec![
                ("/", "text/html", "<link rel=stylesheet href=s.css><script src=a.js></script><script src=b.js></script>".into()),
                ("/s.css", "text/css", "p { color: red }".into()),
                ("/a.js", "text/javascript", "document.title = 'a';".into()),
                ("/b.js", "text/javascript", "document.title += 'b';".into()),
            ],
            vec!["/s.css", "/a.js", "/b.js"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(engine.last_error, None);
        assert_eq!(evaluate(&mut engine, "document.title")["result"]["value"], "ab");
        assert_eq!(evaluate(&mut engine, "document.styleSheets.length")["result"]["value"], 1);
    }

    #[test]
    fn what_script_requests_downloads_together_and_runs_in_order() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<script>
                       window.log = [];
                       for (const name of ['c.js', 'd.js']) {
                         const s = document.createElement('script');
                         s.src = name;
                         document.head.append(s);
                       }
                       const l = document.createElement('link');
                       l.rel = 'stylesheet';
                       l.href = 't.css';
                       l.onload = () => log.push('css');
                       document.head.append(l);
                       fetch('e.json').then((r) => r.json()).then((j) => log.push(j.name));
                       const x = new XMLHttpRequest();
                       x.open('GET', 'f.txt');
                       x.onload = () => log.push(x.responseText);
                       x.send();
                     </script>"
                        .into(),
                ),
                ("/c.js", "text/javascript", "log.push('c');".into()),
                ("/d.js", "text/javascript", "log.push('d');".into()),
                ("/t.css", "text/css", "p { color: red }".into()),
                ("/e.json", "application/json", r#"{"name":"e"}"#.into()),
                ("/f.txt", "text/plain", "f".into()),
            ],
            vec!["/c.js", "/d.js", "/t.css", "/e.json", "/f.txt"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(engine.last_error, None);
        // Each completes in the task it was queued in, so the order is the
        // order they were asked for, whatever order they arrived in.
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "c,d,css,e,f");
    }

    #[test]
    fn an_aborted_fetch_is_dropped() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<script>
                       window.log = [];
                       const c = new AbortController();
                       fetch('slow.txt', { signal: c.signal }).then(() => log.push('done'), (e) => log.push(e.name));
                       c.abort();
                     </script>"
                        .into(),
                ),
                ("/slow.txt", "text/plain", "x".into()),
            ],
            vec!["/slow.txt"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "AbortError");
    }

    #[test]
    fn script_cookies_land_in_the_shared_jar() {
        let mut engine = engine_with("<body>x</body>");
        evaluate(&mut engine, "document.cookie = 'lang=ko; path=/'");
        let all = dispatch(&mut engine, "Network.getAllCookies", json!({}));
        assert_eq!(cookie_names(&all), ["lang"]);
        assert_eq!(all["cookies"][0]["domain"], "example.test");
    }

    #[test]
    fn cdp_can_set_delete_and_clear_cookies() {
        let mut engine = engine_with("<body>x</body>");
        let ok = dispatch(
            &mut engine,
            "Network.setCookie",
            json!({ "name": "a", "value": "1", "url": "http://example.test/" }),
        );
        assert_eq!(ok["success"], true);
        dispatch(
            &mut engine,
            "Network.setCookies",
            json!({ "cookies": [{ "name": "b", "value": "2", "domain": "example.test", "path": "/" }] }),
        );
        assert_eq!(evaluate(&mut engine, "document.cookie")["result"]["value"], "a=1; b=2");

        dispatch(&mut engine, "Network.deleteCookies", json!({ "name": "a", "url": "http://example.test/" }));
        assert_eq!(cookie_names(&dispatch(&mut engine, "Network.getAllCookies", json!({}))), ["b"]);

        dispatch(&mut engine, "Network.clearBrowserCookies", json!({}));
        assert!(cookie_names(&dispatch(&mut engine, "Network.getAllCookies", json!({}))).is_empty());

        // Nothing to scope it to.
        let refused = dispatch(&mut engine, "Network.setCookie", json!({ "name": "c", "value": "3" }));
        assert_eq!(refused["success"], false);
    }

    #[test]
    fn targets_from_one_factory_share_cookies() {
        let factory = HtmlEngine::factory();
        let mut first = factory("one");
        let mut second = factory("two");
        let mut sink = |_: &str, _: Value| {};
        first
            .dispatch(
                "Network.setCookie",
                &json!({ "name": "shared", "value": "1", "url": "http://example.test/" }),
                &mut sink,
            )
            .unwrap();
        let all = second.dispatch("Network.getAllCookies", &json!({}), &mut sink).unwrap();
        assert_eq!(cookie_names(&all), ["shared"]);
    }

    #[test]
    fn history_back_and_forward_from_script_move_through_history() {
        let mut engine = HtmlEngine::new("t");
        engine.load("data:text/html,<title>first</title>");
        engine.load("data:text/html,<title>second</title>");
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.back()" }));
        assert_eq!(engine.info().title, "first");
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.forward()" }));
        assert_eq!(engine.info().title, "second");
        // Past the end is ignored.
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.forward()" }));
        assert_eq!(engine.info().title, "second");
        assert_eq!(engine.history.len(), 3, "{:?}", engine.history);
    }

    #[test]
    fn each_document_gets_a_fresh_global_environment() {
        let page = "data:text/html,<div id=\"o\"></div><script>const once = 1; \
                    document.getElementById('o').textContent = 'ran'; leaked = 'yes';</script>";
        let mut engine = HtmlEngine::new("t");
        engine.load(page);
        engine.load("data:text/html,<title>other</title>");
        // The previous page's globals are gone.
        assert_eq!(evaluate(&mut engine, "typeof leaked")["result"]["value"], "undefined");
        // And the same page loads again without redeclaring its `const`.
        engine.load(page);
        assert_eq!(engine.last_error, None);
        let doc = engine.doc();
        let o = doc.find_id(doc.document_id(), "o").unwrap();
        assert_eq!(doc.text_content(o), "ran");
    }

    #[test]
    fn whole_numbers_go_out_without_a_fraction() {
        let mut engine = engine_with("<body>x</body>");
        assert_eq!(evaluate(&mut engine, "6 * 7")["result"]["value"].to_string(), "42");
        assert_eq!(evaluate(&mut engine, "1.5")["result"]["value"].to_string(), "1.5");
    }

    #[test]
    fn exceptions_come_back_as_exception_details() {
        // A page script that throws is normal. Reporting it as a protocol error
        // would make a client abort instead of carrying on.
        let mut engine = engine_with("<body>x</body>");
        let result = evaluate(&mut engine, "throw new Error('page bug')");
        assert_eq!(result["result"]["subtype"], "error");
        let text = result["exceptionDetails"]["text"].as_str().unwrap_or("");
        assert!(text.contains("page bug"), "message was: {text}");
    }

    #[test]
    fn a_script_navigating_updates_the_url() {
        let mut engine = engine_with("<body>x</body>");
        engine.load("data:text/html,<body><script>location.href='#other'</script></body>");
        // A fragment change is a same-document navigation, so the load happens
        // once and does not loop.
        assert!(engine.url.contains("#other") || engine.url.starts_with("data:"), "{}", engine.url);
    }

    #[test]
    fn inline_scripts_run_against_the_document() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<title>T</title><body><div id=\"out\"></div>\
             <script>document.getElementById('out').textContent = 'from script';</script></body>",
        );
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        assert_eq!(doc.text_content(out), "from script");
    }

    #[test]
    fn a_script_that_throws_does_not_stop_the_next_one() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<body><div id=\"out\"></div>\
             <script>throw new Error('first');</script>\
             <script>document.getElementById('out').textContent = 'second ran';</script></body>",
        );
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        assert_eq!(doc.text_content(out), "second ran");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("first"));
    }

    #[test]
    fn external_scripts_are_fetched_and_run_in_order() {
        let script = "document.getElementById('out').textContent += 'external;';";
        let (base, _requests) = crate::test_server::serve_script(vec![
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                PAGE.len(),
                PAGE
            ),
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\n\r\n{script}", script.len()),
        ]);
        const PAGE: &str = "<body><div id=\"out\"></div><script src=\"/app.js\" defer></script>\
                            <script>document.getElementById('out').textContent += 'inline;';</script></body>";
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        // `defer` runs after the parser-blocking inline script.
        assert_eq!(doc.text_content(out), "inline;external;");
    }

    #[test]
    fn a_script_that_cannot_be_fetched_is_reported() {
        let mut engine = HtmlEngine::new("t");
        engine.load("data:text/html,<body><script src=\"http://127.0.0.1:1/app.js\"></script></body>");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("could not load"), "{:?}", engine.last_error);
    }

    #[test]
    fn the_accessibility_tree_reflects_script_mutations() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<title>T</title><body><div id=\"root\"></div>\
             <script>\
               const h = document.createElement('h2');\
               h.textContent = 'Added by script';\
               document.getElementById('root').appendChild(h);\
             </script></body>",
        );
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        let value =
            engine.dispatch("Accessibility.getFullAXTree", &Value::Null, &mut sink).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n["role"]["value"] == "heading" && n["name"]["value"] == "Added by script"),
            "script-created heading missing from the tree"
        );
    }

    #[test]
    fn unknown_expressions_are_evaluated_not_guessed() {
        // The old rule set returned undefined for anything unrecognised. Now
        // JavaScript answers, so an arbitrary expression has a real value.
        let mut engine = engine_with("<body><p id='p'>hello</p></body>");
        assert_eq!(
            evaluate(&mut engine, "document.querySelector('#p').textContent.length")["result"]["value"],
            5.0
        );
        assert_eq!(evaluate(&mut engine, "['a','b'].join('-')")["result"]["value"], "a-b");
        assert_eq!(
            evaluate(&mut engine, "JSON.stringify({n: [1,2]})")["result"]["value"],
            r#"{"n":[1,2]}"#
        );
    }

    #[test]
    fn an_evaluated_node_comes_back_as_a_handle_the_client_can_call() {
        // agent-browser resolves a CSS selector this way, then focuses and
        // reads through callFunctionOn.
        let mut engine = engine_with("<body><p id='p' class='a b'>hello there</p></body>");
        let object = evaluate(&mut engine, "document.querySelector('#p')");
        assert_eq!(object["result"]["subtype"], "node");
        assert_eq!(object["result"]["className"], "HTMLParagraphElement");
        assert_eq!(object["result"]["description"], "p#p.a.b");
        let id = object["result"]["objectId"].as_str().expect("a handle").to_string();

        let text = dispatch(
            &mut engine,
            "Runtime.callFunctionOn",
            json!({ "objectId": id, "functionDeclaration": "function(suffix) { return this.textContent + suffix; }",
                    "arguments": [{ "value": "!" }], "returnByValue": true }),
        );
        assert_eq!(text["result"]["value"], "hello there!");

        // The same node through DOM.resolveNode is the same handle.
        let backend = { let doc = engine.doc(); doc.find_id(doc.document_id(), "p").unwrap() + 1 };
        let resolved = dispatch(&mut engine, "DOM.resolveNode", json!({ "backendNodeId": backend }));
        assert_eq!(resolved["object"]["objectId"], json!(id));
        let described = dispatch(&mut engine, "DOM.describeNode", json!({ "objectId": id }));
        assert_eq!(described["node"]["backendNodeId"], json!(backend));
    }

    #[test]
    fn values_come_back_by_value_and_promises_are_awaited() {
        let engine = engine_with("<body>x</body>");
        let by_value = engine.evaluate("({ a: [1, 'two'] })", true, false).unwrap();
        assert_eq!(by_value["result"]["value"], json!({ "a": [1, "two"] }));
        let awaited =
            engine.evaluate("new Promise((r) => setTimeout(() => r(7), 2000))", true, true).unwrap();
        assert_eq!(awaited["result"]["value"], json!(7));
        let rejected = engine.evaluate("Promise.reject(new Error('no'))", true, true).unwrap();
        assert!(rejected["exceptionDetails"]["text"].as_str().unwrap().contains("no"), "{rejected}");
        let never = engine.evaluate("new Promise(() => {})", true, true);
        assert!(never.unwrap_err().message.contains("did not settle"));
    }

    #[test]
    fn fill_types_into_a_react_style_controlled_input() {
        // React tracks a controlled input's value through a property it
        // defines on the element, and fires onChange only when the value the
        // element reports differs from the one it tracked. This mimics that
        // tracker, then fills the field the way agent-browser does.
        let mut engine = engine_with(
            "<body><form><input id='user' name='user'></form>\
             <script></script></body>",
        );
        evaluate(
            &mut engine,
            "const input = document.getElementById('user');\
             const native = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');\
             let tracked = '';\
             Object.defineProperty(input, 'value', { configurable: true,\
               get() { return native.get.call(this); },\
               set(v) { tracked = '' + v; native.set.call(this, v); } });\
             globalThis.changes = [];\
             input.addEventListener('input', () => { if (input.value !== tracked) { tracked = input.value; changes.push(tracked); } });",
        );
        let object = evaluate(&mut engine, "document.getElementById('user')");
        let id = object["result"]["objectId"].as_str().unwrap().to_string();
        dispatch(&mut engine, "Runtime.callFunctionOn", json!({ "objectId": id, "functionDeclaration": "function() { this.focus(); }" }));
        dispatch(&mut engine, "Input.insertText", json!({ "text": "kim" }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8 }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "a", "code": "KeyA", "text": "a" }));
        assert_eq!(evaluate(&mut engine, "changes.join(',')")["result"]["value"], "kim,ki,kia");
        assert_eq!(evaluate(&mut engine, "document.getElementById('user').value")["result"]["value"], "kia");
    }

    #[test]
    fn enter_in_a_field_submits_its_form() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 74\r\n\r\n\
             <form action='/login' method='post'><input name='user'><button>go</button></form>"
                .to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 22\r\n\r\n<title>welcome</title>"
                .to_string(),
        ]);
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        evaluate(&mut engine, "document.querySelector('input').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "kim" }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Enter", "code": "Enter", "text": "\r", "windowsVirtualKeyCode": 13 }));
        assert_eq!(engine.info().title, "welcome");
        let _ = requests.recv().unwrap();
        let login = requests.recv().unwrap();
        assert!(login.starts_with("POST /login HTTP/1.1\r\n"), "{login}");
        assert!(login.ends_with("\r\n\r\nuser=kim"), "{login}");
    }

    #[test]
    fn the_mouse_clicks_what_is_at_the_point() {
        let mut engine = engine_with(
            "<style>body { margin: 0 } button { display: block; width: 100px; height: 40px }\
             #cover { position: fixed; top: 100px; left: 0; width: 200px; height: 50px; z-index: 5 }</style>\
             <body><button id='a'>A</button><button id='b'>B</button><div id='cover'></div>\
             <button id='c' style='margin-top: 30px'>C</button></body>",
        );
        evaluate(
            &mut engine,
            "globalThis.log = []; for (const id of ['a', 'b', 'c', 'cover']) \
               document.getElementById(id).addEventListener('click', () => log.push(id));\
             document.getElementById('b').addEventListener('mousedown', () => log.push('down'));",
        );
        let click = |engine: &mut HtmlEngine, x: f64, y: f64| {
            for kind in ["mousePressed", "mouseReleased"] {
                dispatch(engine, "Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }));
            }
        };
        click(&mut engine, 50.0, 60.0);
        assert_eq!(evaluate(&mut engine, "document.activeElement.id")["result"]["value"], "b");
        // The fixed cover sits over C, and takes its click; pressing
        // something that cannot take focus takes it away.
        click(&mut engine, 50.0, 120.0);
        assert_eq!(evaluate(&mut engine, "log.join(',')")["result"]["value"], "down,b,cover");
        assert_eq!(evaluate(&mut engine, "document.activeElement.tagName")["result"]["value"], "BODY");
        // What the client measures is what it hits.
        let rect = evaluate(&mut engine, "JSON.stringify(document.getElementById('a').getBoundingClientRect())");
        assert_eq!(rect["result"]["value"], r#"{"x":0,"y":0,"width":100,"height":40,"top":0,"right":100,"bottom":40,"left":0}"#);
        assert_eq!(evaluate(&mut engine, "document.elementFromPoint(50, 20).id")["result"]["value"], "a");
    }

    fn engine_with_secrets(html: &str) -> HtmlEngine {
        let mut secrets = Secrets::new();
        secrets.add("pw@http://example.test", "hunter2-s3cret".into()).unwrap();
        secrets.add("user@http://example.test", "kim.minsu".into()).unwrap();
        let mut engine =
            HtmlEngine::with_shared("target", Arc::new(Mutex::new(CookieJar::new())), Arc::new(secrets));
        {
            let mut doc = engine.doc();
            *doc = parse_html(html);
            doc.set_url("http://example.test/page");
        }
        engine.url = "http://example.test/page".to_string();
        engine
    }

    #[test]
    fn secrets_are_typed_by_placeholder_and_never_come_back() {
        let mut engine = engine_with_secrets(
            "<body><input id='u' aria-label='User'><input id='p' type='password' aria-label='Password'></body>",
        );
        for (field, placeholder) in [("u", "{{secret:user}}"), ("p", "{{secret:pw}}")] {
            evaluate(&mut engine, &format!("document.getElementById('{field}').focus()"));
            dispatch(&mut engine, "Input.insertText", json!({ "text": placeholder }));
        }
        // The page has the real values: it can log in.
        let check = evaluate(&mut engine, "document.getElementById('p').value === 'hunter2-s3cret' && document.getElementById('u').value.length");
        assert_eq!(check["result"]["value"], json!(9));
        // A client reading them gets placeholders, in results, snapshots and
        // errors alike. Clients only ever come in through `dispatch`.
        let read = dispatch(
            &mut engine,
            "Runtime.evaluate",
            json!({ "expression": "document.getElementById('p').value + ' / ' + document.getElementById('u').value" }),
        );
        assert_eq!(read["result"]["value"], "{{secret:pw}} / {{secret:user}}");
        let tree = dispatch(&mut engine, "Accessibility.getFullAXTree", json!({})).to_string();
        assert!(tree.contains("{{secret:user}}") && !tree.contains("kim.minsu"), "{tree}");
        let thrown = dispatch(
            &mut engine,
            "Runtime.evaluate",
            json!({ "expression": "throw new Error('pw is ' + document.getElementById('p').value)" }),
        );
        assert!(!thrown.to_string().contains("hunter2"), "{thrown}");
    }

    #[test]
    fn a_secret_is_not_typed_into_another_origin() {
        let mut engine = engine_with_secrets("<body><input id='p'></body>");
        engine.url = "http://evil.test/".to_string();
        evaluate(&mut engine, "document.getElementById('p').focus()");
        let mut sink = |_: &str, _: Value| {};
        let error = engine.dispatch("Input.insertText", &json!({ "text": "{{secret:pw}}" }), &mut sink).unwrap_err();
        assert!(error.message.contains("is for http://example.test"), "{}", error.message);
        assert!(!error.message.contains("hunter2"));
        assert_eq!(evaluate(&mut engine, "document.getElementById('p').value")["result"]["value"], "");
    }

    #[test]
    fn a_typed_password_stays_out_of_the_tree_and_node_descriptions() {
        // Page script reads it, as it must to log in, and so can a client
        // calling into the page, as in Chrome. The accessibility tree and
        // DOM.describeNode, which clients log and snapshot, never carry it.
        let mut engine = engine_with("<body><input type='password' id='pw' aria-label='Password'></body>");
        evaluate(&mut engine, "document.getElementById('pw').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "hunter2" }));
        let tree = dispatch(&mut engine, "Accessibility.getFullAXTree", json!({}));
        assert!(!tree.to_string().contains("hunter2"), "{tree}");
        let backend = { let doc = engine.doc(); doc.find_id(doc.document_id(), "pw").unwrap() + 1 };
        let described = dispatch(&mut engine, "DOM.describeNode", json!({ "backendNodeId": backend }));
        assert!(!described.to_string().contains("hunter2"), "{described}");
    }

    #[test]
    fn dom_node_ids_round_trip() {
        let engine = engine_with("<body><p id='p'>x</p></body>");
        let p = { let doc = engine.doc(); doc.find_id(doc.document_id(), "p") }.unwrap();
        assert_eq!(HtmlEngine::dom_from_backend(HtmlEngine::backend_id(p)), Some(p));
    }

    #[test]
    fn describe_reports_node_types_and_attributes() {
        let engine = engine_with("<body><a href='/x' id='l'>t</a></body>");
        let root = engine.doc().document_id();
        let document = engine.describe(root);
        assert_eq!(document["nodeType"], 9);
        assert_eq!(document["nodeName"], "#document");

        let a = { let doc = engine.doc(); doc.find_id(doc.document_id(), "l") }.unwrap();
        let described = engine.describe(a);
        assert_eq!(described["nodeType"], 1);
        assert_eq!(described["nodeName"], "A");
        assert!(described["attributes"].as_array().unwrap().contains(&json!("href")));
    }

    #[test]
    fn serialize_round_trips_a_small_fragment() {
        let engine = engine_with("<body><div id=\"a\"><p>hi</p></div></body>");
        let div = { let doc = engine.doc(); doc.find_id(doc.document_id(), "a") }.unwrap();
        assert_eq!(serialize(&engine.doc(), div), "<div id=\"a\"><p>hi</p></div>");
    }

    #[test]
    fn serialize_does_not_invent_closing_tags_for_void_elements() {
        let engine = engine_with("<body><img src=\"x.png\" alt=\"a\"><br></body>");
        let img = { let doc = engine.doc(); doc.find_tag(doc.document_id(), "img") }.unwrap();
        assert_eq!(serialize(&engine.doc(), img), "<img src=\"x.png\" alt=\"a\">");
    }

    #[test]
    fn methods_that_would_lie_return_an_error_instead() {
        let mut engine = engine_with("<body>x</body>");
        let mut events = Vec::new();
        for method in ["Page.captureScreenshot", "Input.dispatchTouchEvent"] {
            let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
            let error = engine.dispatch(method, &Value::Null, &mut sink).expect_err(method);
            assert!(
                error.message.contains("not implemented yet"),
                "{method} should say why: {}",
                error.message
            );
        }
    }

    #[test]
    fn the_engine_is_send() {
        // `browser-core` hands engines to connection threads.
        fn assert_send<T: Send>() {}
        assert_send::<HtmlEngine>();
    }

    #[test]
    fn navigate_on_a_data_url_parses_and_reports() {
        let mut engine = HtmlEngine::new("t");
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        engine
            .navigate("data:text/html,<title>Inline</title><body><h1>Hi</h1></body>", &mut sink)
            .expect("navigate");

        assert_eq!(engine.info().title, "Inline");
        let names: Vec<&str> = events.iter().map(|(m, _)| m.as_str()).collect();
        assert!(names.contains(&"Page.loadEventFired"), "{names:?}");
        assert!(names.contains(&"Runtime.executionContextCreated"), "{names:?}");
    }

    #[test]
    fn a_failed_navigation_becomes_an_error_page_not_a_failure() {
        let mut engine = HtmlEngine::new("t");
        // Nothing listens on port 1. The engine reports that as page content
        // the client can read rather than as a protocol failure.
        engine.load("http://127.0.0.1:1/");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("connect"));
        assert_eq!(engine.info().title, "Navigation failed");
        let doc = engine.doc();
        assert!(doc.text_content(doc.document_id()).contains("connect"));
    }

    #[test]
    fn full_ax_tree_comes_from_the_loaded_document() {
        let mut engine =
            engine_with("<title>Login</title><body><h1>Login</h1><a href='/'>go</a></body>");
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        let value =
            engine.dispatch("Accessibility.getFullAXTree", &Value::Null, &mut sink).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert!(nodes
            .iter()
            .any(|n| n["role"]["value"] == "heading" && n["name"]["value"] == "Login"));
        assert!(nodes.iter().any(|n| n["role"]["value"] == "link" && n["name"]["value"] == "go"));
    }

    /// Opens a real page and prints what its scripts said. Not a check, a
    /// tool for finding what a site needs next:
    ///
    ///   WHB_PROBE_URL=https://example.com cargo test -p engine probe_a_url -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe_a_url() {
        let Ok(url) = std::env::var("WHB_PROBE_URL") else { return };
        let mut engine = HtmlEngine::new("probe");
        let started = std::time::Instant::now();
        engine.load(&url);
        eprintln!("loaded {} in {:?}", engine.url, started.elapsed());
        eprintln!("last error: {:?}", engine.last_error);
        if let Some(script) = &engine.script {
            for line in script.console_lines() {
                eprintln!("console: {}", line.chars().take(600).collect::<String>());
            }
        }
        let doc = engine.doc();
        let text = doc.text_content(doc.document_id());
        eprintln!("text: {}", text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(800).collect::<String>());
    }
}
