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

This does not settle the browser case, which is a different runtime. It is
settled further down, and the answer there is the opposite one: in a page, the
listener is not reachable from outside the sandbox. Running both sides inside
turns out to be the design, not the fallback.

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

## Egress from the browser sandbox

Loopback is one question; reaching the outside world is another. The browser
sandbox has no direct network access, so egress goes over a **WISP** relay: a
WebSocket server that opens the real TCP connection on its own side. The SDK
installs the client half when the sandbox is created with
`network: { mode: "wisp", url: "ws://..." }`.

`harness/wisp-server.mjs` runs the relay locally, and two probes cover the path.

**E4** opens a plain TCP connection to a public host. A TLS port will not answer
a plaintext request, so a completed connect is the signal.

**E5** runs the `curl/curl` package inside the sandbox and makes a real HTTPS
request. TLS terminates **inside the guest**, so certificates are the guest's
problem, not the relay's.

Against a real login site (its name left out here):

```
E4 PASS: TCP connected (login.example.com:443)
E5 PASS: HTTP/1.1 200 OK
```

The headers match what `curl` on the host returns for the same URL, which is
expected: the WISP relay runs on this machine, so the sandbox egresses from the
same address.

The response body is worth reading, because it answers a question the engine
work depends on. It contains no `<title>` and a single loading overlay from the
site's load balancer. The actual screen is built later by JavaScript from a
set of bundles. **Fetching the
HTML gets you nothing.** A headless engine that cannot run page script cannot
read this page at all, which is the whole argument for M2.

### Two smaller findings

- A spurious `accept failed: Operation timed out (os error 73)` reaches the
  engine's stderr when a client disconnects. The accept loop already logs and
  continues rather than tearing down, which is why E1 still passed.
- `proc.stdout` in the SDK is a `ReadableBytes` with a `lines()` async generator,
  not a `Web Streams` `ReadableStream`. Piping it with `pipeTo` fails.

## Build notes

**No C toolchain was needed at first.** The project was pure Rust, and nothing
in the dependency tree needed a sysroot. QuickJS and ring changed that: both
compile C. See [quickjs.md](quickjs.md) for QuickJS.

**The C archiver must understand wasm.** Each crate's C objects are put into a
static archive, and the linker finds them through the archive's symbol table.
macOS's own `ar` and `ranlib` cannot read wasm objects: they warn
`archive member '...' not a mach-o file` and write an archive whose only member
is an empty `__.SYMDEF` (96 bytes). The link then fails:

```
rust-lld: error: ... undefined symbol: ring_core_0_17_14__LIMB_is_zero
```

The `cc` crate looks for `llvm-ar` next to the C compiler for wasm targets, and
Apple's clang has none beside it, so it falls back to `ar`. QuickJS is not hit,
because rquickjs-sys archives with the wasi-sdk it fetches. `ring` is.
`scripts/wasix-build.sh` therefore finds a wasm-capable `llvm-ar` and passes it
as `AR_wasm32_wasmer_wasi` and `RANLIB_wasm32_wasmer_wasi`. It looks at
`WASI_SDK`, `WASI_SDK_PATH`, `PATH`, rustup's `llvm-tools`, Homebrew's LLVM, and
last the wasi-sdk rquickjs-sys fetched into the target directory. When there is
none, it builds rquickjs-sys first, which fetches that wasi-sdk, so a clean
machine needs nothing extra installed.

ring's build script does not rerun when the archiver changes. An empty archive
left by a wrong `ar` would keep failing the link after the fix, so the script
removes ring's build outputs when it finds one.

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

The file is gitignored so a fresh clone resolves from crates.io. Depending on a
third-party mirror for the default build is not something to leave in place by
accident.

**One `Cargo.lock` cannot serve both registries.** For `libc`, `cc`, `ring`
and `getrandom` the overlay registry carries `+wasix` builds
(`libc 0.2.186+wasix.1`), which crates.io does not have. Each registry
therefore fails to resolve the other's lockfile. The committed lockfile is the
crates.io one; `scripts/wasix-build.sh` re-pins the packages in its
`WASIX_FORKS` list for the build, then restores the lockfile and removes
`.cargo/config.toml`.

## TLS inside the guest

`experiments/tls` probes it, and `crates/engine/src/fetch.rs` uses the result.

rustls 0.23 with the **ring** provider, over a plain `std::net::TcpStream`,
trusting `webpki-roots`. ring because the overlay registry carries a fork of it
(`ring 0.17.14+wasix.1`); rustls's default provider, aws-lc-rs, has none and is
a much larger C build. Under Wasmer 7.5.0 with `--net`:

```
[tls] tcp connected to github.com:443
[tls] handshake done: Some(TLSv1_3) Some(TLS13_AES_128_GCM_SHA256)
[tls] status: HTTP/1.1 200 OK
[tls] 582933 bytes in 181.717ms
```

`example.com`, `www.google.com`, `github.com` and the reference login site
all handshake TLS 1.3 and return 200, so that site's certificate chains to a
public root. A site signed by a private CA needs its root in a PEM file named by
`WHB_EXTRA_CA_CERTS` (a path list, like `NODE_EXTRA_CA_CERTS`).

The engine module grew from 1.6 MB to 2.3 MB with TLS.

The probe has no lockfile of its own committed. To build it for WASIX, re-pin
the forks first:

```bash
cd experiments/tls
cargo wasix init
cargo update -p libc -p cc -p ring -p getrandom
cargo wasix build --release
wasmer run --net target/wasm32-wasmer-wasi/release/tls-probe.wasm -- example.com
```

## Repeated heavy page loads

Measured on 2026-10-12: repeated loads originally exhausted WASIX thread
stacks (`thread_spawn`, `Errno::overflow`). Fetches now reuse a process-wide
pool of twelve workers, with six requests per host. Requests waiting for a
host occupy queue entries instead of allocating stacks; a saturated host
does not hold workers needed by another host.

The browser still ran out of memory until computed styles stopped copying
thousands of inherited CSS variables into every element and pseudo-element.
The variable map and immutable values are now shared, copied only where
an element overrides variables. The 32 MB script stack stays unchanged and
is reused when navigating within a tab.

`verify-loads.sh` passes natively and under Wasmer: three tabs with ten loads
each, 2,400 asset requests, all forty scripts executing on every load. Each
page also has 1,600 root CSS variables and 800 spans, with pseudo-element
rules, to exercise the large-theme case without relying on a public site.

In the browser sandbox, NAVER passes E6, E7, E8 and E9's ten additional
navigations in the same engine. Allocated linear memory at the end of the
final verification run was 339.2 MiB (not live heap; linear memory cannot
shrink). Run with `LOADS=10 RESOURCES=1` to repeat and observe it; see
[`../harness/README.md`](../harness/README.md).

## Commands

```bash
cargo install cargo-wasix        # fetches the WASIX Rust toolchain on first use
brew install wasmer              # runtime

scripts/wasix-build.sh           # → target/wasm32-wasmer-wasi/release/*.wasm
scripts/wasix-run.sh             # → serves on 127.0.0.1:9222

MODE=wasix ./scripts/verify-m0.sh
MODE=wasix ./scripts/verify-loads.sh

# the browser-sandbox probes, driven and judged automatically
./scripts/verify-harness.sh
OUTBOUND=login.example.com:443 CURL=https://login.example.com/ ./scripts/verify-harness.sh
```

For the browser sandbox, see the section above and
[`../harness/README.md`](../harness/README.md).
