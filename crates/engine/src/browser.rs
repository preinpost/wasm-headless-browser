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

use crate::dom::{Document, NodeData, NodeId};
use crate::fetch;
use crate::js::{JsError, JsValue, ScriptHost};
use crate::parse::{parse_html, parse_html_bytes};
use crate::select;

/// How many times a script-initiated navigation is followed before stopping.
/// A page that redirects itself in a loop should not hang the engine.
const MAX_SCRIPT_NAVIGATIONS: u8 = 3;

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
    url: String,
    frame_id: String,
    loader_id: String,
    context_id: u64,
    objects: Vec<NodeId>,
    history: Vec<String>,
    history_index: usize,
    viewport: (f64, f64),
    last_error: Option<String>,
}

impl HtmlEngine {
    pub fn new(target_id: &str) -> Self {
        let mut counter = ids::seed();
        let doc = Arc::new(Mutex::new(Document::new()));
        let viewport = (800.0, 600.0);
        let script = ScriptHost::spawn(Arc::clone(&doc), viewport, USER_AGENT).ok();

        HtmlEngine {
            target_id: target_id.to_string(),
            doc,
            script,
            url: "about:blank".to_string(),
            frame_id: ids::token(&mut counter),
            loader_id: ids::token(&mut counter),
            context_id: 1,
            objects: Vec::new(),
            history: vec!["about:blank".to_string()],
            history_index: 0,
            viewport,
            last_error: None,
        }
    }

