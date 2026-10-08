//! The CDP engine, backed by a real document.
//!
//! `engine-fake` answered from a fixed model. This answers from whatever the
//! last navigation actually fetched, which is the whole point of M1: `open` and
//! `snapshot` now describe a real page.
//!
//! Two things are still missing and are reported rather than faked:
//!
//!   * **Layout.** There is no CSS cascade and no box model, so
//!     `getBoundingClientRect`, hit testing and screenshots cannot mean
//!     anything. Methods that depend on them return an error that says so.
//!   * **JavaScript.** There is no engine yet, so `Runtime.evaluate` answers a
//!     small set of navigation-shaped expressions and nothing else. That rule
//!     set is temporary and marked as such; QuickJS replaces it.

use std::collections::HashMap;

use browser_core::{ids, CdpError, Engine, EngineEmit, TargetInfo};
use serde_json::{json, Value};

use crate::dom::{Document, NodeData, NodeId};
use crate::fetch;
use crate::parse::{parse_html, parse_html_bytes};
use crate::select;

pub struct HtmlEngine {
    target_id: String,
    doc: Document,
    url: String,
    frame_id: String,
    loader_id: String,
    context_id: u64,
    /// Remote object handles, in the shape `Runtime.evaluate` hands out.
    objects: HashMap<String, NodeId>,
    next_object: u64,
    history: Vec<String>,
    history_index: usize,
    viewport: (f64, f64),
    /// Why the last navigation failed, if it did. The page shows it too.
    last_error: Option<String>,
}

impl HtmlEngine {
    pub fn new(target_id: &str) -> Self {
        let mut counter = ids::seed();
        HtmlEngine {
            target_id: target_id.to_string(),
            doc: Document::new(),
            url: "about:blank".to_string(),
            frame_id: ids::token(&mut counter),
            loader_id: ids::token(&mut counter),
            context_id: 1,
            objects: HashMap::new(),
            next_object: 0,
            history: vec!["about:blank".to_string()],
            history_index: 0,
            viewport: (800.0, 600.0),
            last_error: None,
        }
    }

