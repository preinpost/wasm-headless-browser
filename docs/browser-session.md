# Browser session adapter

`harness/public/browser-session.js` is a browser-specific command adapter for
an existing `@wasmer/sdk` sandbox. It owns an engine, an agent-browser daemon
and the CLI processes it launches. It has no SDK import, Node dependency,
shell parser, model connection or pi dependency.

The interface is `start()`, `run(argv, options)` and `stop()`. The harness
uses it for E8; E10 exercises its result and lifecycle behavior.

## Use it

Build the modules with `scripts/wasix-build.sh` and
`scripts/build-agent-browser-wasix.sh`. Serve them as `engine.wasm` and
`agent-browser.wasm`, and serve this module alongside the page. The existing
harness does that when `AGENT_BROWSER_WASM` is set.

The page needs cross-origin isolation (COOP/COEP), SharedArrayBuffer and JSPI,
as the Wasmer SDK does. Chrome is the tested browser. Outbound networking
needs a WISP relay; the engine and agent-browser talk over sandbox loopback.

```js
import { Wasmer } from '/wasmer-sdk/dist/index.js'
import { BrowserSession } from '/browser-session.js'

async function moduleBytes(url) {
  const response = await fetch(url)
  if (!response.ok) throw new Error(`could not load ${url}: HTTP ${response.status}`)
  return new Uint8Array(await response.arrayBuffer())
}

const wasmer = new Wasmer()
const [engine, cli] = await Promise.all([
  moduleBytes('/engine.wasm'),
  moduleBytes('/agent-browser.wasm'),
])
const pkg = await wasmer.packages.create({
  modules: { engine, cli },
  commands: {
    engine: { module: 'engine' },
    'agent-browser': { module: 'cli' },
  },
})
const sandbox = await wasmer.sandboxes.create({
  packages: [pkg],
  network: { mode: 'wisp', url: 'ws://127.0.0.1:5010/', peers: [] },
})
const browser = new BrowserSession(sandbox)

try {
  await browser.start()
  const opened = await browser.run(['open', 'https://example.com/'])
  if (!opened.ok) throw new Error(opened.stderr || opened.stdout || opened.reason)

  const snapshot = await browser.run(['snapshot', '-i'])
  console.log(snapshot)
  const evaluated = await browser.run(['eval', '--stdin'], {
    stdin: 'document.title',
    timeoutMs: 30_000,
  })
  console.log(evaluated)
} finally {
  await browser.stop()
}
// The caller still owns sandbox and wasmer; their lifetime is separate.
```

`peers: []` isolates the sandbox's virtual loopback from other sandboxes.
Within a sandbox the port must be unused. The adapter also prevents another
instance in this module from claiming the same port on the same sandbox
object while it is owned.

## Interface

### `new BrowserSession(sandbox, options)`

The sandbox must already contain the two commands and expose `command`,
`fs` and `ports.wait`. Package acquisition and sandbox creation stay with the
caller. Do not set `AGENT_BROWSER_DAEMON` or `AGENT_BROWSER_DASHBOARD` in the
sandbox's base environment: agent-browser treats their **presence**, even an
empty or `"0"` value, as a request to enter those background modes.

| Option | Default | Purpose |
|---|---|---|
| `port` | `9222` | Engine's loopback port |
| `engine` | `'engine'` | SDK selector for the engine command |
| `agentBrowser` | `'agent-browser'` | SDK selector for the patched CLI |
| `engineKind` | `'html'` | `'html'` or `'fake'` |
| `startTimeoutMs` | `30000` | Readiness deadline, checked between SDK calls |
| `commandTimeoutMs` | `90000` | Default SDK execution timeout per CLI process |
| `outputBytes` | `262144` | SDK capture limit per output stream |
| `traceResources` | `false` | Sets engine `WHB_TRACE_RESOURCES=1` |
| `onLog` | no-op | Service log observer receiving `{ process, stream, line }` |

Read-only getters: `state`, `session` (a generated UUID-based name) and
`cdpUrl`. Observer exceptions cannot interrupt execution or cleanup. The
adapter retains only the last 8,192 characters of service diagnostics for
startup errors; CLI output is returned to the caller, not sent to `onLog`.

### `await browser.start()`

Creates a private directory under `/workspace/.browser-sessions/`, starts
the engine and waits for its successful-bind discovery banner and listener. It then starts the daemon, waits for
its port file and runs `get url` against the engine to prove readiness.
There is no fixed startup sleep and no reuse of someone else's port file.

Concurrent `start()` calls share one startup operation. Calling it when
ready does not spawn more processes. On a partial failure, it stops what it
started and removes its own directory. A failed or stopped instance cannot
restart; create another instance instead.

### `await browser.run(argv, options)`

`argv` is a nonempty array of strings, **without** the executable name.
Arguments pass directly to `sandbox.command`, never to a shell. A string
such as `"open https://example.com"` is rejected. The adapter pins CDP,
session and namespace; attempts to supply `--cdp`, `--session` or
`--namespace` are rejected.

