// What does a non-blocking read return under Wasmer when no data is there yet?
// POSIX says WouldBlock (EAGAIN). tokio relies on it.
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:9496".into());
    let mut s = TcpStream::connect(&addr).expect("connect");
    s.set_nonblocking(true).expect("nonblocking");
    let mut buf = [0u8; 4096];
    match s.read(&mut buf) {
        Ok(n) => println!("empty read: Ok({n})"),
        Err(e) => println!("empty read: kind={:?} raw={:?} ({e})", e.kind(), e.raw_os_error()),
    }
    s.write_all(b"GET /json/version HTTP/1.0\r\nHost: x\r\n\r\n").expect("write");
    sleep(Duration::from_millis(300));
    match s.read(&mut buf) {
        Ok(n) => println!("after data: Ok({n} bytes)"),
        Err(e) if e.kind() == ErrorKind::WouldBlock => println!("after data: WouldBlock"),
        Err(e) => println!("after data: kind={:?} raw={:?} ({e})", e.kind(), e.raw_os_error()),
    }
}
