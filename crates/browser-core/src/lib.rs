//! Chrome DevTools Protocol semantics: targets, sessions, and method dispatch.
//!
//! This crate owns the protocol shape and nothing else. It never touches the
//! network and never parses HTML; it asks an [`Engine`] for the answers.
//!
//! The engine seam is deliberate. M0 ships `engine-fake`, which recognises a
//! fixed set of expressions so the client plumbing can be proven. A real
//! engine later implements the same trait.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

/// Sends an event inside one session at once, from any thread, even while a
/// command is still running. Returns `false` when no client is connected to
/// receive it, so nothing should wait for an answer.
pub type LiveEmit = Arc<dyn Fn(&str, Value) -> bool + Send + Sync>;

/// Writes a whole message to the connected client. `false` when it could not.
pub type Sink = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// Commands an engine answers outside its command queue.
///
/// Commands are otherwise handled one at a time, each to the end, and a page
/// load is one command. Some answer what that command is waiting for:
/// `Fetch.continueRequest` releases a request the load paused. Queued behind
/// the load, it would never run.
pub trait Concurrent: Send + Sync {
    /// The answer, or `None` for a command that is not one of these, which
    /// then goes through [`Engine::dispatch`].
    fn handle(&self, method: &str, params: &Value) -> Option<Result<Value, CdpError>>;

    /// The client went away. Nothing may wait for it any longer.
    fn disconnected(&self) {}
}

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

    /// Called once when a client attaches, with a way to send events at
    /// once. An engine with commands to answer outside its queue returns
    /// them here.
    fn concurrent(&mut self, _emit: LiveEmit) -> Option<Arc<dyn Concurrent>> {
        None
    }

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
    side: Arc<SideDoor>,
}

/// What answers [`Concurrent`] commands and carries [`LiveEmit`] events.
/// It has locks of its own: it is used while the [`Browser`] is locked by
/// the command that is waiting.
#[derive(Default)]
pub struct SideDoor {
    handlers: Mutex<HashMap<String, Arc<dyn Concurrent>>>,
    /// The connected client: its number, and how to write to it.
    sink: Mutex<Option<(u64, Sink)>>,
    connections: Mutex<u64>,
}

impl SideDoor {
    /// Starts sending live events to a client. Returns the number to
    /// [`disconnect`](Self::disconnect) it with. A later client takes over.
    pub fn connect(&self, sink: Sink) -> u64 {
        let id = {
            let mut connections = self.connections.lock().unwrap_or_else(|p| p.into_inner());
            *connections += 1;
            *connections
        };
        *self.sink.lock().unwrap_or_else(|p| p.into_inner()) = Some((id, sink));
        id
    }

    /// The client numbered `id` went away. If it was the one receiving live
    /// events, whatever waits on it is told.
    pub fn disconnect(&self, id: u64) {
        let current = {
            let mut sink = self.sink.lock().unwrap_or_else(|p| p.into_inner());
            if sink.as_ref().map(|(i, _)| *i) == Some(id) {
                *sink = None;
                true
            } else {
                false
            }
        };
        if current {
            let handlers: Vec<_> = self.handlers().values().cloned().collect();
            for handler in handlers {
                handler.disconnected();
            }
        }
    }

    /// The response to `msg`, when it is a command answered here.
    pub fn handle(&self, msg: &Value) -> Option<Value> {
        let session = msg.get("sessionId")?.as_str()?;
        let method = msg.get("method")?.as_str()?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let handler = self.handlers().get(session).cloned()?;
        let result = handler.handle(method, &params)?;
        trace(method, &params, &result);
        Some(response(msg.get("id").cloned()?, Some(session), result))
    }

    fn send(&self, session: &str, method: &str, params: Value) -> bool {
        let sink = self.sink.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|(_, s)| Arc::clone(s));
        match sink {
            Some(sink) => sink(&json!({ "method": method, "params": params, "sessionId": session })),
            None => false,
        }
    }

    fn handlers(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<dyn Concurrent>>> {
        self.handlers.lock().unwrap_or_else(|p| p.into_inner())
    }
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
            side: Arc::new(SideDoor::default()),
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

    /// Where commands are answered that must not queue behind a running one.
    pub fn side_door(&self) -> Arc<SideDoor> {
        Arc::clone(&self.side)
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

        let side = Arc::clone(&self.side);
        let session = session_id.clone();
        let live: LiveEmit = Arc::new(move |method: &str, params: Value| side.send(&session, method, params));
        if let Some(handler) = self.targets[index].engine.concurrent(live) {
            self.side.handlers().insert(session_id.clone(), handler);
        }
        session_id
    }

    /// Handles one decoded CDP message.
    ///
    /// With `WHB_TRACE_CDP` set, every method, its parameters and its result
    /// go to stderr, truncated. That is how to find out what a client sends.
    /// What is typed is replaced by its length, because it is how passwords
    /// arrive. Other methods are logged as sent, so a client that puts a
    /// secret in an expression still exposes it there. Returns the response, if the message
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

        trace(&method, &params, &result);
        Some(response(id?, session.as_deref(), result))
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
                        let closed: Vec<String> =
                            self.sessions.iter().filter(|(_, t)| *t == target_id).map(|(s, _)| s.clone()).collect();
                        for session in closed {
                            if let Some(handler) = self.side.handlers().remove(&session) {
                                handler.disconnected();
                            }
                        }
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

/// A command's response, inside its session if it came in one.
fn response(id: Value, session: Option<&str>, result: Result<Value, CdpError>) -> Value {
    let mut response = match result {
        Ok(value) => json!({ "id": id, "result": value }),
        Err(e) => json!({ "id": id, "error": { "code": e.code, "message": e.message } }),
    };
    if let Some(session) = session {
        response["sessionId"] = json!(session);
    }
    response
}

fn trace(method: &str, params: &Value, result: &Result<Value, CdpError>) {
    if trace_enabled() {
        let shown = |v: &Value| v.to_string().chars().take(300).collect::<String>();
        let logged = redact_typing(method, params);
        match result {
            Ok(value) => eprintln!("cdp {method} {} -> {}", shown(&logged), shown(value)),
            Err(e) => eprintln!("cdp {method} {} -> error {}", shown(&logged), e.message),
        }
    }
}

fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("WHB_TRACE_CDP").is_some())
}

