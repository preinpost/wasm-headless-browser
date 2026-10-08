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

## Commands

```bash
cargo install cargo-wasix        # fetches the WASIX Rust toolchain on first use
brew install wasmer              # runtime

scripts/wasix-build.sh           # → target/wasm32-wasmer-wasi/release/*.wasm
scripts/wasix-run.sh             # → serves on 127.0.0.1:9222

MODE=wasix ./scripts/verify-m0.sh
```
