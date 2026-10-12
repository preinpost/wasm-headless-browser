# mio's WASIX fork leaves connected TCP sockets blocking

A bug in `wasix-org/mio` (the `mio` that `cargo wasix` resolves, published as
`1.0.3+wasix.1`), a minimal reproduction, a patch, and an issue ready to file.
This directory is the handoff point for reporting and fixing it upstream.

## Handoff

**Status (2026-10-12): not reported upstream.** Nothing matching was found in
`wasmerio/wasmer` issues or PRs, nor anywhere on GitHub by symptom. The fork
repositories `wasix-org/mio` and `wasix-org/tokio` have issues disabled. The
code is unchanged on `wasix-org/mio` branches `wasix-1.0.3` (the published
crate's source), `wasix`, `epoll` and `master`.

| File | What |
|---|---|
| `src/main.rs` | The reproduction: one program, tokio only, no network beyond loopback |
| `repro.sh` | Runs it under Wasmer against the published mio, then the patched one |
| `mio-wasix-nonblocking.patch` | The fix, for `src/sys/wasi/tcp.rs` of `mio 1.0.3+wasix.1` |
| `Cargo.lock` | Pins the versions reported: tokio `1.47.0+wasix.1`, mio `1.0.3+wasix.1` |

```bash
./repro.sh      # needs cargo-wasix and wasmer; about 35 s, 30 of them the bug
```

Next steps:

- [ ] File the issue below on `wasmerio/wasmer` (the fork takes no issues).
- [ ] Optionally open a PR on `wasix-org/mio` against `wasix-1.0.3`; see
      "Proposed fix" for the choice it has to make.
- [ ] Once a fixed mio is published, delete `Cargo.lock` and run `repro.sh`
      to confirm, then drop the mio patch from
      `scripts/build-agent-browser-wasix.sh`.

In this repository the patch is what lets agent-browser's daemon run under
WASIX: `scripts/build-agent-browser-wasix.sh` applies it, and
[`experiments/agent-browser`](../../experiments/agent-browser/README.md)
records how it was found.

---

## Issue

**Title:** mio WASIX fork leaves connected TCP sockets blocking: a tokio read
stalls the runtime for 30 s, then fails with ETIMEDOUT

### Summary

`mio 1.0.3+wasix.1` (`wasix-org/mio`, branch `wasix-1.0.3`) opens TCP sockets
with `sock_open` and never sets `FDFLAGS_NONBLOCK` on a socket it connects or
listens on. tokio expects mio's sockets to be non-blocking: it calls `read`
when the stream is marked readable and waits for `WouldBlock`. On a blocking
socket that read instead blocks the runtime's thread until Wasmer's socket
timeout, about 30 seconds, and then fails with `ETIMEDOUT` (os error 73).
Meanwhile no timer and no other task on that thread runs.

Accepted sockets are not affected: `accept` asks `sock_accept_v2` for
`FDFLAGS_NONBLOCK`.

### Reproduction

`src/main.rs`: a std listener on loopback sends `hi` and stays silent. A tokio
client connects, reads the two bytes, spawns a task that waits for more, and
sleeps 300 ms. Reading exactly what arrived leaves the stream marked
readable, so the waiting task's read reaches the socket, as it does for any
client that reads a reply and waits for the next.

```bash
cargo wasix init && cargo wasix build --release
wasmer run --net target/wasm32-wasmer-wasi/release/repro.wasm
```

| Run | 300 ms timer fired after | The waiting read |
|---|---|---|
| Native (macOS arm64) | 301.7 ms | still waiting |
| WASIX, mio `1.0.3+wasix.1` | **30.004 s** | returned `Err(Os { code: 73, kind: TimedOut, message: "Operation timed out" })` |
| WASIX, mio with the patch below | 303.4 ms | still waiting |

The same happens in the browser with `@wasmer/sdk` 0.19.0, where we first met
it: agent-browser's daemon, connecting to a CDP server in the same sandbox
with tokio-tungstenite, never saw the server's first answer.

### Cause

`src/sys/wasi/net.rs`, whose doc comment promises a non-blocking socket:

```rust
/// Create a new non-blocking socket.
pub(crate) fn new_socket(domain: libc::c_int, socket_type: libc::c_int) -> io::Result<libc::c_int> {
    // ...
    let socket = unsafe { wasi::sock_open(wasi_address_type(domain)?, wasi_socket_type(socket_type)?, proto) ... };
    Ok(socket as libc::c_int)
}
```

`src/sys/wasi/tcp.rs` connects and listens on it as it is, while `accept`
does ask for non-blocking:

```rust
pub(crate) fn connect(socket: &net::TcpStream, addr: SocketAddr) -> io::Result<()> {
    unsafe { wasi::sock_connect(socket.as_raw_fd() as u32, &addr) ... }
}

pub(crate) fn accept(listener: &net::TcpListener) -> io::Result<(net::TcpStream, SocketAddr)> {
    unsafe { wasi::sock_accept_v2(listener.as_raw_fd() as u32, wasi::FDFLAGS_NONBLOCK) ... }
}
```

### Impact

Every tokio client socket under WASIX: `tokio::net::TcpStream::connect`, and
through it hyper, reqwest, tokio-tungstenite. A program survives only as long
as data is always already there when it reads. A listener has the same
problem with an `accept` that finds nothing waiting.

### Environment

- Wasmer 7.5.0 (CLI, macOS 26.6.2 arm64) and `@wasmer/sdk` 0.19.0 (Chrome)
- cargo-wasix 0.1.34, rustc 1.97.1-dev (the `wasix` toolchain)
- tokio `1.47.0+wasix.1`, mio `1.0.3+wasix.1`, from the WASIX registry

### Proposed fix

`mio-wasix-nonblocking.patch`: set `FDFLAGS_NONBLOCK` with
`fd_fdstat_set_flags` (as `waker.rs` already does) once `sock_connect` returns,
and once `sock_listen` returns. Connecting while still blocking means there is
no `EINPROGRESS` to handle. It fixes the reproduction above and agent-browser
in both the Wasmer CLI and the browser.

The fuller fix makes the socket non-blocking in `new_socket`, as Unix mio
does, and treats `EINPROGRESS` from `sock_connect` as success, leaving the
completion to the writable event. The WASIX `connect` currently returns any
errno as an error, and since #6468 Wasmer can answer a non-blocking connect
with `EINPROGRESS`, so moving the flag alone would break connecting.

Related, but each about connecting rather than reading: #6433 (non-blocking
connect returns success instead of `EINPROGRESS`), #6897 (false-positive
connect success on non-blocking sockets), #6479 (a blocking connect waits
about 30 s regardless of the requested timeout).

### Ruled out

Each under the same Wasmer, each with a small program:

- A std `TcpStream` with `set_nonblocking(true)` and no data gives
  `WouldBlock`, as natively.
- WASIX `epoll_wait`, registered as mio registers (`EPOLLIN | EPOLLRDHUP |
  EPOLLET`, plus `EPOLLOUT`), keeps a 300 ms timeout with an idle socket and
  wakes on data arriving later.
- While the tokio repro hangs, mio's selector is never called: the thread is
  inside the read, not inside `epoll_wait`.