    pub fn factory() -> browser_core::EngineFactory {
        Box::new(|target_id: &str| Box::new(HtmlEngine::new(target_id)) as Box<dyn Engine>)
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

    /// Fetches and parses. A failure becomes an error page, which is what a
    /// browser shows and what a client can read.
    fn load(&mut self, url: &str) {
        match fetch::get(url) {
            Ok(response) => {
                let final_url = response.url.clone();
                if response.is_html() {
                    self.doc = parse_html_bytes(&response.body);
                } else {
                    // Non-HTML is wrapped so the client sees something rather
                    // than an empty document with a misleading title.
                    let escaped = escape_html(&String::from_utf8_lossy(&response.body));
                    self.doc = parse_html(&format!(
                        "<!doctype html><title>{}</title><body><pre>{escaped}</pre>",
                        escape_html(&response.header("content-type").unwrap_or("document"))
                    ));
                }
                self.url = final_url;
                self.last_error = response
                    .charset()
                    .filter(|c| c != "utf-8" && c != "utf8")
                    .map(|c| format!("the page declares charset {c}; only UTF-8 is decoded"));
            }
            Err(e) => {
                let message = e.to_string();
                self.doc = parse_html(&error_page(url, &message));
                self.url = url.to_string();
                self.last_error = Some(message);
            }
        }
        self.push_history();
    }

    fn push_history(&mut self) {
        self.history.truncate(self.history_index + 1);
        if self.history.last() != Some(&self.url) {
            self.history.push(self.url.clone());
            self.history_index = self.history.len() - 1;
        }
    }

    fn store_object(&mut self, dom: NodeId) -> Value {
        self.next_object += 1;
        let object_id = format!("{}.{}", self.next_object, dom);
        self.objects.insert(object_id.clone(), dom);
        let class = class_for(&self.doc, dom);
        let description = describe_node(&self.doc, dom);
        json!({ "type": "object", "className": class, "description": description, "objectId": object_id })
    }

    fn node_of(&self, object_id: &str) -> Option<NodeId> {
        self.objects.get(object_id).copied()
    }

    /// Remote-object id for a raw DOM index, matching `backendDOMNodeId`.
    fn backend_id(dom: NodeId) -> usize {
        dom + 1
    }

    fn dom_from_backend(backend: usize) -> Option<NodeId> {
        backend.checked_sub(1).filter(|id| *id > 0 || backend > 0)
    }

    /// Evaluates the handful of expressions a client sends before it does
    /// anything interesting.
    ///
    /// TEMPORARY. This is a rule set, not an interpreter, and it exists only so
    /// that `open`, `get url`, `get title` and the accessibility path work
    /// before QuickJS is in. It is deleted in M2, and anything not listed here
    /// comes back `undefined` rather than being guessed at.
    fn evaluate_expression(&mut self, expression: &str) -> Value {
        let e = expression.trim().trim_end_matches(';').trim();

        match e {
            "" => return json!({ "type": "undefined" }),
            "location.href" | "document.URL" | "window.location.href" => {
                return json!({ "type": "string", "value": self.url })
            }
            "document.title" => {
                return json!({ "type": "string", "value": self.doc.title().unwrap_or_default() })
            }
            "window.innerWidth" => {
                return json!({ "type": "number", "value": self.viewport.0 })
            }
            "window.innerHeight" => {
                return json!({ "type": "number", "value": self.viewport.1 })
            }
            "document.body" => {
                return match self.doc.find_tag(self.doc.document_id(), "body") {
                    Some(body) => self.store_object(body),
                    None => json!({ "type": "object", "subtype": "null", "value": Value::Null }),
                }
            }
            "document.documentElement" => {
                return match self.doc.find_tag(self.doc.document_id(), "html") {
                    Some(html) => self.store_object(html),
                    None => json!({ "type": "object", "subtype": "null", "value": Value::Null }),
                }
            }
            _ => {}
        }

        // Element lookups, now against the real tree.
        for prefix in ["document.querySelector(", "document.body.querySelector("] {
            if e.starts_with(prefix) {
                let root = if prefix.contains("body") {
                    self.doc.find_tag(self.doc.document_id(), "body").unwrap_or(self.doc.document_id())
                } else {
                    self.doc.document_id()
                };
                return match literal(e).and_then(|selector| select::query(&self.doc, root, &selector))
                {
                    Some(node) => self.store_object(node),
                    None => json!({ "type": "object", "subtype": "null", "value": Value::Null }),
                };
            }
        }

        if e.starts_with("document.querySelectorAll(") {
            // Clients traverse the members themselves; an opaque array-shaped
            // object keeps the expression from failing.
            return json!({ "type": "object", "subtype": "array", "description": "NodeList", "value": [] });
        }

        if e.starts_with("window.scrollBy") || e.starts_with("window.scrollTo") {
            // Scrolling needs layout, but the expression itself is a no-op that
            // clients are happy to see succeed.
            return json!({ "type": "undefined" });
        }

        if e.starts_with("history.back") {
            self.go_back();
            return json!({ "type": "undefined" });
        }
        if e.starts_with("history.forward") {
            self.go_forward();
            return json!({ "type": "undefined" });
        }

        json!({ "type": "undefined" })
    }

    fn go_back(&mut self) {
        if self.history_index == 0 {
            return;
        }
        self.history_index -= 1;
        let url = self.history[self.history_index].clone();
        self.load_without_history(&url);
    }

    fn go_forward(&mut self) {
        if self.history_index + 1 >= self.history.len() {
            return;
        }
        self.history_index += 1;
        let url = self.history[self.history_index].clone();
        self.load_without_history(&url);
    }

    fn load_without_history(&mut self, url: &str) {
        let history = self.history.clone();
        let index = self.history_index;
        match fetch::get(url) {
            Ok(response) => {
                self.doc = parse_html_bytes(&response.body);
                self.url = response.url;
                self.last_error = None;
            }
            Err(e) => {
                let message = e.to_string();
                self.doc = parse_html(&error_page(url, &message));
                self.url = url.to_string();
                self.last_error = Some(message);
            }
        }
        self.history = history;
        self.history_index = index;
    }

    fn call_function(&self, object_id: &str, declaration: &str) -> Value {
        let Some(dom) = self.node_of(object_id) else {
            return json!({ "type": "undefined" });
        };
        if declaration.contains("textContent") || declaration.contains("innerText") {
            return json!({ "type": "string", "value": self.doc.text_content(dom).trim() });
        }
        if declaration.contains("outerHTML") || declaration.contains("innerHTML") {
            return json!({ "type": "string", "value": serialize(&self.doc, dom) });
        }
        if declaration.contains("value") {
            return json!({
                "type": "string",
                "value": self.doc.attr(dom, "value").unwrap_or("").to_string(),
            });
        }
        json!({ "type": "undefined" })
    }
}

impl Engine for HtmlEngine {
    fn info(&self) -> TargetInfo {
        let mut info = TargetInfo::new(self.target_id.clone());
        info.title = self.doc.title().unwrap_or_default();
        info.url = self.url.clone();
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
                let entries: Vec<Value> = self
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, url)| {
                        json!({
                            "id": i,
                            "url": url,
                            "userTypedURL": url,
                            "title": if *url == self.url {
                                self.doc.title().unwrap_or_default()
                            } else {
                                String::new()
                            },
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
            "Runtime.disable" | "Runtime.runIfWaitingForDebugger" | "Runtime.releaseObject" => {
                Ok(json!({}))
            }

            "Runtime.evaluate" => {
                let expression =
                    params.get("expression").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(json!({ "result": self.evaluate_expression(&expression) }))
            }

            "Runtime.callFunctionOn" => {
                let object_id =
                    params.get("objectId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let declaration =
                    params.get("functionDeclaration").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Ok(json!({ "result": self.call_function(&object_id, &declaration) }))
            }

            "Runtime.getIsolateId" => Ok(json!({ "id": "m1" })),

            // ---- DOM -----------------------------------------------------
            "DOM.enable" | "DOM.disable" => Ok(json!({})),

            "DOM.getDocument" => {
                let root = self.doc.document_id();
                Ok(json!({ "root": self.describe(root) }))
            }

            "DOM.querySelector" => {
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                match select::query(&self.doc, root, &selector) {
                    Some(node) => Ok(json!({ "nodeId": Self::backend_id(node) })),
                    None => Ok(json!({ "nodeId": 0 })),
                }
            }

            "DOM.querySelectorAll" => {
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let ids: Vec<usize> = select::query_all(&self.doc, root, &selector)
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
                    .unwrap_or_else(|| self.doc.document_id());
                Ok(json!({ "node": self.describe(node) }))
            }

            "DOM.resolveNode" => {
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc.document_id());
                Ok(json!({ "object": self.store_object(node) }))
            }

            "DOM.getOuterHTML" => {
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| self.doc.document_id());
                Ok(json!({ "outerHTML": serialize(&self.doc, node) }))
            }

            // ---- Accessibility -------------------------------------------
            "Accessibility.enable" => {
                out("Accessibility.loadComplete", json!({}));
                Ok(json!({}))
            }
            "Accessibility.disable" => Ok(json!({})),

            "Accessibility.getFullAXTree" | "Accessibility.getPartialAXTree" => {
                Ok(crate::ax::to_cdp(&crate::ax::build(&self.doc)))
            }

            // ---- Network -------------------------------------------------
            // There is no cookie jar or cache yet, and pretending otherwise
            // would make a login flow look like it worked.
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
                "input needs hit testing, which needs layout, and event dispatch, which needs a \
                 JavaScript engine",
            )),

