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
//
// It retries, because the engine it is aimed at may still be starting.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:9222".to_string());
    let path = args.next().unwrap_or_else(|| "/json/version".to_string());
    let host = args.next().unwrap_or_else(|| addr.clone());
    println!("[client] target {addr}{path} (Host: {host})");

    let mut stream = None;
    for attempt in 1..=40 {
        match TcpStream::connect(&addr) {
            Ok(s) => {
                println!("[client] connected on attempt {attempt}");
                stream = Some(s);
                break;
            }
            Err(e) => {
                if attempt == 1 || attempt % 10 == 0 {
                    println!("[client] attempt {attempt}: {e} (raw={:?})", e.raw_os_error());
                }
                sleep(Duration::from_millis(250));
            }
        }
    }

    let Some(mut stream) = stream else {
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
