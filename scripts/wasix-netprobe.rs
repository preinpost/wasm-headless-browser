// Probe: does WASIX allow a process to listen on loopback and talk to itself?
//
// This is the question that decides whether the engine and the automation
// client can both live inside one sandbox and talk over 127.0.0.1, or whether
// CDP has to be carried over a pipe instead. It is small and self-contained on
// purpose: it answers nothing else.
//
// Build and run:
//
//   rustup run wasix rustc --target wasm32-wasmer-wasi -O \
//     scripts/wasix-netprobe.rs -o /tmp/netprobe.wasm
//   wasmer run --net /tmp/netprobe.wasm
//
// Without `--net` the first bind fails with EOPNOTSUPP (os error 58). See the
// result recorded in the README.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn main() {
    println!("[1] starting");

    let listener = match TcpListener::bind("127.0.0.1:9445") {
        Ok(l) => {
            println!("[2] bind ok, local_addr = {:?}", l.local_addr());
            l
        }
        Err(e) => {
            println!("[2] bind FAILED: {e} (raw_os_error={:?})", e.raw_os_error());
            return;
        }
    };

    // A second thread, so this also checks that WASIX threads work.
    let client = thread::spawn(|| {
        println!("[3] client thread up");
        match TcpStream::connect("127.0.0.1:9445") {
            Ok(mut s) => {
                println!("[4] connect ok");
                let _ = s.write_all(b"ping");
                let mut buf = [0u8; 4];
                let _ = s.read_exact(&mut buf);
                println!("[5] round trip: {:?}", String::from_utf8_lossy(&buf));
            }
            Err(e) => println!("[4] connect FAILED: {e} (raw={:?})", e.raw_os_error()),
        }
    });

    println!("[6] accepting");
    match listener.accept() {
        Ok((mut s, peer)) => {
            println!("[7] accept ok from {peer}");
            let mut buf = [0u8; 4];
            let _ = s.read_exact(&mut buf);
            println!("[8] server read: {:?}", String::from_utf8_lossy(&buf));
            let _ = s.write_all(b"pong");
            let _ = s.flush();
        }
        Err(e) => println!("[7] accept FAILED: {e} (raw={:?})", e.raw_os_error()),
    }

    let _ = client.join();
    println!("[9] done");
}
