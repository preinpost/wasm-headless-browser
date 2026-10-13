# wasm-headless-browser

A CDP-speaking headless web engine, compiled to WASIX for use in a Wasmer
sandbox.

## Purpose

**Let agent-browser use this project's WASM engine instead of Chrome/Chromium.**

agent-browser remains the automation client, with its existing commands. This
project supplies the web engine behind it: HTTP(S), DOM, JavaScript, CSS,
layout, screenshots and the CDP surface those commands need. The engine is
selected through agent-browser's existing `--cdp` connection, not a new
built-in browser type or an engine plugin inside agent-browser.

Existing agents, including pi, are consumers of agent-browser. Building a new
pi agent, web agent UI or model/tool loop is not this project's purpose. pi
runs `agent-browser` on the host, and that CLI drives this engine with
`--cdp`. The release artefact is the engine module alone.

The browser-hosted Wasmer sandbox cannot accept a WebSocket from the host
page (E3). Modules that put a client inside that sandbox are harness fixtures,
not part of the release. See [Artefacts](#artefacts).

This is an automation engine, not a browser for people or a promise of full
Chrome compatibility. It paints software screenshots but has no GUI or GPU
compositor. Replacing Chrome here means replacing the engine agent-browser
automates, not the host browser running the Wasmer SDK page.

## Artefacts

| File | Where it belongs |
|---|---|
| `wasm-headless-browser.wasm` | The release. WASIX build of the engine. A host `agent-browser --cdp` drives it. |
| `whb.wasm` | Test client in this repo. `verify-client.sh` and harness E7. Not released. |
| `agent-browser.wasm` | Harness fixture: agent-browser 0.38.2 patched for WASIX. E8, E10, E11. Not released. |
| `loopback-client.wasm` | Harness probe for sandbox loopback. Not released. |

None of these are committed. `scripts/wasix-build.sh` builds the engine and,
because it is the same workspace, the test client `whb`. `scripts/package-release.sh`
copies only `wasm-headless-browser.wasm` and a checksum into `dist/`.
`scripts/build-agent-browser-wasix.sh` writes the agent-browser fixture under
`target/agent-browser-wasix/`, and the harness copies whatever it needs into
`harness/public/`.

### Releasing

The version is the workspace semver in `Cargo.toml` (`0.0.1` today). Members
inherit it. The engine prints it: `wasm-headless-browser --version`.

Do not tag by hand. On `main`, run the **Release** workflow and choose a level:

| Level | From `0.0.1` |
|---|---|
| `patch` | `0.0.2` |
| `minor` | `0.1.0` |
| `major` | `1.0.0` |

The workflow bumps that version, builds the engine, and only then pushes
commit `Release vX.Y.Z`, tag `vX.Y.Z`, and a GitHub release whose assets are
`wasm-headless-browser.wasm` and `SHA256SUMS`. A failed build pushes nothing,
so it does not consume the version. `scripts/bump-version.sh` is the same bump
the workflow runs.

## Status: the core engine replacement works

The CDP replacement path is implemented and tested with agent-browser 0.38.2,
including a WASIX build of that CLI driving this engine inside a browser
sandbox (E8), repeated page loads (E9), and command/session lifecycle checks
(E10). That WASIX client is a harness fixture, not a release artefact. It
needs the platform patches and separately started daemon described below;
ordinary agent-browser commands still use `--cdp`.

In the browser sandbox, guest bash can run both from `PATH`
(`/usr/bin/engine`, `/usr/bin/agent-browser`) and drive a page with ordinary
shell commands (E11, `HARNESS_BASH=1`). Running pi itself there is a separate
consumer integration, not proof required of the web engine. Web-platform and CDP
compatibility are still partial; this is not a complete Chrome replacement
for every command or site.

`--engine html` is a real engine: it fetches over HTTP and HTTPS (TLS inside
the guest), parses HTML, runs the page's JavaScript in QuickJS, applies its
stylesheets, lays it out, and answers CDP from the result. A client can read a
page, fill it, click it and submit it, and the session survives in an
in-memory cookie jar. Credentials are typed by placeholder and never come back
to the client; see [`docs/credentials.md`](docs/credentials.md).

| Check | What it drives | Command |
|---|---|---|
| M0 | The protocol path, against a fixed page model (`--engine fake`, the default) | `./scripts/verify-m0.sh` |
| M1 | A real page: parsing, script, the accessibility tree, cookies, clicks | `./scripts/verify-m1.sh` |
| Login | A script-rendered login site, filled with secrets and submitted, with leak checks | `./scripts/verify-login.sh` |

Each runs natively and as the WASIX module under Wasmer (`MODE=wasix`), with
the real `agent-browser` CLI (0.38.2).

On a real React single-page app, a reference site's login screen opens in
about four seconds (3.5 native, 4.2 under WASIX), its 71 requests downloading
in parallel, lays out within a pixel or two of Chrome at the same
viewport, and takes `fill`, `click` and `press Enter`.

`Network.*` events are emitted, so `agent-browser network requests` and HAR
recording work, with the login POST's body showing placeholders.

Screenshots are painted from the same layout, with text in an embedded Noto
Sans KR (Hangul included), so `agent-browser screenshot`, `--full` and
`--annotate` work. A password field paints bullets; other secrets paint as
their placeholders.

Images are fetched as a browser fetches them: `<img>` loads fire before the
window's `load`, layout uses intrinsic sizes, and screenshots paint images
(PNG, JPEG, GIF, WebP), CSS backgrounds and gradients.

ES modules run: `<script type=module>`, static and dynamic `import`,
top-level `await` and import maps, so Vite-built sites hydrate.

`agent-browser console` and `errors` work: console calls and uncaught
exceptions are sent as Chrome sends them, object previews included.

Pages are decoded in whatever encoding they declare, EUC-KR included, as
a browser decides it, and `TextDecoder` takes every encoding label.

Pages scroll: script, `scrollIntoView`, the wheel and agent-browser's
`scroll` move the viewport and scroll containers, and boxes, clicks and
screenshots follow.

Screenshots paint box shadows and SVG, inline and as images (resvg).

Requests can be intercepted (`network route`, CDP's `Fetch` domain): failed,
answered by the client, or passed on changed, while the page waits for the
answer.

Not there yet: sticky positioning, text inside SVG, page-side WebAssembly
(QuickJS does not provide it), and pausing at the response stage. The WASIX
module is 6.3 MB, the font 1.15 MB and the SVG renderer 0.8 MB of it.

### Sandbox test clients

These are not a second product. The browser sandbox has no way in for a host
WebSocket (E3), so a probe that asks its questions inside that sandbox has to
load a client next to the engine. Two are checked there. Neither is released.

**agent-browser itself**, patched for WASIX and built by
`scripts/build-agent-browser-wasix.sh` into
`target/agent-browser-wasix/agent-browser.wasm` (11.4 MB). The harness uses it
for E8, E10 and E11, which replay the commands pi already types, but inside
the sandbox rather than on the host. Under WASIX the CLI cannot start its
daemon, so whatever starts the engine starts the daemon too; see
[`experiments/agent-browser`](experiments/agent-browser/README.md) for how,
for the two patches, and for the bug in the WASIX fork of mio that one of
them fixes.

**`whb`** (`crates/client`) is this repository's test client. Its commands and
output follow a subset of agent-browser's: `open`, `snapshot -i` with `@e3`
refs, `click`, `fill` (placeholders included), `press`, `eval`, `get`,
`screenshot`, `close`. Each command is its own process: the engine keeps the
tab, and a small state file per session keeps the refs, so a ref is refused
once the page it came from is gone. It is 273 KB as a WASIX module and uses
blocking sockets only. pi does not invoke it.

`scripts/verify-client.sh` runs the login flow with `whb`, natively and with
both sides under Wasmer (`MODE=wasix`). The harness runs whb (E7) and the
patched agent-browser (E8) in the browser sandbox.

**`BrowserSession`** (`harness/public/browser-session.js`) is the page-side
adapter: `start()`, `run(argv)` and `stop()` own the engine, daemon and CLI
processes, returning separate stdout/stderr, exit status, timeout/abort and
truncation flags. It needs no shell or pi integration. E8 uses it and E10
checks its lifecycle against the real SDK; see
[`docs/browser-session.md`](docs/browser-session.md).

## Why the shape is what it is

**No GPU compositor.** Automation primarily needs the DOM, JavaScript,
accessibility and box geometry. The same computed layout also feeds software
screenshots. This engine does not embed Chromium's renderer or require a
WASI GPU surface.

**CDP is the boundary.** Roughly 24 methods and 25 events are all a client
touches. Measured, not assumed: see [`docs/cdp-surface.md`](docs/cdp-surface.md).
Building to that boundary lets agent-browser use the engine for the tested
commands without changing their semantics. It does not imply support for the
entire Chrome protocol or every other CDP client.

**The engine is a trait.** `browser-core` owns protocol semantics and asks an
`Engine` for answers. `engine-fake` and `engine` implement the same trait, and
nothing above them changes. That is what made adding a second engine a matter
of a constructor rather than a rewrite.

**The DOM is an arena, not `Rc<RefCell<_>>`.** Node handles are indices into one
`Vec`. Indices are `Send`, which `Engine` requires, and they are the stable
handles M2 will hand to QuickJS. A test asserts `Document: Send` so a regression
is caught rather than discovered later.

**The transport is hand-rolled.** `cdp-wire` implements the WebSocket handshake
and RFC 6455 framing directly on `std::net`, with no async runtime. WASIX
supports tokio, but a single-client protocol does not need it, and fewer
dependencies means fewer WASIX surprises. SHA-1 is implemented in-tree rather
than pulled in, because the `sha1` crate reaches `libc`, which WASIX would then
have to port.

**Networking is the runtime's decision, not the engine's.** Under Wasmer, the
guest cannot create a socket unless the runtime allows it, and denial appears as
`EOPNOTSUPP` on the first `bind`. That reads like a WASIX limitation and is not
one. Details in [`docs/wasix.md`](docs/wasix.md).

## Layout

```
crates/
  cdp-wire/       HTTP discovery, RFC 6455 framing, SHA-1, JSON envelope.
  browser-core/   Targets, sessions, method dispatch. Engine-agnostic.
  engine/         The real engine: DOM, parser, fetch and TLS, cookies, selectors, CSS,
                  layout, accessibility, secrets, and QuickJS with the DOM prelude
                  (src/prelude/*.js).
  engine-fake/    M0 engine: fixed page model, accessibility projection, PNG encoder.
  browser/        The binary. Wires a transport to an engine, chosen with --engine.
  client/         whb, a test CDP client for the harness. Not a release artefact.
docs/
  cdp-surface.md          The measured protocol surface, and what it implies.
  wasix.md                What the WASIX run proved, and how it was measured.
  quickjs.md              Why Context::full aborts, and the context setup that works.
  credentials.md          How secrets are typed and kept from the client, and the limits.
  browser-session.md      Page-side command adapter: results, lifecycle and SDK limits.
  backlog.md              What is measured, what is left, and what was decided against.
  capture/                Raw frames and expressions from a real Chrome session.
experiments/
  quickjs/                Probe: QuickJS evaluating JavaScript inside WASIX.
  tls/                    Probe: rustls with ring inside WASIX.
  agent-browser/          agent-browser under WASIX: its patches, and the mio fix.
vendor/rquickjs-sys/      QuickJS, patched; see vendor/README.md.
tools/cdp-log-proxy/      The logging proxy used to take that capture.
fixtures/                 Pages the M1 test serves, and fixtures/login, a login site.
harness/                  Browser-sandbox experiments (E0-E10), adapter and Node tests.
scripts/
  verify-m0.sh            M0 acceptance test, native or WASIX.
  verify-m1.sh            M1 acceptance test: fetch, parse, script, snapshot, click.
  verify-login.sh         Login with secrets, with leak checks.
  verify-client.sh        The same login driven by whb, native or WASIX.
  verify-loads.sh         Repeated heavy loads and large CSS themes, native or WASIX.
  compare-layout.sh       Lays a page out here and in Chrome and compares the boxes.
  make-font.sh            Rebuilds the embedded font subset.
  verify-harness.sh       Runs the browser-sandbox probes and judges them.
  harness-stop.sh         Frees the harness ports if a manual run was abandoned.
  wasix-build.sh          Builds the WASIX engine. Also builds whb, a test client.
  package-release.sh      Copies only the engine wasm and a checksum into dist/.
  bump-version.sh         Bumps the workspace semver. The release workflow calls it.
  build-agent-browser-wasix.sh  Builds the agent-browser harness fixture, not a release.
  lib/llvm-ar.sh          Finds an llvm-ar that can archive wasm objects.
  wasix-run.sh            Runs it under Wasmer with networking enabled.
  wasix-netprobe.rs       Isolated probe for the loopback question.
  wasix-loopback-client.rs  Sandbox-side HTTP client, used for loopback and egress.
```

## Running it

```bash
cargo build --release
./target/release/wasm-headless-browser --engine html 127.0.0.1:9222
```

Then point the host `agent-browser` at it. That is the path pi uses:

```bash
agent-browser --cdp http://127.0.0.1:9222 open https://example.com
agent-browser --cdp http://127.0.0.1:9222 snapshot
```

A bare port (`--cdp 9222`) also works; `host:port` without a scheme does not.
The WASIX engine is the same server. `scripts/wasix-build.sh` writes
`target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm`, and
`scripts/wasix-run.sh` serves it. `scripts/package-release.sh` stages that
file alone in `dist/`.

`whb` can drive the same port, but it is a test client. `scripts/verify-client.sh`
uses it. Its wasm is not released.

To log in, pipe the credentials in and type placeholders
([`docs/credentials.md`](docs/credentials.md)):

```bash
security find-generic-password -s gw-password -w \
  | ./target/release/wasm-headless-browser --engine html --secret gw@https://gw.example.com 127.0.0.1:9222
agent-browser --cdp http://127.0.0.1:9222 fill @e3 '{{secret:gw}}'
```

### Watching it live

`scripts/watch.sh URL` opens a page and starts agent-browser's dashboard at
`http://localhost:4848`, which shows the engine's page as it changes and
sends clicks and keys back to it, so a person can watch an agent or drive
the page themselves:

```bash
./scripts/watch.sh https://gw.example.com
security find-generic-password -s gw-password -w \
  | ./scripts/watch.sh https://gw.example.com --secret gw@https://gw.example.com
```

The engine implements `Page.startScreencast` for this: after any command
that changed the page it paints a JPEG frame and sends it once the viewer
has acknowledged the previous one. The focused field shows a caret.

## Roadmap

| Milestone | Content |
|---|---|
| M0 | Protocol plumbing and the engine seam. **Done** |
| M1 | Real engine: parsing, DOM, fetching over HTTP(S), CSS cascade, layout, screenshots with images. **Done** |
| M2 | JavaScript: QuickJS (patched, see [`vendor/README.md`](vendor/README.md)), a DOM binding, an event loop, `fetch` and XHR, input, ES modules. **Done** |
| M3 | Network: cookies, `Network.*` events and request interception (`Fetch.*`). **Done** |
| M4 | Credentials held in memory only, typed by placeholder, scrubbed from CDP. **Done** |

The working log, with what was measured and what is left, is
[`docs/backlog.md`](docs/backlog.md).

## Open questions

- **Does `@wasmer/sdk` share loopback with the host page?** It does not need
  to: the engine and its client share one sandbox (E1), and whb and
  agent-browser drive the engine there (E7, E8).
- **How much of Playwright is reachable?** Playwright leans harder on page-side
  JavaScript than `agent-browser` does, so it needs a more complete engine.
- **Do we compile QuickJS against the WASIX sysroot?** It currently builds with
  wasi-sdk's, and one libc constant diverges, which is worked around by skipping
  `JS_AddPerformance`. Installing `wasixcc` would fix it properly. See
  [`docs/quickjs.md`](docs/quickjs.md).
