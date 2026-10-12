// Build as a cargo project with `wasix = "0.13"`, then `cargo wasix build
// --release`. Run: wasmer run --net probe.wasm -- <quiet host:port> <late host:port>,
// the second a server that answers 300 ms after the first byte it reads.
//
// WASIX epoll, as mio uses it under Wasmer: is epoll_wait's timeout kept
// while an idle socket is registered?
use std::io::Write;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::time::Instant;
use wasix as w;

const IN: u32 = w::EPOLL_TYPE_EPOLLIN | w::EPOLL_TYPE_EPOLLRDHUP | w::EPOLL_TYPE_EPOLLET;
const INOUT: u32 = IN | w::EPOLL_TYPE_EPOLLOUT;

fn ep_with(fd: u32, kinds: u32) -> w::Fd {
    unsafe {
        let ep = w::epoll_create().unwrap();
        let ev = w::EpollEvent { events: kinds, data: w::EpollData { ptr: 0, fd, data1: 0, data2: 7 } };
        w::epoll_ctl(ep, w::EPOLL_CTL_ADD, fd, &ev).unwrap();
        ep
    }
}

fn wait(label: &str, ep: w::Fd, ms: u64) {
    let mut events: Vec<w::EpollEvent> = Vec::with_capacity(8);
    let start = Instant::now();
    let res = unsafe { w::epoll_wait(ep, events.as_mut_ptr(), 8, ms * 1_000_000) };
    let took = start.elapsed();
    match res {
        Ok(n) => {
            unsafe { events.set_len(n as usize) };
            let kinds: Vec<String> = events.iter().map(|e| format!("{:#x}", e.events)).collect();
            println!("{label}: {took:?} -> {n} events [{}]", kinds.join(","));
        }
        Err(e) => println!("{label}: {took:?} -> error {}", e.raw()),
    }
}

fn quiet(addr: &str) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_nonblocking(true).unwrap();
    s
}

fn main() {
    let quiet_addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:9499".into());
    let late_addr = std::env::args().nth(2).unwrap_or_else(|| "127.0.0.1:9501".into());

    let s = quiet(&quiet_addr);
    wait("A quiet socket, IN, 300 ms", ep_with(s.as_raw_fd() as u32, IN), 300);

    let s = quiet(&quiet_addr);
    wait("B quiet socket, IN|OUT, 300 ms", ep_with(s.as_raw_fd() as u32, INOUT), 300);

    let s = quiet(&quiet_addr);
    let ep = ep_with(s.as_raw_fd() as u32, INOUT);
    wait("E1 quiet socket, IN|OUT, first wait", ep, 300);
    wait("E2 same, second wait, 300 ms", ep, 300);

    let efd = unsafe { w::fd_event(0, 0).unwrap() };
    wait("C eventfd only, 300 ms", ep_with(efd, w::EPOLL_TYPE_EPOLLIN | w::EPOLL_TYPE_EPOLLET), 300);

    let mut s = TcpStream::connect(&late_addr).unwrap();
    s.write_all(b"x").unwrap();
    s.set_nonblocking(true).unwrap();
    wait("D socket answered after 300 ms, IN, up to 5 s", ep_with(s.as_raw_fd() as u32, IN), 5000);
}
