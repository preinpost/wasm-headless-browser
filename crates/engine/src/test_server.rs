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

/// Serves `pages`, `(path, content type, body)`, each connection on a thread
/// of its own, and answers none of the paths in `together` until every one
/// of them has been asked for. A client that fetches them one at a time asks
/// for the first and waits; after three seconds it gets a 503 instead. That
/// is how a test tells parallel fetching from sequential.
pub fn serve_together(pages: Vec<(&'static str, &'static str, String)>, together: Vec<&'static str>) -> String {
    use std::collections::HashSet;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let pages = Arc::new(pages);
    let together = Arc::new(together);
    let seen = Arc::new((Mutex::new(HashSet::<String>::new()), Condvar::new()));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let (pages, together, seen) = (Arc::clone(&pages), Arc::clone(&together), Arc::clone(&seen));
            std::thread::spawn(move || {
                let request = read_request(&mut stream);
                let path = request.split(' ').nth(1).unwrap_or("").to_string();
                let held = together.iter().any(|p| *p == path);
                let mut all_seen = true;
                if held {
                    let (lock, arrived) = &*seen;
                    let mut set = lock.lock().unwrap();
                    set.insert(path.clone());
                    arrived.notify_all();
                    let (set, _) = arrived
                        .wait_timeout_while(set, Duration::from_secs(3), |s| together.iter().any(|p| !s.contains(*p)))
                        .unwrap();
                    all_seen = together.iter().all(|p| set.contains(*p));
                }
                let page = pages.iter().find(|(p, _, _)| *p == path);
                let response = match page {
                    Some((_, kind, body)) if all_seen => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    Some(_) => "HTTP/1.1 503 Alone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}
