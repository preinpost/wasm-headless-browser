// Probe: from inside a sandbox, can we reach the engine listening on loopback?
//
// Run alongside `wasm-headless-browser` in the same sandbox. It is the whole
// "both sides inside one sandbox" design reduced to its smallest form: open a
// TCP connection to 127.0.0.1, speak HTTP, print what came back.
//
// Build:
//   rustup run wasix rustc --target wasm32-wasmer-wasi -O \
//     scripts/wasix-loopback-client.rs -o /tmp/loopback-client.wasm
//
// Run:
//   wasmer run --net /tmp/loopback-client.wasm -- 127.0.0.1:9222
//
// It retries, because the engine it is aimed at may still be starting.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:9222".to_string());
    println!("[client] target {addr}");

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

    let request = format!("GET /json/version HTTP/1.0\r\nHost: {addr}\r\n\r\n");
    if let Err(e) = stream.write_all(request.as_bytes()) {
        println!("[client] write failed: {e}");
        return;
    }
    let _ = stream.flush();

    let mut response = Vec::new();
    match stream.read_to_end(&mut response) {
        Ok(_) => {
            let text = String::from_utf8_lossy(&response);
            let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&text);
            println!("[client] RESULT: got {} bytes back", response.len());
            println!("[client] body: {}", body.trim());
        }
        Err(e) => println!("[client] RESULT: read failed: {e}"),
    }
}
