//! Chrome DevTools Protocol semantics: targets, sessions, and method dispatch.
//!
//! This crate owns the protocol shape and nothing else. It never touches the
//! network and never parses HTML; it asks an [`Engine`] for the answers.
//!
//! The engine seam is deliberate. M0 ships `engine-fake`, which recognises a
//! fixed set of expressions so the client plumbing can be proven. A real
//! engine later implements the same trait.

use std::collections::HashMap;

pub mod ids;

use serde_json::{json, Value};

/// A CDP error response.
#[derive(Debug, Clone)]
pub struct CdpError {
    pub code: i64,
    pub message: String,
}

impl CdpError {
    pub fn internal(message: impl Into<String>) -> Self {
        CdpError { code: -32000, message: message.into() }
    }

    pub fn method_not_found(method: &str) -> Self {
        CdpError { code: -32601, message: format!("'{method}' wasn't found") }
    }
}

/// Emits a protocol event at browser scope. The session id is `None` for
/// browser-level events and `Some` for events belonging to an attached target.
pub type BrowserEmit<'a> = dyn FnMut(Option<&str>, &str, Value) + 'a;

/// Emits a protocol event inside one attached session.
pub type EngineEmit<'a> = dyn FnMut(&str, Value) + 'a;

/// What a target advertises through `Target.getTargets`.
#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub target_id: String,
    pub target_type: String,
    pub title: String,
    pub url: String,
    pub attached: bool,
}

impl TargetInfo {
    pub fn new(target_id: impl Into<String>) -> Self {
        TargetInfo {
            target_id: target_id.into(),
            target_type: "page".into(),
            title: String::new(),
            url: "about:blank".into(),
            attached: false,
        }
    }

    pub fn to_json(&self, browser_context_id: &str) -> Value {
        json!({
            "targetId": self.target_id,
            "type": self.target_type,
            "title": self.title,
            "url": self.url,
            "attached": self.attached,
            "canAccessOpener": false,
            "browserContextId": browser_context_id,
        })
    }
}

/// A thing that can answer CDP commands for one page.
pub trait Engine: Send {
    fn info(&self) -> TargetInfo;

    /// Navigates the page. The engine emits its own lifecycle events.
    fn navigate(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError>;

    /// Called once when a client attaches. Engines publish their initial
    /// state here, such as `Runtime.executionContextCreated`.
    fn attach(&mut self, _out: &mut EngineEmit) {}

    fn dispatch(&mut self, method: &str, params: &Value, out: &mut EngineEmit) -> Result<Value, CdpError>;

    fn close(&mut self) {}
}

/// Builds an engine for a freshly allocated target id.
pub type EngineFactory = Box<dyn Fn(&str) -> Box<dyn Engine> + Send + Sync>;

struct Target {
    engine: Box<dyn Engine>,
}

/// The browser-level CDP endpoint: one client socket, many attached targets.
pub struct Browser {
    pub browser_id: String,
    pub browser_context_id: String,
    pub product: String,
    pub user_agent: String,
    factory: EngineFactory,
    targets: Vec<Target>,
    sessions: HashMap<String, String>,
    counter: u64,
}

impl Browser {
    pub fn new(factory: EngineFactory) -> Self {
        let mut counter = ids::seed();
        let browser_id = ids::token(&mut counter);

        let mut browser = Browser {
            browser_id,
            browser_context_id: ids::token(&mut counter),
            product: "Chrome/140.0.0.0".into(),
            user_agent: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                         (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36"
                .into(),
            factory,
            targets: Vec::new(),
            sessions: HashMap::new(),
            counter,
        };
        browser.add_target();
        browser
    }

    /// Creates a new page target and returns its id.
    pub fn add_target(&mut self) -> String {
        let target_id = ids::token(&mut self.counter);
        let engine = (self.factory)(&target_id);
        self.targets.push(Target { engine });
        target_id
    }

    fn index_of(&self, target_id: &str) -> Option<usize> {
        self.targets.iter().position(|t| t.engine.info().target_id == target_id)
    }

    fn session_for(&self, target_id: &str) -> Option<String> {
        self.sessions.iter().find(|(_, t)| t.as_str() == target_id).map(|(s, _)| s.clone())
    }

    /// Attaches a client to a target, reusing an existing session if one
    /// already exists.
    fn attach(&mut self, index: usize, out: &mut BrowserEmit) -> String {
        let target_id = self.targets[index].engine.info().target_id.clone();
        if let Some(existing) = self.session_for(&target_id) {
            return existing;
        }

        let session_id = ids::token(&mut self.counter);
        self.sessions.insert(session_id.clone(), target_id);

        let mut emitted: Vec<(String, Value)> = Vec::new();
        {
            let mut sink = |m: &str, p: Value| emitted.push((m.to_string(), p));
            self.targets[index].engine.attach(&mut sink);
        }
        for (method, params) in emitted {
            out(Some(&session_id), &method, params);
        }
        session_id
    }

    /// Handles one decoded CDP message. Returns the response, if the message
    /// expected one. Events go out through `out` before the response.
    pub fn handle(&mut self, msg: &Value, out: &mut BrowserEmit) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let session = msg.get("sessionId").and_then(|s| s.as_str()).map(str::to_string);

        let result = match &session {
            Some(session_id) => self.handle_in_session(session_id, &method, &params, out),
            None => self.handle_at_browser(&method, &params, out),
        };

        let id = id?;
        Some(match (result, session) {
            (Ok(value), Some(s)) => json!({ "id": id, "result": value, "sessionId": s }),
            (Ok(value), None) => json!({ "id": id, "result": value }),
            (Err(e), Some(s)) => json!({
                "id": id,
                "error": { "code": e.code, "message": e.message },
                "sessionId": s,
            }),
            (Err(e), None) => json!({ "id": id, "error": { "code": e.code, "message": e.message } }),
        })
    }