| Option | Purpose |
|---|---|
| `timeoutMs` | Overrides the SDK execution timeout for this command |
| `outputBytes` | Overrides the capture limit |
| `stdin` | String or Uint8Array, written to the CLI's stdin and followed by EOF |
| `signal` | AbortSignal; kills the CLI process if aborted |

Commands serialize so a later snapshot cannot race an earlier navigation.
Arguments and byte input are copied at submission time. Queue waiting time
is not charged to the SDK process timeout. SDK package/module acquisition is
also not interruptible through this interface; a stop/abort during spawn
waits for the process handle and kills it once available, rather than losing
track of a late process.

A normal process completion, including a nonzero exit, timeout or
termination, returns a plain, JSON-serializable object:

```js
{
  ok: true,
  stdout: '"Example Domain"\n',
  stderr: '',
  exitCode: 0,
  reason: 'exited',
  timedOut: false,
  aborted: false,
  stdoutTruncated: false,
  stderrTruncated: false,
  durationMs: 42.5,
}
```

`reason` is `exited`, `timeout`, `terminated` or `aborted`. An already-aborted
queued command returns `aborted` with `exitCode: null`, without spawning.
Output limits are not parse limits: check the truncation flags before
parsing a snapshot or JSON response. Invalid arguments, an unavailable
session and SDK/infrastructure failures reject the promise rather than
inventing a guest exit code. `BrowserSessionError` carries a `code`.

**Timeout/abort kills the CLI, not a command already received by the daemon.**
The daemon may still finish a browser action, and external effects cannot be
rolled back. Do not blindly retry an action such as form submission. Use
`stop()` to terminate the daemon and engine too when abandoning the session.
This adapter is not a command/domain permission policy: browser commands,
including `eval`, retain their normal capabilities. A model-facing tool must
apply its own authorization rules and treat stdout/stderr as untrusted data.

### `await browser.stop()`

Stops accepting work, terminates active CLI processes, drains queued/late-spawn
work, then terminates the daemon and engine in that order. Termination has a
250 ms grace period before the SDK forces exit, with `kill()` as a fallback
if signaling fails. It awaits confirmed SDK process exit, ends its log
observers, removes only its private directory, releases its port lease and
drops its process handles. Log observers stop at process exit even if a
pending SDK pipe read never receives EOF; final service logs are best-effort. Repeated calls share the same shutdown.
It also handles stop during startup. Cleanup failures reject and leave the
state failed rather than claiming that shutdown succeeded.

State progression: `idle → starting → ready → stopping → stopped`.
Startup or unexpected service exits can produce `failed`. An unexpected
engine/daemon exit prevents further commands and initiates cleanup of its
peer. The supplied sandbox and SDK client are **never** closed by the adapter.
Files written in its private working directory (including default screenshot
paths) disappear at stop; read them through `sandbox.fs` beforehand if needed.

## Verification

```bash
(cd harness && npm test)
ENGINE=html AGENT_BROWSER_WASM=target/agent-browser-wasix/agent-browser.wasm \
  ./scripts/verify-harness.sh
ENGINE=html LOADS=10 PAGE_URL=<public-page-url> \
  AGENT_BROWSER_WASM=target/agent-browser-wasix/agent-browser.wasm \
  ./scripts/verify-harness.sh
```

Recorded on 2026-10-12:

- 36 Node tests: argv/byte copies, serialization, output/status/truncation,
  stdin, timeout/abort, pending stdin writes, late spawns, startup rollback,
  private ownership, unrelated listeners, missing pipe EOF, unexpected exits,
  and shutdown errors.
- Small-page browser harness: eight checks including adapter-backed E8 and E10.
- NAVER with ten extra loads: nine checks including E9 and E10; allocated
  engine linear memory after the repeat segment was 341.5 MiB.
- Without agent-browser served, the existing HTML harness still passes its
  six checks using the original engine/probe path.

E10 uses the real SDK to check stdin evaluation, a nonzero exit with stderr,
truncated stdout, AbortSignal cancellation, timeout, idempotent stop and
rejection after stop. It also starts another independent session on a new
port in the same sandbox, navigates to `about:blank`, and stops it.

**SDK I/O cleanup can lag confirmed process exit.** With SDK 0.19.0, service
pipes sometimes never delivered EOF, and an immediate new engine on the old
port sometimes failed with `AddrInUse`. A released adapter lease is not a
guarantee that the runtime has already released its TCP binding. Use another
unused port or a separately isolated sandbox for an immediate new session;
otherwise allow time for runtime cleanup. The adapter never silently attaches
to the existing listener in place of starting its own engine.

**SDK client teardown is a separate limitation.** With SDK 0.19.0 the harness
observed `wasmer.close()` remaining pending after its sandbox was closed.
The harness awaits owned-process and sandbox shutdown, requests client
shutdown and waits at most one second for that separate step. If still
pending, it logs the fact; `verify-harness.sh` subsequently closes the Chrome
page. E10 proves the adapter's process lifecycle, not completion of SDK-client
worker teardown.
