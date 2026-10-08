//! `wasm-headless-browser` — a CDP-speaking headless browser.
//!
//! M0 serves the discovery endpoints and a browser-level WebSocket. The page
//! it hands out comes from `engine-fake`, so this binary proves the protocol
//! path end to end before any HTML is parsed.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use cdp_wire::{serve, Connection, Handler, Request, Response};
use browser_core::Browser;
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

fn main() -> io::Result<()> {
    let addr = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| "127.0.0.1:9222".to_string());

    let listener = TcpListener::bind(&addr)?;
    let local: SocketAddr = listener.local_addr()?;

    let handler = CdpHandler { browser: Arc::new(Mutex::new(Browser::new(FakeEngine::factory()))) };
    let ws_url = {
        let browser = handler.browser.lock().unwrap();
        format!("ws://{local}/devtools/browser/{}", browser.browser_id)
    };

    println!("wasm-headless-browser (M0, fake engine)");
    println!("  discovery : http://{local}/json/version");
    println!("  websocket : {ws_url}");
    println!();
    println!("  agent-browser --cdp {local} open https://example.com");
    println!("  agent-browser --cdp {local} snapshot");
    // Flush explicitly: when stdout is a pipe rather than a terminal it may be
    // block-buffered, and a caller waiting on this banner would hang.
    io::stdout().flush()?;

    serve(listener, handler)
}
