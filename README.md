# wasm-headless-browser

A CDP-speaking headless browser, built to run inside a WASIX sandbox so that
browser automation can happen where the credentials are.

The immediate goal is modest: when a task needs a little crawling, do it inside
the sandbox. A user hands over an id and password, those live in memory only,
and a browser automation client drives the requests. The longer-term goal is
that a coding agent running in the same sandbox can reach for it as a tool.

It is not a browser for people. There is no window, no compositor, and no
rendering pipeline. It speaks the Chrome DevTools Protocol so that existing
clients work unchanged.

## Status: it logs in

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

On a real React single-page app, the reference site's login screen opens in
about five seconds, lays out within a pixel or two of Chrome at the same
viewport, and takes `fill`, `click` and `press Enter`.

Not there yet: screenshots (no rasterizer), scrolling, ES modules, and
`Network.*` events. The WASIX module is 3.3 MB.

## Why the shape is what it is

**No rendering engine.** A headless browser for automation needs the DOM, a
JavaScript engine, an accessibility tree, and approximate box geometry. It does
not need pixels. Dropping the rendering pipeline also removes the WASI GPU
problem, since WASI has no surface interface yet and `wasi:webgpu` is still a
proposal.

**CDP is the boundary.** Roughly 24 methods and 25 events are all a client
touches. Measured, not assumed: see [`docs/cdp-surface.md`](docs/cdp-surface.md).
Building to that boundary means any existing client works, and it means the
engine can be replaced under it without touching clients.

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
docs/
  cdp-surface.md          The measured protocol surface, and what it implies.
  wasix.md                What the WASIX run proved, and how it was measured.
  quickjs.md              Why Context::full aborts, and the context setup that works.
  credentials.md          How secrets are typed and kept from the client, and the limits.
  backlog.md              What is measured, what is left, and what was decided against.
  capture/                Raw frames and expressions from a real Chrome session.
experiments/
  quickjs/                Probe: QuickJS evaluating JavaScript inside WASIX.
  tls/                    Probe: rustls with ring inside WASIX.
vendor/rquickjs-sys/      QuickJS, patched; see vendor/README.md.
tools/cdp-log-proxy/      The logging proxy used to take that capture.
fixtures/                 Pages the M1 test serves, and fixtures/login, a login site.
harness/                  Browser-sandbox experiments (E0-E5) and a runner page.
scripts/
  verify-m0.sh            M0 acceptance test, native or WASIX.
  verify-m1.sh            M1 acceptance test: fetch, parse, script, snapshot, click.
  verify-login.sh         Login with secrets, with leak checks.
  verify-harness.sh       Runs the browser-sandbox probes and judges them.
  harness-stop.sh         Frees the harness ports if a manual run was abandoned.
  wasix-build.sh          Builds the WASIX module.
  wasix-run.sh            Runs it under Wasmer with networking enabled.
  wasix-netprobe.rs       Isolated probe for the loopback question.
  wasix-loopback-client.rs  Sandbox-side HTTP client, used for loopback and egress.
```

## Running it

```bash
cargo build --release
./target/release/wasm-headless-browser --engine html 127.0.0.1:9222
```

Then point any CDP client at it. With `agent-browser`:

```bash
agent-browser --cdp http://127.0.0.1:9222 open https://example.com
agent-browser --cdp http://127.0.0.1:9222 snapshot
```

A bare port (`--cdp 9222`) also works; `host:port` without a scheme does not.

To log in, pipe the credentials in and type placeholders
([`docs/credentials.md`](docs/credentials.md)):

```bash
security find-generic-password -s gw-password -w \
  | ./target/release/wasm-headless-browser --engine html --secret gw@https://gw.example.com 127.0.0.1:9222
agent-browser --cdp http://127.0.0.1:9222 fill @e3 '{{secret:gw}}'
```

## Roadmap

| Milestone | Content |
|---|---|
| M0 | Protocol plumbing and the engine seam. **Done** |
| M1 | Real engine: parsing, DOM, fetching over HTTP(S), CSS cascade, layout. **Done**, except screenshots |
| M2 | JavaScript: QuickJS (patched, see [`vendor/README.md`](vendor/README.md)), a DOM binding, an event loop, `fetch` and XHR, input. **Done**, except ES modules |
| M3 | Network: cookies are **done**; `Network.*` events and request interception are not |
| M4 | Credentials held in memory only, typed by placeholder, scrubbed from CDP. **Done** |

The working log, with what was measured and what is left, is
[`docs/backlog.md`](docs/backlog.md).

## Open questions

- **Does `@wasmer/sdk` share loopback with the host page?** Under the Wasmer CLI
  it does, so a client outside the sandbox can reach the engine. The browser
  sandbox is a different runtime and needs its own check. If it does not,
  running both sides inside the sandbox is already known to work.
- **How much of Playwright is reachable?** Playwright leans harder on page-side
  JavaScript than `agent-browser` does, so it needs a more complete engine.
- **Do we compile QuickJS against the WASIX sysroot?** It currently builds with
  wasi-sdk's, and one libc constant diverges, which is worked around by skipping
  `JS_AddPerformance`. Installing `wasixcc` would fix it properly. See
  [`docs/quickjs.md`](docs/quickjs.md).
