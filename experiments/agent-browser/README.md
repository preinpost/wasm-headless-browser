# agent-browser under WASIX: an experiment

Can `agent-browser` 0.38.2 run inside a WASIX sandbox next to the engine? This
records how far it got on 2026-10-12. `wasix.patch` applies to
`vercel-labs/agent-browser` at `v0.38.2`, against `cli/src`.

## How to reproduce

```bash
git clone --depth 1 --branch v0.38.2 https://github.com/vercel-labs/agent-browser.git
cd agent-browser && git apply ../wasix.patch && cd cli
cargo wasix init
cargo update            # resolves the WASIX forks; a per-package update does not
AR_wasm32_wasmer_wasi=<wasi-sdk>/bin/llvm-ar RANLIB_wasm32_wasmer_wasi=<wasi-sdk>/bin/llvm-ranlib \
  cargo wasix build --release
```

The WASIX registry carries forks of `tokio`, `mio`, `socket2`, `hyper`,
`reqwest`, `rustix`, `ring`, `libc`, `cc` and `getrandom`, so every dependency
compiles. Only agent-browser's own code needed changes. The module is 11.4 MB.

## What works

- It compiles, and `--version` and `--help` run under Wasmer 7.5.0.
- A daemon started by hand binds loopback TCP and writes its state files.
- The CLI reaches that daemon over TCP and hands it commands.

## What the patch changes

WASIX is neither `unix` nor `windows`, so every function split on those two
lost its body. The patch gives `target_os = "wasi"` a branch:

| Where | Change |
|---|---|
| CLI to daemon transport | The Windows path: TCP on loopback, with a `.port` file, not a Unix socket |
| `daemon_ready` | Also requires the `.port` and `.version` files (see quirk 1) |
| Daemon port file | Records the port it bound when `local_addr()` says 0 (quirk 2) |
| Spawning the daemon | By command name (`AGENT_BROWSER_COMMAND`, default `agent-browser`), since there is no `current_exe()`; a failure explains how to start it by hand |
| A daemon without a config file | Reused, not restarted. A hand-started daemon has none, and its options only shape a browser it would launch |
| `is_pid_alive`, `which_exists`, Playwright path, dashboard config | Small stand-ins |
| CDP socket | No TCP keepalive and no duplicated socket (tried for quirk 4; neither was the cause) |

## Wasmer quirks found

1. **A connect to a closed loopback port reports success.** The first write
   then fails with `Broken pipe`. Liveness by connecting alone is wrong.
2. **`local_addr()` returns port 0** after binding a fixed port.
3. **`dirs::home_dir()` is `None`**, so state went to the guest's `/tmp`, which
   each process has to itself. `AGENT_BROWSER_SOCKET_DIR` must point at a
   shared, mounted directory.
4. **The blocker: the daemon's async CDP socket fails with `ETIMEDOUT`.** The
   daemon connects to the engine with tokio-tungstenite, completes the
   WebSocket handshake and sends `Target.setDiscoverTargets`. The engine
   answers at once, and the daemon's next read fails with `Operation timed out
   (os error 73)`. The CLI then times out after 30 s.

For quirk 4, ruled out so far:

- **Wasmer's socket reads.** A plain non-blocking std read returns `WouldBlock`
  under Wasmer exactly as natively (`nonblock-probe.rs` here), and the
  blocking CDP probe in `scripts/wasix-loopback-client.rs` drives the same
  engine to completion under the same Wasmer.
- **TCP keepalive** and **the duplicated socket**: removing both changes nothing.

The suspect left is the WASIX fork of mio (`1.0.3+wasix.1`). Its selector
waits with `poll_oneoff`, and `check_errors` fails the whole wait when any one
event carries an error. Which event carries `ETIMEDOUT`, and why, was not
traced.

Not tried: the browser sandbox (`@wasmer/sdk`), whose networking is a separate
implementation from the Wasmer CLI's.

## Outcome

The project went on with `whb` (`crates/client`), a client of its own with
agent-browser's commands and output, on blocking sockets. It runs under the
Wasmer CLI and in the browser sandbox. agent-browser could follow once quirk 4
is understood; the patch above is the starting point.
