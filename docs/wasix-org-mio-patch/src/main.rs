//! A tokio `TcpStream` that is *connected* under WASIX is left blocking by
//! mio's WASIX fork, so a read with no data blocks the runtime's thread
//! instead of returning `WouldBlock`. Nothing else runs meanwhile: this
//! 300 ms timer fires only when Wasmer gives up on the read, ~30 s later.
//!
//! The peer sends two bytes first. tokio issues the read syscall only when
//! the stream is marked readable, and it stays marked until a read says
//! `WouldBlock`; reading exactly what arrived leaves it marked, so the next
//! read reaches the socket. That is a client reading a reply and waiting for
//! the next one, as any protocol client does.

use std::io::Write;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

fn main() {
    let addr = std::env::var("REPRO_ADDR").unwrap_or_else(|_| "127.0.0.1:47111".into());

    // A peer that says "hi" and then stays silent.
    let listener = std::net::TcpListener::bind(&addr).expect("bind");
    std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().expect("accept");
        peer.write_all(b"hi").expect("write");
        std::thread::sleep(Duration::from_secs(120));
    });

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut stream = tokio::net::TcpStream::connect(&addr).await.expect("connect");
        let mut greeting = [0u8; 2];
        stream.read_exact(&mut greeting).await.expect("read the greeting");

        // Wait for a second message that is not coming, on its own task.
        let reader = tokio::spawn(async move { stream.read(&mut [0u8; 1]).await });

        let start = Instant::now();
        tokio::time::sleep(Duration::from_millis(300)).await;
        println!("300 ms timer fired after {:?}", start.elapsed());
        if reader.is_finished() {
            println!("the read returned meanwhile: {:?}", reader.await.unwrap());
        } else {
            println!("the read is still waiting, as it should");
        }
    });
}