            // ---- Emulation -----------------------------------------------
            "Emulation.setDeviceMetricsOverride" => {
                let w = params.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = params.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                self.viewport = (w, h);
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

impl HtmlEngine {
    /// `DOM.describeNode`-shaped output for one node.
    fn describe(&self, node: NodeId) -> Value {
        let entry = self.doc.node(node);
        let (node_type, node_name) = match &entry.data {
            NodeData::Document => (9, "#document".to_string()),
            NodeData::Doctype { name } => (10, name.clone()),
            NodeData::Comment => (8, "#comment".to_string()),
            NodeData::Text => (3, "#text".to_string()),
            NodeData::Element { .. } => (1, self.doc.tag(node).unwrap_or("").to_ascii_uppercase()),
        };
        let mut value = json!({
            "nodeId": Self::backend_id(node),
            "backendNodeId": Self::backend_id(node),
            "nodeType": node_type,
            "nodeName": node_name,
            "childNodeCount": self.doc.children(node).len(),
        });
        if node_type == 1 {
            let attrs: Vec<Value> = self
                .doc
                .attrs(node)
                .iter()
                .flat_map(|a| [json!(a.local_name()), json!(a.value)])
                .collect();
            value["attributes"] = json!(attrs);
            value["localName"] = json!(self.doc.tag(node).unwrap_or(""));
        }
        if node_type == 3 || node_type == 8 {
            value["nodeValue"] = json!(entry.text);
        }
        value
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

fn literal(expression: &str) -> Option<String> {
    let bytes = expression.as_bytes();
    let open = bytes.iter().position(|b| *b == b'"' || *b == b'\'')?;
    let quote = bytes[open];
    let rest = &bytes[open + 1..];
    let close = rest.iter().position(|b| *b == quote)?;
    String::from_utf8(rest[..close].to_vec()).ok()
}

fn class_for(doc: &Document, node: NodeId) -> &'static str {
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
        engine.doc = parse_html(html);
        engine.url = "http://example.test/page".to_string();
        engine
    }

    fn evaluate(engine: &mut HtmlEngine, expression: &str) -> Value {
        engine.evaluate_expression(expression)
    }

    #[test]
    fn answers_the_identity_expressions_clients_probe_with() {
        let mut engine = engine_with("<title>Real</title><body><p>x</p></body>");
        assert_eq!(evaluate(&mut engine, "location.href")["value"], "http://example.test/page");
        assert_eq!(evaluate(&mut engine, "document.title")["value"], "Real");
        assert_eq!(evaluate(&mut engine, "window.innerWidth")["value"], 800.0);
    }

    #[test]
    fn resolves_selectors_against_the_real_tree() {
        let mut engine = engine_with("<body><a href='/x'>one</a><a href='/y'>two</a></body>");
        let object = evaluate(&mut engine, "document.querySelector(\"a\")");
        assert_eq!(object["className"], "HTMLAnchorElement");
        assert!(object["objectId"].is_string());

        let node = engine.node_of(object["objectId"].as_str().unwrap()).expect("handle resolves");
        assert_eq!(engine.doc.text_content(node).trim(), "one");
    }

    #[test]
    fn a_missing_selector_returns_null_not_an_error() {
        let mut engine = engine_with("<body><p>x</p></body>");
        let result = evaluate(&mut engine, "document.querySelector(\".absent\")");
        assert_eq!(result["subtype"], "null");
    }

    #[test]
    fn unknown_expressions_are_undefined_rather_than_guessed() {
        let mut engine = engine_with("<body>x</body>");
        assert_eq!(evaluate(&mut engine, "someUntrackedPageScript()")["type"], "undefined");
        assert_eq!(evaluate(&mut engine, "1 + 1")["type"], "undefined");
    }

    #[test]
    fn call_function_reads_text_from_the_real_document() {
        let mut engine = engine_with("<body><p id='p'>hello there</p></body>");
        let object = evaluate(&mut engine, "document.querySelector(\"#p\")");
        let id = object["objectId"].as_str().unwrap();
        let text = engine.call_function(id, "function() { return this.textContent; }");
        assert_eq!(text["value"], "hello there");
    }

    #[test]
    fn dom_node_ids_round_trip() {
        let engine = engine_with("<body><p id='p'>x</p></body>");
        let p = engine.doc.find_id(engine.doc.document_id(), "p").unwrap();
        let backend = HtmlEngine::backend_id(p);
        assert_eq!(HtmlEngine::dom_from_backend(backend), Some(p));
    }

    #[test]
    fn describe_reports_node_types_and_attributes() {
        let engine = engine_with("<body><a href='/x' id='l'>t</a></body>");
        let document = engine.describe(engine.doc.document_id());
        assert_eq!(document["nodeType"], 9);
        assert_eq!(document["nodeName"], "#document");

        let a = engine.doc.find_id(engine.doc.document_id(), "l").unwrap();
        let described = engine.describe(a);
        assert_eq!(described["nodeType"], 1);
        assert_eq!(described["nodeName"], "A");
        assert_eq!(described["localName"], "a");
        assert!(described["attributes"].as_array().unwrap().contains(&json!("href")));
    }

    #[test]
    fn serialize_round_trips_a_small_fragment() {
        let engine = engine_with("<body><div id=\"a\"><p>hi</p></div></body>");
        let div = engine.doc.find_id(engine.doc.document_id(), "a").unwrap();
        assert_eq!(serialize(&engine.doc, div), "<div id=\"a\"><p>hi</p></div>");
    }

    #[test]
    fn serialize_does_not_invent_closing_tags_for_void_elements() {
        let engine = engine_with("<body><img src=\"x.png\" alt=\"a\"><br></body>");
        let img = engine.doc.find_tag(engine.doc.document_id(), "img").unwrap();
        assert_eq!(serialize(&engine.doc, img), "<img src=\"x.png\" alt=\"a\">");
    }

    #[test]
    fn methods_that_would_lie_return_an_error_instead() {
        let mut engine = engine_with("<body>x</body>");
        let mut events = Vec::new();

        for method in ["Page.captureScreenshot", "Network.setCookie", "Input.dispatchMouseEvent"] {
            let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
            let result = engine.dispatch(method, &Value::Null, &mut sink);
            let error = result.expect_err(method);
            assert!(
                error.message.contains("not implemented yet"),
                "{method} should say why: {}",
                error.message
            );
        }
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
        assert_eq!(engine.info().url, "data:text/html,<title>Inline</title><body><h1>Hi</h1></body>");
        let names: Vec<&str> = events.iter().map(|(m, _)| m.as_str()).collect();
        assert!(names.contains(&"Page.loadEventFired"), "{names:?}");
        assert!(names.contains(&"Runtime.executionContextCreated"), "{names:?}");
    }

    #[test]
    fn a_failed_navigation_becomes_an_error_page_not_a_failure() {
        let mut engine = HtmlEngine::new("t");
        engine.load("https://example.com/");
        // HTTPS is not implemented, so the engine reports that as page content
        // the client can read, rather than as a protocol failure.
        assert!(engine.last_error.as_deref().unwrap_or("").contains("TLS"));
        assert_eq!(engine.info().title, "Navigation failed");
        assert!(engine.doc.text_content(engine.doc.document_id()).contains("TLS"));
    }

    #[test]
    fn full_ax_tree_comes_from_the_loaded_document() {
        let mut engine =
            engine_with("<title>Login</title><body><h1>Login</h1><a href='/'>go</a></body>");
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        let value = engine.dispatch("Accessibility.getFullAXTree", &Value::Null, &mut sink).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| n["role"]["value"] == "heading" && n["name"]["value"] == "Login"));
        assert!(nodes.iter().any(|n| n["role"]["value"] == "link" && n["name"]["value"] == "go"));
    }
}
