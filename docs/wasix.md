# Running on WASIX

Recorded from an actual run, not from documentation. Environment: Rust 1.96
(rustup default), `cargo-wasix` 0.1.34 with the WASIX toolchain
`v2026-09-18.1+rust-1.97`, Wasmer CLI 7.5.0, macOS arm64.

## Result

The engine builds for `wasm32-wasmer-wasi`, binds loopback, and serves the
protocol from inside the sandbox. `MODE=wasix ./scripts/verify-m0.sh` passes all
16 checks against the real `agent-browser`, driven from outside the sandbox.

Artefact size: **272 KB** (`wasm-headless-browser.wasm`).

## Loopback works

This was the largest open question, because it decides whether the engine and
the automation client can both live inside one sandbox and talk over
`127.0.0.1`, or whether CDP has to be carried over a pipe instead.

It works. `scripts/wasix-netprobe.rs` isolates the question:

```
$ rustup run wasix rustc --target wasm32-wasmer-wasi -O \
    scripts/wasix-netprobe.rs -o /tmp/netprobe.wasm
$ wasmer run --net /tmp/netprobe.wasm
[1] starting
[2] bind ok, local_addr = Ok(127.0.0.1:9445)
[3] client thread up
[6] accepting
[4] connect ok
[7] accept ok from 127.0.0.1:49366
[8] server read: "ping"
[5] round trip: "pong"
[9] done
```

That confirms four separate things at once: `TcpListener::bind` on loopback,
`TcpStream::connect` to it, a working round trip, and WASIX threads. The last
matters because `cdp-wire` runs one thread per connection.

## `--net` is required, and the failure looks like a WASIX limitation

Without `--net`, the guest gets EOPNOTSUPP on the first bind:

```
$ wasmer run /tmp/netprobe.wasm
[2] bind FAILED: Not supported (os error 58) (raw_os_error=Some(58))
```

Error 58 is easy to misread as "WASIX cannot do sockets". It is not. Wasmer
denies socket creation by default, and denial surfaces as `EOPNOTSUPP`. The same
binary works unchanged once the runtime is allowed:

```
$ wasmer run --net /tmp/netprobe.wasm
[2] bind ok, local_addr = Ok(127.0.0.1:9445)
```

Wasmer also supports narrowing the grant by address, port and direction, which
is what a real deployment would do:

```
wasmer run --net=ipv4:allow=127.0.0.1:9222/in ...
```

## A client outside the sandbox can reach in

Under the Wasmer CLI, the guest's loopback listener is the host's loopback
listener. `curl` on macOS reaches it directly, and so does `agent-browser`. So
the second option from the design discussion — engine inside, client outside —
works in this runtime, and no port forwarding is involved.

This does **not** yet settle the browser case. `@wasmer/sdk` inside a page is a
different runtime, and whether its sandbox shares loopback with the host page
still needs checking. The alternative remains: run both inside, which the
netprobe shows is viable.

## Build notes

**No C toolchain was needed.** The project is pure Rust, and nothing in the
dependency tree needs a sysroot. `wasixcc` was not installed.

**`sha1` was removed for this reason.** The `sha1` crate reaches `libc` through
`cpufeatures`, and the WASIX overlay registry has no matching `libc`, which
fails resolution outright:

```
error: failed to select a version for the requirement `libc = "^0.2.155"`
candidate versions found which didn't match: 0.2.186+wasix.1, 0.2.183+wasix.1, ...
```

`crates/cdp-wire/src/sha1.rs` replaces it. The algorithm is needed only because
RFC 6455 requires it in the handshake, so its known weaknesses do not matter,
and its unit tests include the worked example from the RFC.

**`cargo-wasix` writes `.cargo/config.toml`.** It replaces `crates.io` with the
WASIX overlay registry:

```toml
[source.crates-io]
replace-with = "wasix"
```

The file is gitignored so a fresh clone resolves from crates.io. Native builds
happen to work either way, because every dependency here exists in both
registries, but depending on a third-party mirror for the default build is not
something to leave in place by accident.

## The browser sandbox: loopback is shared

`scripts/verify-m0.sh` proves the engine works under a WASIX runtime on the host.
It does not prove anything about `@wasmer/sdk` in a page, which is a different
runtime and the one that matters.

`harness/` answers it. It loads two WASIX modules into one browser sandbox: the
real engine, and a client that opens a TCP connection to `127.0.0.1` and speaks
HTTP. Run against `@wasmer/sdk` 0.19.0 in Chrome, with
`network: { mode: "http" }`:

| | Question | Result |
|---|---|---|
| E0 | Does the engine bind loopback in the sandbox? | **pass** |
| E1 | Can a second command in the *same* sandbox reach it? | **pass** |
| E2 | Does the SDK observe the guest listener? | **pass** |
| E3 | Can the *host page* reach it? | fail |

E1 is the one that decides the architecture, and it is a clean pass. The client
connected on its first attempt and received the engine's full response:

```
[client] target 127.0.0.1:9222
[client] connected on attempt 1
[client] RESULT: got 444 bytes back
[client] body: {"Browser":"Chrome/140.0.0.0","Protocol-Version":"1.3",...
                "webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/browser/66AC..."}
```

So **the engine and its automation client can both live in one browser sandbox
and talk over loopback.** No pipe, no port forwarding, no relay. The engine's
own discovery endpoint answers a client that is inside the same sandbox.

E2 confirms it from the SDK's side: `sandbox.ports.wait(9222)` resolved, meaning
the platform itself sees the guest listener rather than merely tolerating it.

E3 failed on an external dependency, not on our code:

```
FAIL  E3 host page reaches the engine: the Wasmer HTTP host at
      https://default.local.wasmer.site did not become ready
```

`ports.expose()` routes host HTTP into a guest listener through a service worker
served from `https://default.local.wasmer.site`, which never became ready here.
That path is worth revisiting only if a host-side client is ever wanted. E1 says
it is not needed.

### Two smaller findings

- A spurious `accept failed: Operation timed out (os error 73)` reaches the
  engine's stderr when a client disconnects. The accept loop already logs and
  continues rather than tearing down, which is why E1 still passed.
- `proc.stdout` in the SDK is a `ReadableBytes` with a `lines()` async generator,
  not a `Web Streams` `ReadableStream`. Piping it with `pipeTo` fails.

## Commands

```bash
cargo install cargo-wasix        # fetches the WASIX Rust toolchain on first use
brew install wasmer              # runtime

scripts/wasix-build.sh           # → target/wasm32-wasmer-wasi/release/*.wasm
scripts/wasix-run.sh             # → serves on 127.0.0.1:9222

MODE=wasix ./scripts/verify-m0.sh
```

For the browser sandbox, see the section above and
[`../harness/README.md`](../harness/README.md).
