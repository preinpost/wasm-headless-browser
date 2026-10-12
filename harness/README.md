# Browser harness

Questions about the `@wasmer/sdk` browser sandbox cannot be answered from
outside it, so they get a page and an experiment.

## The question it answers

Can a WASIX process listen on loopback inside the browser sandbox, and can
something else reach it?

This decides the architecture. If loopback works inside one sandbox, the engine
and its automation client can both live there and talk over `127.0.0.1`. If it
does not, CDP has to be carried over a pipe, or the client has to run outside.

## What it does

| Step | Question |
|---|---|
| E0 | Does the engine bind at all inside the sandbox? |
| E1 | Can a second command **in the same sandbox** reach it over loopback? |
| E2 | Does the SDK observe the guest listener (`ports.wait`)? |
| E3 | Can the **host page** reach it (`ports.expose` + `fetch`)? |
| E4 | Can the sandbox reach the public internet at all? |
| E5 | Can the sandbox complete a real HTTPS request? |
| E6 | Does the **real engine** load and script a page inside the sandbox? |
| E7 | Does **whb**, one process per command, drive the engine there? |
| E8 | Does **agent-browser** itself, patched for WASIX, drive it there? |
| E9 | Do repeated loads in the same engine still navigate and evaluate? |
| E10 | Does the browser adapter return structured results and stop what it owns? |

Two WASIX modules are loaded into one sandbox as commands: `engine` is the real
`wasm-headless-browser`, and `client` is `scripts/wasix-loopback-client.rs`,
which opens a TCP connection to `127.0.0.1`, speaks HTTP, and prints what came
back. The engine is spawned in the background; the client runs against it.

The engine binary starts its M0 fake engine unless told otherwise, and E0-E2
ask only whether it serves. `engine=html` starts the real one (QuickJS, TLS,
layout) instead, and adds E6: the client's `cdp` mode opens a WebSocket to the
engine over loopback and sends `Target.createTarget`, `Target.attachToTarget`,
`Page.navigate` and `Runtime.evaluate`. The engine fetches the page through the
WISP relay with TLS in the guest and runs its scripts in QuickJS, and the
evaluated value comes back.

E7 runs too when `whb.wasm` is served: `whb open`, `whb snapshot -i` and
`whb eval`, each as its own command, as an agent in the sandbox would run
them. They share the tab and the refs through a state file under
`/workspace`: the SDK's sandbox filesystem, which every command sees. A
command's `/tmp` is its own, and the SDK refuses paths outside `/workspace`.

E8 runs when `public/agent-browser.wasm` is served, which `verify-harness.sh`
does for `AGENT_BROWSER_WASM=<path>` (build it with
`../scripts/build-agent-browser-wasix.sh`). The page uses `BrowserSession`
(`public/browser-session.js`) to start the engine and agent-browser daemon,
since the CLI cannot spawn its daemon under WASIX. It then runs `open`,
`snapshot -i` and `eval` through the adapter as separate CLI processes.
See [`../docs/browser-session.md`](../docs/browser-session.md) for the
interface and [`../experiments/agent-browser`](../experiments/agent-browser/README.md)
for the WASIX build.

E10 runs alongside E8. It checks stdin evaluation, a nonzero exit with stderr,
output truncation, AbortSignal cancellation, SDK timeout, idempotent stop,
refusal after stop and a fresh session in the same sandbox on another port.
Only processes and the private directory created by the adapter are its
responsibility; the page owns and closes the sandbox.

E4 and E5 are a different question from E0-E3, and need the WISP relay, because
the browser sandbox has no direct network access. E4 opens a plain TCP
connection to a public host. E5 runs the `curl/curl` package inside the sandbox,
so TLS terminates **in the guest**, and reports the status line plus a slice of
the response.

## Result

Recorded in [`../docs/wasix.md`](../docs/wasix.md).

E0, E1 and E2 pass. E1 is the answer to the architectural question: the engine
and its client can share one sandbox.

E3 fails, because `ports.expose` depends on a service worker hosted at
`https://default.local.wasmer.site` that never became ready. It is not needed,
and it would not have sufficed anyway: the ingress path is request/response HTTP
only, with no WebSocket upgrade, so CDP could not pass through it.

E4 and E5 pass.

E6 passes with the real engine (2026-10-12, a 6,331,554-byte module):
`https://example.com/` comes back as `{"title":"Example Domain","links":1}`,
the link made by the page's external script, and `https://www.naver.com/` as
`{"title":"NAVER","links":172}`. The whole NAVER run of `verify-harness.sh`,
server and sandbox start included, took about 23 seconds; natively the page
alone loads in about 4.7. The page load in the sandbox was not timed apart.

E7 and E8 pass on both the small page and NAVER (2026-10-12). E8 uses
agent-browser 0.38.2 with both patches, 11.4 MB, in the browser sandbox.

