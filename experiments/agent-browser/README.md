# agent-browser under WASIX

`agent-browser` 0.38.2 runs inside a WASIX sandbox next to the engine, the
browser sandbox (`@wasmer/sdk`) included: the harness's E8 starts its daemon
and drives the engine with its CLI, one process per command, as pi in a page
would. Two patches make it work; `scripts/build-agent-browser-wasix.sh`
applies them and builds.

The wasm it writes is a harness fixture, not a release artefact of this
repository. The release is `wasm-headless-browser.wasm`. On the host, an
ordinary install of agent-browser drives that engine with `--cdp`. This build
exists because the browser sandbox accepts no WebSocket from the host page.

```bash
./scripts/build-agent-browser-wasix.sh          # → target/agent-browser-wasix/agent-browser.wasm
ENGINE=html AGENT_BROWSER_WASM=target/agent-browser-wasix/agent-browser.wasm \
  ./scripts/verify-harness.sh                   # E8
```

The module is 11.4 MB. Measured 2026-10-12 with Wasmer 7.5.0 and
`@wasmer/sdk` 0.19.0. With `wasmer/bash` in the same sandbox, both commands
are on the guest `PATH` as `/usr/bin/engine` and `/usr/bin/agent-browser`.
`HARNESS_BASH=1` checks that shell path (E11).

## Running it in a sandbox

The CLI cannot start its own daemon under WASIX (there is no
`current_exe()`), so whatever starts the engine starts the daemon too, with
`AGENT_BROWSER_DAEMON=1`. Both need the same state directory, set explicitly:
`dirs::home_dir()` gives nothing under WASIX, and a command's `/tmp` is its
own. In the browser sandbox that directory must be under `/workspace`, the
SDK's shared filesystem.

```js
const env = { HOME: '/workspace', AGENT_BROWSER_SOCKET_DIR: '/workspace/.agent-browser', AGENT_BROWSER_SESSION: 'pi' }
await sandbox.fs.mkdir(env.AGENT_BROWSER_SOCKET_DIR, { recursive: true })
await sandbox.command('agent-browser', [], { env: { ...env, AGENT_BROWSER_DAEMON: '1' } }).spawn({})
// wait for /workspace/.agent-browser/pi.port, then, per command:
await sandbox.command('agent-browser', ['--cdp', 'http://127.0.0.1:9222', 'snapshot', '-i'], { env }).run()
```

## The patches

**`wasix.patch`, for agent-browser.** WASIX is neither `unix` nor `windows`,
so every function split on those two lost its body:

| Where | Change |
|---|---|
| CLI to daemon transport | The Windows path: TCP on loopback, with a `.port` file, not a Unix socket |
| `daemon_ready` | Also requires the `.port` and `.version` files (quirk 1) |
| Daemon port file | Records the port it bound when `local_addr()` says 0 (quirk 2) |
| Spawning the daemon | By command name (`AGENT_BROWSER_COMMAND`, default `agent-browser`), since there is no `current_exe()`; a failure explains how to start it by hand |
| A daemon without a config file | Reused, not restarted. A hand-started daemon has none, and its options only shape a browser it would launch |
| `is_pid_alive`, `which_exists`, Playwright path, dashboard config | Small stand-ins |

**`mio-wasix-nonblocking.patch`, for the WASIX fork of mio
(`1.0.3+wasix.1`).** The fork's `new_socket` says it makes a non-blocking
socket and does not: `sock_open` gives a blocking one. Accepted sockets are fine
(`sock_accept_v2` is asked for `FDFLAGS_NONBLOCK`), but a socket tokio
*connects* stays blocking. A read with no data then blocks the runtime's
thread until Wasmer's socket timeout, 30 seconds, and fails with `ETIMEDOUT`;
no timer or other task runs meanwhile. The patch makes a connected socket
non-blocking once the connect completes (so there is no `EINPROGRESS` to
handle), and a listener once it listens. The build uses the patched copy
through `[patch.crates-io]`.

## Wasmer quirks found

1. **A connect to a closed loopback port reports success.** The first write
   then fails with `Broken pipe`. Liveness by connecting alone is wrong.
2. **`local_addr()` returns port 0** after binding a fixed port.
3. **`dirs::home_dir()` is `None`.**
4. **A blocking socket read gives up after 30 seconds with `ETIMEDOUT`.**
   Harmless for a blocking client that waits for an answer it knows is
   coming; fatal under an event loop that expected `WouldBlock`. The engine's
   own log line `accept failed: Operation timed out (os error 73)` is the
   same timeout on its blocking accept loop.

## How quirk 4 was found

Each step ruled out a layer. Under Wasmer mio waits with WASIX epoll, not
`poll_oneoff`: the second probe tested a call mio does not make, the third
the one it does.

| Probe | Result |
|---|---|
| std non-blocking read, no data (`nonblock-probe.rs`) | `WouldBlock`, as natively |
| raw `poll_oneoff`, idle socket and a 300 ms clock | returns at 300 ms |
| raw WASIX `epoll_wait`, registered as mio registers (`epoll-probe.rs`) | timeouts kept, late data wakes it |
| tokio: a 300 ms timer while a read waits (`tokio-timer-probe.rs`) | 30 s, and `epoll_wait` was never called meanwhile; 303 ms with the patch |

The last line placed the hang outside the event loop's wait, in a read that
blocked. TCP keepalive and the duplicated socket the daemon makes were
suspected first; neither mattered, and the patch leaves both alone.