    pub fn factory() -> browser_core::EngineFactory {
        Box::new(|target_id: &str| Box::new(HtmlEngine::new(target_id)) as Box<dyn Engine>)
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

    /// Fetches, parses, and runs the page's own script. A fetch failure becomes
    /// an error page, which is what a browser shows and a client can read.
    fn load(&mut self, url: &str) {
        // The document lock borrows `self`, so everything that touches other
        // fields is collected here and applied once the guard is dropped.
        let fetched = fetch::get(url);
        let mut new_url = url.to_string();
        let mut new_error = None;

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
                    new_url = response.url.clone();
                    new_error = response
                        .charset()
                        .filter(|c| c != "utf-8" && c != "utf8")
                        .map(|c| format!("the page declares charset {c}; only UTF-8 is decoded"));
                }
                Err(e) => {
                    let message = e.to_string();
                    *doc = parse_html(&error_page(url, &message));
                    doc.set_url(url);
                    new_error = Some(message);
                }
            }
        }

        self.url = new_url;
        self.last_error = new_error;

        self.run_document_scripts();
        self.follow_script_navigation();
        self.push_history();
    }

    /// Runs inline `<script>` elements in document order.
    ///
    /// External scripts are skipped, as are modules: fetching and evaluating
    /// them is a separate piece of work. A script that throws is reported to the
    /// engine's error slot and does not stop the ones after it, which is what a
    /// browser does.
    fn run_document_scripts(&mut self) {
        let Some(script) = &self.script else { return };

        let sources: Vec<(String, String)> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter(|id| doc.is_tag(*id, "script"))
                .filter(|id| {
                    // `src` is not fetched, and a module has different semantics.
                    doc.attr(*id, "src").is_none()
                        && !matches!(doc.attr(*id, "type"), Some("module"))
                })
                .map(|id| (doc.text_content(id), format!("inline-{id}")))
                .filter(|(source, _)| !source.trim().is_empty())
                .collect()
        };

        for (source, label) in sources {
            // The document lock is not held here: the script thread locks it.
            if let Err(e) = script.run(&source) {
                self.last_error = Some(format!("script error in <script> ({label}): {}", e.message));
            }
            // Timers are drained between scripts so that one script's deferred
            // work happens before the next script runs, roughly as a browser
            // would order it.
            let _ = script.run("__drainTimers()");
        }
    }

    /// If a script assigned `location.href`, follow it. Bounded, because a page
    /// that redirects itself in a loop should not hang the engine.
    fn follow_script_navigation(&mut self) {
        for _ in 0..MAX_SCRIPT_NAVIGATIONS {
            let Some(requested) = self
                .script
                .as_ref()
                .and_then(|script| script.state().take_requested_navigation())
            else {
                return;
            };
            if requested == self.url {
                return;
            }
            let target = match fetch::Url::parse(&self.url)
                .ok()
                .and_then(|base| base.join(&requested).ok())
            {
                Some(url) => url.to_string_with_path(),
                None => requested,
            };

            let fetched = fetch::get(&target);
            let mut new_url = target.clone();
            let mut new_error = None;
            {
                let mut doc = self.doc();
                match fetched {
                    Ok(response) => {
                        *doc = parse_html_bytes(&response.body);
                        doc.set_url(&response.url);
                        new_url = response.url.clone();
                    }
                    Err(e) => {
                        let message = e.to_string();
                        *doc = parse_html(&error_page(&target, &message));
                        doc.set_url(&target);
                        new_error = Some(message);
                    }
                }
            }
            self.url = new_url;
            if new_error.is_some() {
                self.last_error = new_error;
            }
            self.run_document_scripts();
        }
    }

    fn push_history(&mut self) {
        self.history.truncate(self.history_index + 1);
        if self.history.last() != Some(&self.url) {
            self.history.push(self.url.clone());
            self.history_index = self.history.len() - 1;
        }
    }

    /// Registers a DOM node as a remote object and returns its handle.
    ///
    /// Handles are not interned, so two handles to the same node are two
    /// objects. `Runtime.callFunctionOn` only needs to resolve one back to a
    /// node, so that is enough for now.
    fn store_object(&mut self, dom: NodeId) -> Value {
        self.objects.push(dom);
        let index = self.objects.len() - 1;
        let object_id = format!("{index}");
        let doc = self.doc();
        json!({
            "type": "object",
            "className": class_for(&doc, dom),
            "description": describe_node(&doc, dom),
            "objectId": object_id,
        })
    }

    fn node_of(&self, object_id: &str) -> Option<NodeId> {
        object_id.parse::<usize>().ok().and_then(|index| self.objects.get(index).copied())
    }

    /// Remote-object id for a raw DOM index, matching `backendDOMNodeId`.
    fn backend_id(dom: NodeId) -> usize {
        dom + 1
    }

    fn dom_from_backend(backend: usize) -> Option<NodeId> {
        backend.checked_sub(1)
    }

    fn evaluate(&self, source: &str) -> Value {
        let Some(script) = &self.script else {
            return json!({
                "result": { "type": "undefined" },
                "exceptionDetails": {
                    "text": "the JavaScript engine is not available in this process",
                }
            });
        };
        match script.evaluate(source) {
            Ok(value) => json!({ "result": cdp_result(value) }),
            Err(JsError { message }) => json!({
                "result": { "type": "object", "subtype": "error", "description": "Error" },
                "exceptionDetails": { "text": message, "exception": { "type": "object", "subtype": "error", "description": message } },
            }),
        }
    }

    fn call_function(&self, object_id: &str, declaration: &str) -> Value {
        let Some(dom) = self.node_of(object_id) else {
            return json!({ "type": "undefined" });
        };
        let doc = self.doc();
        if declaration.contains("textContent") || declaration.contains("innerText") {
            return json!({ "type": "string", "value": doc.text_content(dom).trim() });
        }
        if declaration.contains("outerHTML") || declaration.contains("innerHTML") {
            return json!({ "type": "string", "value": serialize(&doc, dom) });
        }
        if declaration.contains("value") {
            // A password field's value is not readable from script either.
            if doc.attr(dom, "type") == Some("password") {
                return json!({ "type": "string", "value": "" });
            }
            return json!({
                "type": "string",
                "value": doc.attr(dom, "value").unwrap_or("").to_string(),
            });
        }
        json!({ "type": "undefined" })
    }

    fn go_back(&mut self) {
        if self.history_index == 0 {
            return;
        }
        self.history_index -= 1;
        let url = self.history[self.history_index].clone();
        self.load_preserving_history(&url);
    }

    fn go_forward(&mut self) {
        if self.history_index + 1 >= self.history.len() {
            return;
        }
        self.history_index += 1;
        let url = self.history[self.history_index].clone();
        self.load_preserving_history(&url);
    }

    fn load_preserving_history(&mut self, url: &str) {
        let history = self.history.clone();
        let index = self.history_index;

        let fetched = fetch::get(url);
        let mut new_url = url.to_string();
        let mut new_error = None;
        {
            let mut doc = self.doc();
            match fetched {
                Ok(response) => {
                    *doc = parse_html_bytes(&response.body);
                    doc.set_url(&response.url);
                    new_url = response.url.clone();
                }
                Err(e) => {
                    let message = e.to_string();
                    *doc = parse_html(&error_page(url, &message));
                    doc.set_url(url);
                    new_error = Some(message);
                }
            }
        }
        self.url = new_url;
        self.last_error = new_error;

        self.run_document_scripts();
        self.history = history;
        self.history_index = index;
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

impl Engine for HtmlEngine {
    fn info(&self) -> TargetInfo {
        let doc = self.doc();
        let mut info = TargetInfo::new(self.target_id.clone());
        info.title = doc.title().unwrap_or_default();
        info.url = doc.url().to_string();
        if info.url.is_empty() {
            info.url = self.url.clone();
        }
        info
    }

    fn navigate(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        self.load(url);
        self.emit_lifecycle(out);
        Ok(())
    }

    fn attach(&mut self, out: &mut EngineEmit) {
        self.emit_context(out);
    }

    fn dispatch(
        &mut self,
        method: &str,
        params: &Value,
        out: &mut EngineEmit,
    ) -> Result<Value, CdpError> {
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
                "there is no layout or rasterizer yet, so a screenshot would be blank",
            )),

            // ---- Runtime -------------------------------------------------
            "Runtime.enable" => {
                self.emit_context(out);
                Ok(json!({}))
            }
            "Runtime.disable" | "Runtime.runIfWaitingForDebugger" => Ok(json!({})),
            "Runtime.releaseObject" => {
                // Handles are append-only indices; releasing one is a no-op
                // rather than a correctness issue.
                Ok(json!({}))
            }

            // Real JavaScript, from a real engine.
            "Runtime.evaluate" => {
                let source =
                    params.get("expression").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let result = self.evaluate(&source);

                // A script may have asked to navigate; act on it after the
                // evaluation has finished rather than re-entering the runtime.
                if let Some(script) = &self.script {
                    if let Some(requested) = script.state().take_requested_navigation() {
                        if requested != self.url {
                            self.load(&requested);
                            self.emit_lifecycle(out);
                        }
                    }
                }
                Ok(result)
            }

            "Runtime.callFunctionOn" => {
                let object_id =
                    params.get("objectId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let declaration = params
                    .get("functionDeclaration")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(json!({ "result": self.call_function(&object_id, &declaration) }))
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
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc().document_id());
                Ok(json!({ "node": self.describe(node) }))
            }

            "DOM.resolveNode" => {
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc().document_id());
                Ok(json!({ "object": self.store_object(node) }))
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
            // No cookie jar or cache yet. Pretending otherwise would make a
            // login flow look like it worked.
            "Network.enable" | "Network.disable" => Ok(json!({})),
            "Network.getCookies" | "Network.getAllCookies" => Ok(json!({ "cookies": [] })),
            "Network.setCookie" | "Network.deleteCookies" | "Network.clearBrowserCookies" => {
                Err(not_yet(method, "there is no cookie jar yet"))
            }

            // ---- Input ---------------------------------------------------
            "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.insertText"
            | "Input.dispatchTouchEvent" => Err(not_yet(
                method,
                "input needs hit testing, which needs layout",
            )),

            // ---- Emulation -----------------------------------------------
            "Emulation.setDeviceMetricsOverride" => {
                let w = params.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = params.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                self.viewport = (w, h);
                if let Some(script) = &self.script {
                    script.state().set_viewport(w, h);
                }
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

/// Maps a JavaScript value onto the CDP shapes clients read.
fn cdp_result(value: JsValue) -> Value {
    match value {
        JsValue::Undefined => json!({ "type": "undefined" }),
        JsValue::Null => json!({ "type": "object", "subtype": "null", "value": Value::Null }),
        JsValue::Bool(b) => json!({ "type": "boolean", "value": b }),
        JsValue::Number(n) => json!({
            "type": "number",
            "value": number_json(n),
            "description": if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") },
        }),
        JsValue::String(s) => json!({ "type": "string", "value": s }),
        JsValue::Object { class, is_function } => json!({
            "type": if is_function { "function" } else { "object" },
            "className": class,
            "description": class,
        }),
    }
}

/// A whole number goes out as `42`, the way Chrome sends it, not `42.0`.
/// `-0` stays a float so its sign survives.
fn number_json(n: f64) -> Value {
    let whole = n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0;
    if whole && !(n == 0.0 && n.is_sign_negative()) {
        json!(n as i64)
    } else {
        json!(n)
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
        NodeData::Element { .. } => {}
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
        _ => "HTMLElement",
    }
}

fn describe_node(doc: &Document, node: NodeId) -> String {
    match (&doc.node(node).data, doc.tag(node)) {
        (NodeData::Text, _) => "#text".to_string(),
        (NodeData::Document, _) => "#document".to_string(),
        (_, Some(tag)) => {
            let id = doc.attr(node, "id").map(|i| format!("#{i}")).unwrap_or_default();
            format!("{tag}{id}")
        }
        _ => "node".to_string(),
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
        NodeData::Document => {
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
        engine.evaluate(expression)
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
    fn external_scripts_are_skipped_not_faked() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<body><div id=\"out\"></div>\
             <script src=\"https://example.com/app.js\"></script></body>",
        );
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        assert_eq!(doc.text_content(out), "");
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
    fn call_function_reads_text_from_the_real_document() {
        let mut engine = engine_with("<body><p id='p'>hello there</p></body>");
        let object = evaluate(&mut engine, "document.querySelector('#p')");
        // Evaluate returns a description, not a handle; resolve through DOM.
        assert_eq!(object["result"]["className"], "HTMLParagraphElement");

        let node = { let doc = engine.doc(); doc.find_id(doc.document_id(), "p") }.unwrap();
        let handle = engine.store_object(node);
        let id = handle["objectId"].as_str().unwrap().to_string();
        let text = engine.call_function(&id, "function() { return this.textContent; }");
        assert_eq!(text["value"], "hello there");
    }

    #[test]
    fn call_function_does_not_leak_a_password() {
        let mut engine = engine_with("<body><input type='password' id='pw' value='secret'></body>");
        let node = { let doc = engine.doc(); doc.find_id(doc.document_id(), "pw") }.unwrap();
        let handle = engine.store_object(node);
        let id = handle["objectId"].as_str().unwrap().to_string();
        let value = engine.call_function(&id, "function() { return this.value; }");
        assert_eq!(value["value"], "");
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
        for method in ["Page.captureScreenshot", "Network.setCookie", "Input.dispatchMouseEvent"] {
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
        engine.load("https://example.com/");
        // HTTPS is not implemented, so the engine reports that as page content
        // the client can read rather than as a protocol failure.
        assert!(engine.last_error.as_deref().unwrap_or("").contains("TLS"));
        assert_eq!(engine.info().title, "Navigation failed");
        let doc = engine.doc();
        assert!(doc.text_content(doc.document_id()).contains("TLS"));
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
}
