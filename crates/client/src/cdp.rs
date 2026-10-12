//! One CDP connection to the engine: a WebSocket over a blocking `TcpStream`.
//!
//! Blocking std sockets, not async I/O, on purpose. Under Wasmer, tokio's
//! socket reads fail where blocking ones work (`experiments/agent-browser`),
//! and a command-line client waits for one answer at a time anyway.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use cdp_wire::ws;
use serde_json::{json, Value};

/// Where the engine listens: `host:port`, and the WebSocket path when the
/// caller named one.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub addr: String,
    pub path: Option<String>,
}

impl Endpoint {
    /// From `--cdp`: a bare port, `host:port`, `http://host:port`, or a
    /// `ws://` URL, as agent-browser accepts them.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("--cdp is empty".into());
        }
        if spec.chars().all(|c| c.is_ascii_digit()) {
            return Ok(Endpoint { addr: format!("127.0.0.1:{spec}"), path: None });
        }
        if spec.starts_with("https://") || spec.starts_with("wss://") {
            return Err(format!("{spec}: the engine speaks plain HTTP and WebSocket, not TLS"));
        }
        let (rest, websocket) = match (spec.strip_prefix("ws://"), spec.strip_prefix("http://")) {
            (Some(rest), _) => (rest, true),
            (_, Some(rest)) => (rest, false),
            _ => (spec, false),
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        };
        if authority.is_empty() || !authority.contains(':') {
            return Err(format!("{spec}: expected host:port"));
        }
        let path = (websocket && !path.is_empty()).then(|| path.to_string());
        Ok(Endpoint { addr: authority.to_string(), path })
    }
}

