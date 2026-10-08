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

## Status: M1 in progress

Two engines exist behind the same CDP surface, and the binary picks between
them with `--engine`:

| Engine | What it does | Command |
|---|---|---|
| `fake` (default) | Answers from a fixed page model. No HTML is parsed. | `./scripts/verify-m0.sh` |
| `html` | Fetches over HTTP, parses, and projects a real accessibility tree. | `./scripts/verify-m1.sh` |

`fake` stays the default so the M0 acceptance test keeps testing what it was
written against.

The same 16 M0 checks pass natively and inside WASIX:

| Mode | Command |
|---|---|
| Native, for fast iteration | `./scripts/verify-m0.sh` |
| WASIX module under Wasmer | `MODE=wasix ./scripts/verify-m0.sh` |

What works today, driven by the real `agent-browser` CLI:

```
$ agent-browser --cdp http://127.0.0.1:9333 open https://example.com
✓ Sandbox Login
  https://example.com

$ agent-browser --cdp http://127.0.0.1:9333 snapshot
- generic
  - heading "Sandbox Login" [level=1, ref=e1]
  - paragraph "Sign in to continue to the sandbox."
  - form
    - LabelText
      - StaticText "User"
    - textbox "User" [ref=e2]
    - LabelText
      - StaticText "Password"
    - textbox "Password" [ref=e3]
    - button "Sign in" [ref=e4]
  - link "More info" [ref=e5]
```

Also verified: navigation, `get url`, `get title`, `eval`, `scroll`, history
back and forward, `cookies get`, and `Page.captureScreenshot` returning a real
PNG that reflects the fake block layout.

The page behind those results comes from `engine-fake`, which answers from a
fixed model. It cannot run JavaScript. Two things therefore do not work yet:
free-form selectors resolved through page script (`find text ...`), and clicking.

The WASIX build is 272 KB, binds loopback inside the sandbox, and is reachable
from outside it. The largest open question going in — whether WASIX allows
loopback `bind`/`listen` at all — is answered yes, with the evidence in
[`docs/wasix.md`](docs/wasix.md).

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
  engine/         The real engine: arena DOM, parser, fetch, selectors, accessibility.
  engine-fake/    M0 engine: fixed page model, accessibility projection, PNG encoder.
  browser/        The binary. Wires a transport to an engine, chosen with --engine.
docs/
  cdp-surface.md          The measured protocol surface, and what it implies.
  wasix.md                What the WASIX run proved, and how it was measured.
  quickjs.md              Why Context::full aborts, and the context setup that works.
  backlog.md              What is measured, what is left, and what was decided against.
  capture/                Raw frames and expressions from a real Chrome session.
experiments/
  quickjs/                Probe: QuickJS evaluating JavaScript inside WASIX.
tools/cdp-log-proxy/      The logging proxy used to take that capture.
fixtures/                 Pages the M1 test serves over HTTP.
harness/                  Browser-sandbox experiments (E0-E5) and a runner page.
scripts/
  verify-m0.sh            M0 acceptance test, native or WASIX.
  verify-m1.sh            M1 acceptance test: fetch, parse, snapshot.
  verify-harness.sh       Runs the browser-sandbox probes and judges them.
  harness-stop.sh         Frees the harness ports if a manual run was abandoned.
  wasix-build.sh          Builds the WASIX module.
  wasix-run.sh            Runs it under Wasmer with networking enabled.
  wasix-netprobe.rs       Isolated probe for the loopback question.
  wasix-loopback-client.rs  Sandbox-side HTTP client, used for loopback and egress.
```

## Running it

```bash
cargo build
./target/debug/wasm-headless-browser 127.0.0.1:9222
```

Then point any CDP client at it. With `agent-browser`:

```bash
agent-browser --cdp http://127.0.0.1:9222 open https://example.com
agent-browser --cdp http://127.0.0.1:9222 snapshot
```

A bare port (`--cdp 9222`) also works; `host:port` without a scheme does not.

## Roadmap

| Milestone | Content |
|---|---|
| M0 | Protocol plumbing and the engine seam. **Done** |
| M1 | Real engine. HTML parsing, the DOM, fetching and the accessibility tree are **done**; approximate layout is not |
| M2 | A JavaScript engine, which is what makes clicking and arbitrary selectors work. QuickJS is chosen and proven to run; see [`docs/quickjs.md`](docs/quickjs.md) |
| M3 | Network: real fetches, `Network.*` events, cookies |
| M4 | Credentials held in memory only, injected at request time and never written to disk |

M1 is the first milestone where a real site can be read. M2 is where the cost
concentrates, because the two heaviest expressions a client sends are page
scripts, not protocol commands.

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
