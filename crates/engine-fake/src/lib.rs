//! M0 engine.
//!
//! It answers the CDP commands a client needs in order to prove the plumbing:
//! attach, navigate, evaluate, snapshot. Every answer comes from a fixed page
//! model, so the results are self-consistent but not derived from any real
//! HTML.

pub mod ax;
pub mod dom;
pub mod eval;
pub mod png;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use browser_core::{ids, CdpError, Engine, EngineEmit, TargetInfo};
use serde_json::{json, Value};

use dom::Document;

struct ObjectRef {
    dom: usize,
}

pub struct FakeEngine {
    target_id: String,
    doc: Document,
    objects: HashMap<String, ObjectRef>,
    next_object: u64,
    context_id: u64,
    frame_id: String,
    loader_id: String,
}

impl FakeEngine {
    pub fn new(target_id: &str) -> Self {
        let mut counter = ids::seed();
        FakeEngine {
            target_id: target_id.to_string(),
            doc: Document::blank(),
            objects: HashMap::new(),
            next_object: 0,
            context_id: 1,
            frame_id: ids::token(&mut counter),
            loader_id: ids::token(&mut counter),
        }
    }

    /// A page factory suitable for `Browser::new`.
    pub fn factory() -> browser_core::EngineFactory {
        Box::new(|target_id: &str| Box::new(FakeEngine::new(target_id)) as Box<dyn Engine>)
    }

    fn origin(&self) -> String {
        let url = &self.doc.url;
        match url.split_once("://") {
            Some((scheme, rest)) => {
                let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                format!("{scheme}://{host}")
            }
            None => "null".into(),
        }
    }

    fn frame_json(&self) -> Value {
        json!({
            "id": self.frame_id,
            "loaderId": self.loader_id,
            "url": self.doc.url,
            "domainAndRegistry": "",
            "mimeType": "text/html",
            "securityOrigin": self.origin(),
            "secureContextType": "Secure",
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
                    "auxData": {
                        "isDefault": true,
                        "type": "default",
                        "frameId": self.frame_id,
                    },
                }
            }),
        );
    }

    fn emit_lifecycle(&self, out: &mut EngineEmit) {
        let timestamp = now();
        out("Page.frameStartedLoading", json!({ "frameId": self.frame_id }));
        out(
            "Page.frameNavigated",
            json!({ "frame": self.frame_json(), "type": "Navigation" }),
        );
        out("Runtime.executionContextsCleared", json!({}));
        self.emit_context(out);
        out("Page.domContentEventFired", json!({ "timestamp": timestamp }));
        out("Page.loadEventFired", json!({ "timestamp": timestamp }));
    }

    fn store_object(&mut self, class: &'static str, description: String, dom: usize) -> Value {
        self.next_object += 1;
        let object_id = format!("{}.{}", self.next_object, dom);
        self.objects.insert(object_id.clone(), ObjectRef { dom });
        json!({
            "type": "object",
            "className": class,
            "description": description,
            "objectId": object_id,
        })
    }

    fn object_text(&self, object_id: &str) -> Option<String> {
        let entry = self.objects.get(object_id)?;
        if entry.dom == usize::MAX {
            return Some(String::new());
        }
        Some(self.doc.text_of(entry.dom))
    }
}

impl Engine for FakeEngine {
    fn info(&self) -> TargetInfo {
        let mut info = TargetInfo::new(self.target_id.clone());
        info.title = self.doc.title.clone();
        info.url = self.doc.url.clone();
        info
    }

    fn navigate(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        self.doc.navigate(url);
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
                let url = params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("about:blank")
                    .to_string();
                self.doc.navigate(&url);
                self.emit_lifecycle(out);
                Ok(json!({
                    "frameId": self.frame_id,
                    "loaderId": self.loader_id,
                    "type": "Navigation",
                }))
            }

            "Page.reload" => {
                let url = self.doc.url.clone();
                self.doc.navigate(&url);
                self.emit_lifecycle(out);
                Ok(json!({}))
            }