/// `params` with typed text replaced by its length: `Input.insertText` and
/// the text a key event carries.
fn redact_typing(method: &str, params: &Value) -> Value {
    let fields: &[&str] = match method {
        "Input.insertText" | "Input.imeSetComposition" => &["text"],
        "Input.dispatchKeyEvent" => &["text", "unmodifiedText", "key", "code", "keyIdentifier"],
        _ => return params.clone(),
    };
    let mut logged = params.clone();
    for field in fields {
        if let Some(text) = logged.get(*field).and_then(|v| v.as_str()) {
            let length = text.chars().count();
            logged[*field] = json!(format!("[{length} redacted]"));
        }
    }
    logged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine whose `Wait` command waits for `Release`, which only its
    /// concurrent handler answers, and which says so with a live event.
    struct Waiting {
        release: std::sync::mpsc::Receiver<()>,
        handler: Arc<Release>,
    }

    struct Release {
        tx: Mutex<std::sync::mpsc::Sender<()>>,
        emit: Mutex<Option<LiveEmit>>,
    }

    impl Concurrent for Release {
        fn handle(&self, method: &str, _: &Value) -> Option<Result<Value, CdpError>> {
            (method == "Test.release").then(|| {
                let _ = self.tx.lock().unwrap().send(());
                Ok(json!({ "released": true }))
            })
        }
    }

    impl Engine for Waiting {
        fn info(&self) -> TargetInfo {
            TargetInfo::new("only")
        }
        fn navigate(&mut self, _: &str, _: &mut EngineEmit) -> Result<(), CdpError> {
            Ok(())
        }
        fn dispatch(&mut self, method: &str, _: &Value, _: &mut EngineEmit) -> Result<Value, CdpError> {
            if method == "Test.wait" {
                let emit = self.handler.emit.lock().unwrap().clone().unwrap();
                assert!(emit("Test.waiting", json!({})));
                self.release.recv().unwrap();
            }
            Ok(json!({ "done": method }))
        }
        fn concurrent(&mut self, emit: LiveEmit) -> Option<Arc<dyn Concurrent>> {
            *self.handler.emit.lock().unwrap() = Some(emit);
            Some(Arc::clone(&self.handler) as Arc<dyn Concurrent>)
        }
    }

    #[test]
    fn the_side_door_answers_what_a_running_command_waits_for() {
        let browser = Arc::new(Mutex::new(Browser::new(Box::new(|_: &str| {
            let (tx, release) = std::sync::mpsc::channel();
            let handler = Arc::new(Release { tx: Mutex::new(tx), emit: Mutex::new(None) });
            Box::new(Waiting { release, handler }) as Box<dyn Engine>
        }))));
        let side = browser.lock().unwrap().side_door();
        let (sent, live) = std::sync::mpsc::channel::<Value>();
        let sent = Mutex::new(sent);
        side.connect(Arc::new(move |message: &Value| sent.lock().unwrap().send(message.clone()).is_ok()));

        let target = browser.lock().unwrap().targets[0].engine.info().target_id;
        let mut ignore = |_: Option<&str>, _: &str, _: Value| {};
        let attached = browser
            .lock()
            .unwrap()
            .handle(&json!({ "id": 1, "method": "Target.attachToTarget", "params": { "targetId": target } }), &mut ignore)
            .unwrap();
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();

        // Not a side-door command: it queues as usual.
        assert!(side.handle(&json!({ "id": 2, "method": "Test.wait", "sessionId": session })).is_none());
        let running = {
            let (browser, session) = (Arc::clone(&browser), session.clone());
            std::thread::spawn(move || {
                let mut ignore = |_: Option<&str>, _: &str, _: Value| {};
                browser.lock().unwrap().handle(&json!({ "id": 2, "method": "Test.wait", "sessionId": session }), &mut ignore)
            })
        };
        // The live event arrives while the command still holds the browser.
        let waiting = live.recv().unwrap();
        assert_eq!(waiting, json!({ "method": "Test.waiting", "params": {}, "sessionId": session }));
        assert!(browser.try_lock().is_err());

        let released = side.handle(&json!({ "id": 3, "method": "Test.release", "sessionId": session })).unwrap();
        assert_eq!(released, json!({ "id": 3, "result": { "released": true }, "sessionId": session }));
        assert_eq!(running.join().unwrap().unwrap()["result"]["done"], "Test.wait");
    }

    #[test]
    fn tracing_never_shows_what_was_typed() {
        let typed = redact_typing("Input.insertText", &json!({ "text": "hunter2" }));
        assert_eq!(typed, json!({ "text": "[7 redacted]" }));
        let key = redact_typing(
            "Input.dispatchKeyEvent",
            &json!({ "type": "keyDown", "key": "h", "text": "h", "code": "KeyH" }),
        );
        assert!(!key.to_string().contains("\"h\""), "{key}");
        assert!(!key.to_string().contains("KeyH"), "{key}");
        assert_eq!(key["type"], "keyDown");
        let other = json!({ "expression": "1" });
        assert_eq!(redact_typing("Runtime.evaluate", &other), other);
    }
}
