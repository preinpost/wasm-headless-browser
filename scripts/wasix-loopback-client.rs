// Probe: from inside a sandbox, can we open an HTTP connection at all?
//
// Two uses, and the difference between them is the point:
//
//   loopback  run alongside `wasm-headless-browser` in the same sandbox and ask
//             it for /json/version. This is the "both sides inside one sandbox"
//             design reduced to its smallest form.
//   egress    point it at a public host to find out whether the sandbox can
//             reach the outside world. In a browser sandbox that requires a
//             WISP relay, because there is no direct network access.
//
// Build:
//   rustup run wasix rustc --target wasm32-wasmer-wasi -O \
//     scripts/wasix-loopback-client.rs -o /tmp/loopback-client.wasm
//
// Run:
//   wasmer run --net /tmp/loopback-client.wasm -- 127.0.0.1:9222
//   wasmer run --net /tmp/loopback-client.wasm -- example.com:80 / example.com
//   wasmer run --net /tmp/loopback-client.wasm -- cdp 127.0.0.1:9222 https://example.com/
//   wasmer run --net /tmp/loopback-client.wasm -- cdp 127.0.0.1:9222 https://example.com/ 10
//
// The `cdp` mode is the smallest CDP client: a WebSocket to the engine, then
// Target.createTarget, attachToTarget, Page.navigate and Runtime.evaluate. It
// shows that the real engine loads and scripts a page inside the sandbox.
//
// It retries, because the engine it is aimed at may still be starting.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread::sleep;
use std::time::Duration;

fn connect(addr: &str) -> Option<TcpStream> {
    for attempt in 1..=40 {
        match TcpStream::connect(addr) {
            Ok(s) => {
                println!("[client] connected on attempt {attempt}");
                return Some(s);
            }
            Err(e) => {
                if attempt == 1 || attempt % 10 == 0 {
                    println!("[client] attempt {attempt}: {e} (raw={:?})", e.raw_os_error());
                }
                sleep(Duration::from_millis(250));
            }
        }
    }
    None
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("cdp") {
        args.next();
        let addr = args.next().unwrap_or_else(|| "127.0.0.1:9222".to_string());
        let url = args.next().unwrap_or_else(|| "https://example.com/".to_string());
        let loads = args.next().map(|n| n.parse::<u32>().expect("load count must be a positive integer")).unwrap_or(1);
        assert!(loads > 0, "load count must be positive");
        cdp(&addr, &url, loads);
        return;
    }
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:9222".to_string());
    let path = args.next().unwrap_or_else(|| "/json/version".to_string());
    let host = args.next().unwrap_or_else(|| addr.clone());
    println!("[client] target {addr}{path} (Host: {host})");

    let Some(mut stream) = connect(&addr) else {
        println!("[client] RESULT: could not reach {addr}");
        return;
    };

    let request = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: wasix-probe\r\n\r\n");
    if let Err(e) = stream.write_all(request.as_bytes()) {
        println!("[client] write failed: {e}");
        return;
    }
    let _ = stream.flush();

    let mut response = Vec::new();
    match stream.read_to_end(&mut response) {
        Ok(_) => {
            let text = String::from_utf8_lossy(&response);
            let head = text.lines().next().unwrap_or("").trim();
            let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            println!("[client] RESULT: got {} bytes back", response.len());
            println!("[client] status: {head}");
            // Bodies here are small, so print them whole. Truncating made the
            // harness assert on a slice, which is a brittle thing to assert on.
            let trimmed = body.trim();
            if trimmed.len() <= 1200 {
                println!("[client] body ({} bytes): {trimmed}", trimmed.len());
            } else {
                println!("[client] body ({} bytes, first 1200):", trimmed.len());
                println!("{}", trimmed.chars().take(1200).collect::<String>());
            }
        }
        Err(e) => println!("[client] RESULT: read failed: {e}"),
    }
}

// ---- the CDP mode -----------------------------------------------------------

