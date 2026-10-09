//! A scripted HTTP server for tests: one canned response per connection.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::fetch::find_header_end;

/// Answers one connection per canned response, in order, and hands back
/// each request as it arrived.
pub fn serve_script(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let _ = tx.send(read_request(&mut stream));
            let _ = stream.write_all(response.replace("{port}", &port.to_string()).as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

/// Reads a request head and, if it declares one, its body.
pub fn read_request(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some((_, body_start)) = find_header_end(&raw) {
            let head = String::from_utf8_lossy(&raw[..body_start]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if raw.len() >= body_start + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}