    fn handle_in_session(
        &mut self,
        session_id: &str,
        method: &str,
        params: &Value,
        out: &mut BrowserEmit,
    ) -> Result<Value, CdpError> {
        let target_id = self
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| CdpError::internal(format!("unknown session {session_id}")))?;
        let index = self
            .index_of(&target_id)
            .ok_or_else(|| CdpError::internal("session target is gone"))?;

        let mut emitted: Vec<(String, Value)> = Vec::new();
        let result = {
            let mut sink = |m: &str, p: Value| emitted.push((m.to_string(), p));
            self.targets[index].engine.dispatch(method, params, &mut sink)
        };
        for (event_method, event_params) in emitted {
            out(Some(session_id), &event_method, event_params);
        }
        result
    }

    fn handle_at_browser(
        &mut self,
        method: &str,
        params: &Value,
        out: &mut BrowserEmit,
    ) -> Result<Value, CdpError> {
        match method {
            "Browser.getVersion" => Ok(json!({
                "protocolVersion": "1.3",
                "product": self.product,
                "revision": "@wasm-headless-browser",
                "userAgent": self.user_agent,
                "jsVersion": "0.0.0",
            })),

            "Target.setDiscoverTargets" => {
                let ctx = self.browser_context_id.clone();
                let infos: Vec<Value> =
                    self.targets.iter().map(|t| t.engine.info().to_json(&ctx)).collect();
                for info in infos {
                    out(None, "Target.targetCreated", json!({ "targetInfo": info }));
                }
                Ok(json!({}))
            }

            "Target.setAutoAttach" => {
                let auto_attach =
                    params.get("autoAttach").and_then(|v| v.as_bool()).unwrap_or(true);
                let wait_for_debugger = params
                    .get("waitForDebuggerOnStart")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if auto_attach {
                    for index in 0..self.targets.len() {
                        let session_id = self.attach(index, out);
                        let ctx = self.browser_context_id.clone();
                        let mut info = self.targets[index].engine.info().to_json(&ctx);
                        if let Some(obj) = info.as_object_mut() {
                            obj.insert("attached".into(), json!(true));
                        }
                        out(
                            None,
                            "Target.attachedToTarget",
                            json!({
                                "sessionId": session_id,
                                "targetInfo": info,
                                "waitingForDebugger": wait_for_debugger,
                            }),
                        );
                    }
                }
                Ok(json!({}))
            }

            "Target.getTargets" => {
                let ctx = self.browser_context_id.clone();
                let infos: Vec<Value> =
                    self.targets.iter().map(|t| t.engine.info().to_json(&ctx)).collect();
                Ok(json!({ "targetInfos": infos }))
            }

            "Target.attachToTarget" => {
                let target_id =
                    params.get("targetId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let index = self
                    .index_of(&target_id)
                    .ok_or_else(|| CdpError::internal(format!("no such target {target_id}")))?;
                let session_id = self.attach(index, out);
                Ok(json!({ "sessionId": session_id }))
            }

            "Target.createTarget" => {
                let target_id = self.add_target();
                if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                    if let Some(index) = self.index_of(&target_id) {
                        let mut sink = |_: &str, _: Value| {};
                        if let Err(e) = self.targets[index].engine.navigate(url, &mut sink) {
                            eprintln!("createTarget navigate failed: {}", e.message);
                        }
                    }
                }
                let ctx = self.browser_context_id.clone();
                if let Some(index) = self.index_of(&target_id) {
                    let info = self.targets[index].engine.info().to_json(&ctx);
                    out(None, "Target.targetCreated", json!({ "targetInfo": info }));
                }
                Ok(json!({ "targetId": target_id }))
            }

            "Target.closeTarget" => {
                if let Some(target_id) = params.get("targetId").and_then(|v| v.as_str()) {
                    if let Some(index) = self.index_of(target_id) {
                        self.targets[index].engine.close();
                        self.targets.remove(index);
                        self.sessions.retain(|_, t| t != target_id);
                        out(None, "Target.targetDestroyed", json!({ "targetId": target_id }));
                    }
                }
                Ok(json!({ "success": true }))
            }

            "Target.activateTarget" | "Target.detachFromTarget" => Ok(json!({})),

            "Target.getBrowserContexts" => Ok(json!({ "browserContextIds": [] })),

            "Schema.getDomains" => Ok(json!({ "domains": [] })),

            "Browser.close" => Ok(json!({})),

            // Unknown methods answer with an empty result rather than an
            // error. Clients probe optional domains constantly, and an error
            // makes them abort more often than a blank answer does.
            _ => Ok(json!({})),
        }
    }
}