            "Page.getNavigationHistory" => {
                let entries: Vec<Value> = self
                    .doc
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, url)| {
                        json!({
                            "id": i,
                            "url": url,
                            "userTypedURL": url,
                            "title": if *url == self.doc.url { self.doc.title.clone() } else { String::new() },
                            "transitionType": "typed",
                        })
                    })
                    .collect();
                Ok(json!({
                    "currentIndex": self.doc.history_index,
                    "entries": entries,
                }))
            }

            "Page.captureScreenshot" => {
                let bytes = png::render(&self.doc);
                Ok(json!({ "data": png::to_base64(&bytes) }))
            }

            "Page.getLayoutMetrics" => {
                let (w, h) = self.doc.viewport;
                Ok(json!({
                    "layoutViewport": { "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h },
                    "visualViewport": { "offsetX": 0, "offsetY": 0, "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h, "scale": 1 },
                    "contentSize": { "x": 0, "y": 0, "width": w, "height": h },
                }))
            }

            "Page.setLifecycleEventsEnabled" | "Page.setBypassCSP" => Ok(json!({})),

            "Page.addScriptToEvaluateOnNewDocument" => Ok(json!({ "identifier": "1" })),

            "Page.createIsolatedWorld" => Ok(json!({ "executionContextId": self.context_id })),

            // ---- Runtime -------------------------------------------------
            "Runtime.enable" => {
                self.emit_context(out);
                Ok(json!({}))
            }
            "Runtime.disable" | "Runtime.runIfWaitingForDebugger" | "Runtime.releaseObject" => {
                Ok(json!({}))
            }

            "Runtime.evaluate" => {
                let expression = params.get("expression").and_then(|v| v.as_str()).unwrap_or("");
                let result = match eval::evaluate(expression, &mut self.doc) {
                    eval::Outcome::Value(v) => v,
                    eval::Outcome::Object { class, description, dom } => {
                        self.store_object(class, description, dom)
                    }
                    eval::Outcome::Undefined => json!({ "type": "undefined" }),
                };
                Ok(json!({ "result": result }))
            }

            "Runtime.callFunctionOn" => {
                let object_id =
                    params.get("objectId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let declaration = params
                    .get("functionDeclaration")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if declaration.contains("textContent") || declaration.contains("innerText") {
                    let text = self.object_text(&object_id).unwrap_or_default();
                    return Ok(json!({ "result": { "type": "string", "value": text } }));
                }
                if declaration.contains("value") {
                    let value = self
                        .objects
                        .get(&object_id)
                        .and_then(|e| (e.dom != usize::MAX).then_some(e.dom))
                        .and_then(|dom| self.doc.nodes[dom].attr("value"))
                        .unwrap_or("")
                        .to_string();
                    return Ok(json!({ "result": { "type": "string", "value": value } }));
                }
                if declaration.contains("outerHTML") || declaration.contains("innerHTML") {
                    return Ok(json!({ "result": { "type": "string", "value": "" } }));
                }
                Ok(json!({ "result": { "type": "undefined" } }))
            }

            "Runtime.getIsolateId" => Ok(json!({ "id": "m0" })),

            // ---- DOM -----------------------------------------------------
            "DOM.enable" | "DOM.disable" | "DOM.getDocument" => Ok(json!({})),

            "DOM.querySelector" => Ok(json!({ "nodeId": 0 })),

            "DOM.resolveNode" => {
                let object = self.store_object("HTMLElement", "element".into(), self.doc.body);
                Ok(json!({ "object": object }))
            }

            "DOM.describeNode" | "DOM.getOuterHTML" => Ok(json!({})),

            // ---- Accessibility -------------------------------------------
            "Accessibility.enable" => {
                out("Accessibility.loadComplete", json!({}));
                Ok(json!({}))
            }
            "Accessibility.disable" => Ok(json!({})),

            "Accessibility.getFullAXTree" | "Accessibility.getPartialAXTree" => {
                Ok(ax::build(&self.doc))
            }

            // ---- Network -------------------------------------------------
            "Network.enable" | "Network.disable" | "Network.clearBrowserCookies" => Ok(json!({})),

            "Network.getCookies" => Ok(json!({ "cookies": [] })),

            "Network.getAllCookies" => Ok(json!({ "cookies": [] })),

            "Network.setCookie" | "Network.deleteCookies" => Ok(json!({ "success": true })),

            // ---- Input ---------------------------------------------------
            "Input.dispatchMouseEvent"
            | "Input.dispatchKeyEvent"
            | "Input.insertText"
            | "Input.dispatchTouchEvent" => Ok(json!({})),

            // ---- Emulation -----------------------------------------------
            "Emulation.setDeviceMetricsOverride" => {
                let w = params.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = params.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                self.doc.viewport = (w, h);
                Ok(json!({}))
            }

            "Emulation.setUserAgentOverride"
            | "Emulation.setEmulatedMedia"
            | "Emulation.clearDeviceMetricsOverride" => Ok(json!({})),

            "Log.enable" | "Log.clear" | "Performance.enable" => Ok(json!({})),

            // Client-specific extensions simply acknowledge.
            "WebMCP.enable" | "WebMCP.disable" => Ok(json!({})),

            _ => Ok(json!({})),
        }
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