/// Opens the browser's WebSocket. The key is RFC 6455's sample nonce: the
/// engine does not care, and the accept header is not checked here.
fn websocket(addr: &str) -> Result<TcpStream, String> {
    let mut stream = connect(addr).ok_or(format!("could not reach {addr}"))?;
    let request = format!(
        "GET /devtools/browser HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    // The handshake response ends with a blank line; read it byte by byte so
    // no frame bytes are swallowed with it.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).map_err(|e| format!("handshake: {e}"))?;
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string();
    if !status.contains(" 101") {
        return Err(format!("handshake refused: {status}"));
    }
    Ok(stream)
}

/// One masked text frame, as a client must send.
fn send(stream: &mut TcpStream, text: &str) -> Result<(), String> {
    let payload = text.as_bytes();
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let mut frame = vec![0x81u8];
    match payload.len() {
        n if n < 126 => frame.push(0x80 | n as u8),
        n if n < 65536 => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame).map_err(|e| e.to_string())
}

/// The next whole text message, joining continuation frames.
fn receive(stream: &mut TcpStream) -> Result<String, String> {
    let mut message = Vec::new();
    loop {
        let mut head = [0u8; 2];
        stream.read_exact(&mut head).map_err(|e| format!("read: {e}"))?;
        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        let mut len = (head[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            stream.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            stream.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u64::from_be_bytes(b);
        }
        let mut payload = vec![0u8; len as usize];
        stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
        match opcode {
            0x8 => return Err("the engine closed the connection".into()),
            0x9 | 0xa => continue,
            _ => message.extend_from_slice(&payload),
        }
        if fin {
            return Ok(String::from_utf8_lossy(&message).into_owned());
        }
    }
}

/// Sends a command and waits for its response, skipping events. The engine
/// writes compact JSON, so the id is matched as text.
fn call(stream: &mut TcpStream, id: u32, session: Option<&str>, method: &str, params: &str) -> Result<String, String> {
    let session = session.map(|s| format!(",\"sessionId\":\"{s}\"")).unwrap_or_default();
    send(stream, &format!("{{\"id\":{id}{session},\"method\":\"{method}\",\"params\":{params}}}"))?;
    let (a, b) = (format!("\"id\":{id},"), format!("\"id\":{id}}}"));
    loop {
        let message = receive(stream)?;
        if message.contains(&a) || message.contains(&b) {
            if message.contains("\"error\":") {
                return Err(format!("{method}: {message}"));
            }
            return Ok(message);
        }
    }
}

/// The string value of `"key":"..."`, without unescaping.
fn field(message: &str, key: &str) -> Option<String> {
    let start = message.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let rest = &message[start..];
    let mut end = 0;
    let bytes = rest.as_bytes();
    while end < bytes.len() && !(bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\')) {
        end += 1;
    }
    Some(rest[..end].to_string())
}

fn cdp(addr: &str, url: &str, loads: u32) {
    println!("[cdp] target {addr}, page {url}");
    let run = || -> Result<String, String> {
        let mut ws = websocket(addr)?;
        println!("[cdp] websocket open");
        let created = call(&mut ws, 1, None, "Target.createTarget", "{\"url\":\"about:blank\"}")?;
        let target = field(&created, "targetId").ok_or(format!("no targetId in {created}"))?;
        let attached = call(&mut ws, 2, None, "Target.attachToTarget", &format!("{{\"targetId\":\"{target}\",\"flatten\":true}}"))?;
        let session = field(&attached, "sessionId").ok_or(format!("no sessionId in {attached}"))?;
        println!("[cdp] attached to {target}");
        let mut value = String::new();
        for load in 0..loads {
            let id = 3 + load * 2;
            let navigated = call(&mut ws, id, Some(&session), "Page.navigate", &format!("{{\"url\":\"{url}\"}}"))?;
            if let Some(error) = field(&navigated, "errorText") {
                return Err(format!("navigation failed: {error}"));
            }
            println!("[cdp] navigated ({}/{loads})", load + 1);
            // A page-built value, so a reply proves the page's JavaScript runs.
            let expression = "JSON.stringify({title: document.title, links: document.querySelectorAll('a').length, sum: [1,2,3].reduce((a, b) => a + b), loaded: globalThis.__loaded})";
            let evaluated = call(
                &mut ws,
                id + 1,
                Some(&session),
                "Runtime.evaluate",
                &format!("{{\"expression\":\"{}\",\"returnByValue\":true}}", expression.replace('"', "\\\"")),
            )?;
            value = field(&evaluated, "value").ok_or(format!("no value in {evaluated}"))?;
            println!("[cdp] load value: {value}");
        }
        Ok(value)
    };
    match run() {
        Ok(value) => {
            println!("[cdp] value: {value}");
            println!("[cdp] RESULT: page loaded");
        }
        Err(e) => {
            println!("[cdp] RESULT: failed: {e}");
            std::process::exit(1);
        }
    }
}
