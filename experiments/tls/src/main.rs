//! Does a TLS client run inside WASIX?
//!
//! rustls with the ring provider, over a `std::net::TcpStream`, trusting the
//! Mozilla roots from `webpki-roots`. Fetches `/` and prints the status line,
//! the negotiated version and suite, and how many bytes came back.
//!
//! Build and run:
//!
//!   cargo run --release -- example.com
//!   cargo wasix build --release
//!   wasmer run --net target/wasm32-wasmer-wasi/release/tls-probe.wasm -- example.com

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

fn step(msg: &str) {
    println!("[tls] {msg}");
    let _ = std::io::stdout().flush();
}

fn main() {
    let host = std::env::args().nth(1).unwrap_or_else(|| "example.com".to_string());
    let port: u16 = std::env::args().nth(2).and_then(|p| p.parse().ok()).unwrap_or(443);
    if let Err(e) = run(&host, port) {
        step(&format!("FAIL: {e}"));
        std::process::exit(1);
    }
}

fn run(host: &str, port: u16) -> Result<(), String> {
    let started = Instant::now();

    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("protocol versions: {e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    step(&format!("config ready, {} roots", webpki_roots::TLS_SERVER_ROOTS.len()));

    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("server name {host}: {e}"))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("client connection: {e}"))?;

    let mut sock = TcpStream::connect((host, port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;
    step(&format!("tcp connected to {host}:{port}"));

    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: tls-probe\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).map_err(|e| format!("handshake or write: {e}"))?;
    step(&format!(
        "handshake done: {:?} {:?}",
        tls.conn.protocol_version(),
        tls.conn.negotiated_cipher_suite().map(|s| s.suite())
    ));

    let mut body = Vec::new();
    match tls.read_to_end(&mut body) {
        Ok(_) => {}
        // Plenty of servers close without close_notify; what was read is fine.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(format!("read: {e}")),
    }
    let text = String::from_utf8_lossy(&body);
    step(&format!("status: {}", text.lines().next().unwrap_or("")));
    step(&format!("{} bytes in {:?}", body.len(), started.elapsed()));
    Ok(())
}