Repeated heavy loads initially failed: a third load under the Wasmer CLI
panicked in `thread_spawn` (`Errno::overflow`), and a second in the browser
killed the engine. Fetches now reuse twelve process-wide workers (six per
host), rather than starting a thread for each request, including waiters.
That exposed another memory problem: computed styles copied thousands of
inherited CSS variables into every element and pseudo-element. The custom
property map and its immutable strings are now shared. Script threads still
use 32 MB stacks, reused across navigations.

With `LOADS=10`, E9 keeps one tab and WebSocket, navigating and evaluating
after each load. NAVER passes E6, E7, E8 and those ten extra loads in the
same engine (nine checks including adapter-backed E8 and E10); allocated
linear memory after the repeat segment was 341.5 MiB. This cannot shrink
and is not the live heap. `RESOURCES=1` prints it and QuickJS usage.

## Running it

The easy way drives and judges everything:

```bash
../scripts/verify-harness.sh
OUTBOUND=login.example.com:443 CURL=https://login.example.com/ ../scripts/verify-harness.sh
ENGINE=html ../scripts/verify-harness.sh                      # adds E6
ENGINE=html LOADS=10 RESOURCES=1 PAGE_URL=https://www.naver.com/ ../scripts/verify-harness.sh
ENGINE=html AGENT_BROWSER_WASM=../target/agent-browser-wasix/agent-browser.wasm \
  ../scripts/verify-harness.sh                                 # adds E8 and E10
```

The adapter has 36 Node tests, requiring no Wasmer or browser:

```bash
(cd harness && npm test)   # from the repository root
```

The runner starts the harness and the WISP relay, refreshes `public/engine.wasm` when
the WASIX build differs from it (and the client when its source is newer),
opens the page, polls until the page reports it is done, and asserts on each
expected probe. It cleans up after itself; use `../scripts/harness-stop.sh` if
something was started by hand.

By hand:

```bash
cd harness
npm install
npm start                      # vendors the SDK, builds the probes, serves :8099
npm run wisp                   # in another shell, relay on :5010
```

Then open a URL. Chrome is required: the SDK needs JSPI as well as
`SharedArrayBuffer`. Results appear on the page and on `window.__harness`.

```
http://127.0.0.1:8099/?mode=wisp&wisp=ws://127.0.0.1:5010/&outbound=example.com:80
http://127.0.0.1:8099/?mode=wisp&wisp=ws://127.0.0.1:5010/&outbound=login.example.com:443&curl=https://login.example.com/
http://127.0.0.1:8099/?mode=wisp&wisp=ws://127.0.0.1:5010/&outbound=example.com:80&engine=html&page=https://example.com/
```

Query parameters:

- `mode` — the sandbox's `network.mode`, default `http`. `wisp` is what E4 and
  E5 need. E3 only applies to `http`.
- `wisp` — the relay URL.
- `port` — the port the engine listens on, default `9222`.
- `outbound` / `outboundPath` — the E4 target.
- `curl` — the E5 URL.
- `engine` — passed to the binary as `--engine`; unset starts its default, the
  M0 fake engine. `html` starts the real engine and runs E6.
- `page` — the page E6–E9 load, default `https://example.com/`.
- `loads` — additional navigations in E9; greater than 1 enables it. `LOADS` in the runner.
- `resources=1` — guest resource tracing; `RESOURCES=1` in the runner. Reports
  allocated linear memory and QuickJS usage without logging page content.

## Notes

- The page must be cross-origin isolated (`COOP: same-origin`,
  `COEP: require-corp`). `serve.mjs` sets both; without them the SDK refuses to
  start.
- `proc.stdout` is a `ReadableBytes`, not a `ReadableStream`. It exposes an async
  generator of lines, `lines()`, which is what the harness uses.
- `@wasmer/sdk` must be served with its original file layout, because it locates
  its worker and wasm-bindgen module relative to `import.meta.url`.
  `vendor-sdk.mjs` copies it and bundles only the entry point, to inline its one
  bare import.
- Reading the result back through `agent-browser eval` double-encodes it: the
  result arrives as a JSON string, so inner quotes are escaped and a shell test
  for `"done":true` will not match. The page exposes `window.__harnessDone` as a
  bare boolean for exactly this reason.
- `curl/curl` has to be fetched before the sandbox is created, because a
  sandbox's package set is fixed at creation time.
- SDK 0.19.0 sometimes leaves service pipe EOF pending after confirmed
  process exit, and immediate reuse of the old TCP port can fail. The adapter
  bounds log observers by process exit; E10 uses a different port for an
  immediate fresh session. See the adapter doc for the implications.
- SDK `wasmer.close()` remained pending even after `sandbox.close()` completed
  in a measured run. The page requests client shutdown and waits at most one
  second, logging when it is still pending. The runner then closes the Chrome
  page. E10 does not assert completion of SDK-client worker teardown.