/// How long to wait for any one answer. Page loads are one command in the
/// engine, so this bounds a whole navigation.
fn timeout() -> Duration {
    let ms = std::env::var("WHB_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000u64);
    Duration::from_millis(ms.max(1))
}

fn reach(addr: &str) -> Result<TcpStream, String> {
    let stream = TcpStream::connect(addr).map_err(|e| format!("cannot reach the engine at {addr}: {e}"))?;
    // Best effort: a runtime without read timeouts still works, it only
    // cannot give up on a silent engine.
    let _ = stream.set_read_timeout(Some(timeout()));
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// The WebSocket path from `/json/version`, as a client finds Chrome's.
fn discover_path(addr: &str) -> Result<String, String> {
    let mut stream = reach(addr)?;
    write!(stream, "GET /json/version HTTP/1.0\r\nHost: {addr}\r\n\r\n").map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed /json/version response")?;
    if !head.lines().next().is_some_and(|status| status.contains(" 200")) {
        return Err(format!("/json/version: {}", head.lines().next().unwrap_or("")));
    }
    let version: Value = serde_json::from_str(body).map_err(|e| format!("/json/version: {e}"))?;
    let url = version["webSocketDebuggerUrl"].as_str().ok_or("/json/version has no webSocketDebuggerUrl")?;
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    Ok(after_scheme.find('/').map(|at| after_scheme[at..].to_string()).unwrap_or_else(|| "/".into()))
}

/// A small xorshift generator for WebSocket keys and masks. They need to
/// vary, not to be secret: this socket never leaves the machine.
struct Noise(u64);

impl Noise {
    fn new() -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
        Noise(nanos ^ (std::process::id() as u64).rotate_left(32) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn mask(&mut self) -> [u8; 4] {
        (self.next() as u32).to_be_bytes()
    }
}

pub struct Cdp {
    stream: TcpStream,
    next_id: u64,
    noise: Noise,
}

impl Cdp {
    pub fn connect(endpoint: &Endpoint) -> Result<Self, String> {
        let path = match &endpoint.path {
            Some(path) => path.clone(),
            None => discover_path(&endpoint.addr).unwrap_or_else(|_| "/devtools/browser".into()),
        };
        let mut stream = reach(&endpoint.addr)?;
        let mut noise = Noise::new();
        let mut key_bytes = [0u8; 16];
        key_bytes[..8].copy_from_slice(&noise.next().to_be_bytes());
        key_bytes[8..].copy_from_slice(&noise.next().to_be_bytes());
        let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            endpoint.addr
        )
        .map_err(|e| format!("WebSocket handshake: {e}"))?;

        // Byte by byte, so no frame that follows the response is consumed.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 16 * 1024 {
                return Err("WebSocket handshake: response too long".into());
            }
            stream.read_exact(&mut byte).map_err(|e| format!("WebSocket handshake: {e}"))?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        let status = head.lines().next().unwrap_or("");
        if !status.contains(" 101") {
            return Err(format!("WebSocket handshake refused: {status}"));
        }
        let accept = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
            .map(|(_, value)| value.trim().to_string());
        if accept.as_deref() != Some(ws::accept_key(&key).as_str()) {
            return Err("WebSocket handshake: wrong Sec-WebSocket-Accept".into());
        }
        Ok(Cdp { stream, next_id: 1, noise })
    }

    /// Sends a command and returns its result, skipping events and the
    /// answers to anything else.
    pub fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.send(&message.to_string()).map_err(|e| format!("{method}: {e}"))?;
        loop {
            let text = self.receive().map_err(|e| format!("{method}: {e}"))?;
            let Ok(reply) = serde_json::from_str::<Value>(&text) else { continue };
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = reply.get("error") {
                let detail = error.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| error.to_string());
                return Err(format!("{method}: {detail}"));
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn send(&mut self, text: &str) -> io::Result<()> {
        let mask = self.noise.mask();
        ws::write_masked_frame(&mut self.stream, ws::opcode::TEXT, text.as_bytes(), mask)
    }

    /// The next whole text message, joining fragments and answering pings.
    fn receive(&mut self) -> Result<String, String> {
        let mut message = Vec::new();
        loop {
            let frame = match ws::read_frame(&mut self.stream) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Err("the engine closed the connection".into()),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Err(format!("no answer within {} ms (WHB_TIMEOUT_MS)", timeout().as_millis()));
                }
                Err(e) => return Err(e.to_string()),
            };
            match frame.opcode {
                ws::opcode::PING => {
                    let mask = self.noise.mask();
                    ws::write_masked_frame(&mut self.stream, ws::opcode::PONG, &frame.payload, mask).map_err(|e| e.to_string())?;
                    continue;
                }
                ws::opcode::PONG => continue,
                ws::opcode::CLOSE => return Err("the engine closed the connection".into()),
                _ => message.extend_from_slice(&frame.payload),
            }
            if frame.fin {
                return Ok(String::from_utf8_lossy(&message).into_owned());
            }
        }
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        let mask = self.noise.mask();
        let _ = ws::write_masked_frame(&mut self.stream, ws::opcode::CLOSE, &[], mask);
    }
}

#[cfg(test)]
mod tests {
    use super::Endpoint;

    #[test]
    fn endpoints_parse_as_agent_browser_accepts_them() {
        let at = |addr: &str, path: Option<&str>| Endpoint { addr: addr.into(), path: path.map(str::to_string) };
        assert_eq!(Endpoint::parse("9222").unwrap(), at("127.0.0.1:9222", None));
        assert_eq!(Endpoint::parse("127.0.0.1:9333").unwrap(), at("127.0.0.1:9333", None));
        assert_eq!(Endpoint::parse("http://127.0.0.1:9222").unwrap(), at("127.0.0.1:9222", None));
        assert_eq!(Endpoint::parse("http://localhost:9222/").unwrap(), at("localhost:9222", None));
        assert_eq!(
            Endpoint::parse("ws://127.0.0.1:9222/devtools/browser/AB").unwrap(),
            at("127.0.0.1:9222", Some("/devtools/browser/AB"))
        );
        assert!(Endpoint::parse("https://127.0.0.1:9222").is_err());
        assert!(Endpoint::parse("localhost").is_err());
        assert!(Endpoint::parse("").is_err());
    }
}
