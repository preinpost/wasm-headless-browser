// Build as a cargo project with `tokio = { version = "1", features = ["rt",
// "net", "io-util", "time", "macros"] }`, then `cargo wasix init`,
// `cargo update` (to get the WASIX forks) and `cargo wasix build --release`.
// Run: wasmer run --net probe.wasm -- 127.0.0.1:9222 timer   (engine running)
//
// Does a tokio TcpStream under WASIX read an answer that arrives while a
// read is already waiting? agent-browser's daemon fails exactly there: its
// reader task waits on the WebSocket while another task writes a command.
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn frame(text: &str) -> Vec<u8> {
    let mask = [1u8, 2, 3, 4];
    let mut out = vec![0x81, 0x80 | text.len() as u8];
    out.extend_from_slice(&mask);
    out.extend(text.bytes().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

async fn upgrade(addr: &str) -> R<tokio::net::TcpStream> {
    let mut s = tokio::net::TcpStream::connect(addr).await?;
    s.write_all(b"GET /devtools/browser HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").await?;
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b).await?;
        head.push(b[0]);
    }
    Ok(s)
}

/// 1. While a read waits with nothing coming, does a 300 ms timer fire on time?
async fn timer_while_reading(addr: &str) -> R<String> {
    let s = upgrade(addr).await?;
    let (mut r, _w) = tokio::io::split(s);
    let _reader = tokio::spawn(async move {
        let mut b = [0u8; 1];
        let _ = r.read_exact(&mut b).await;
    });
    let t = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(format!("timer took {:?}", t.elapsed()))
}

/// 2. Write first, then wait for the answer on a reader task.
async fn answer_after_write(addr: &str) -> R<String> {
    let s = upgrade(addr).await?;
    let (mut r, mut w) = tokio::io::split(s);
    w.write_all(&frame(r#"{"id":1,"method":"Browser.getVersion"}"#)).await?;
    let reader = tokio::spawn(async move {
        let mut head = [0u8; 2];
        r.read_exact(&mut head).await.map(|_| head)
    });
    let head = reader.await??;
    Ok(format!("answer frame opcode {:#x}", head[0] & 0x0f))
}

/// 3. Control: write and read in one task, no split.
async fn same_task(addr: &str) -> R<String> {
    let mut s = upgrade(addr).await?;
    s.write_all(&frame(r#"{"id":1,"method":"Browser.getVersion"}"#)).await?;
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    Ok(format!("answer frame opcode {:#x}", head[0] & 0x0f))
}

async fn run<F: std::future::Future<Output = R<String>>>(label: &str, f: F) {
    let start = Instant::now();
    match tokio::time::timeout(Duration::from_secs(15), f).await {
        Ok(Ok(v)) => println!("{label}: ok, {v}, in {:?}", start.elapsed()),
        Ok(Err(e)) => println!("{label}: error ({e}) after {:?}", start.elapsed()),
        Err(_) => println!("{label}: no answer in 15 s"),
    }
}

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:9499".into());
    let which = std::env::args().nth(2).unwrap_or_default();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        match which.as_str() {
            "timer" => run("1 timer while a read waits", timer_while_reading(&addr)).await,
            "after" => run("2 answer to a write made before the reader waits", answer_after_write(&addr)).await,
            _ => run("3 control: one task", same_task(&addr)).await,
        }
    });
}
