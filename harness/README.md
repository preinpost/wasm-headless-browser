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

Two WASIX modules are loaded into one sandbox as commands: `engine` is the real
`wasm-headless-browser`, and `client` is `scripts/wasix-loopback-client.rs`,
which opens a TCP connection to `127.0.0.1`, speaks HTTP, and prints what came
back. The engine is spawned in the background; the client runs against it.

## Result

Recorded in [`../docs/wasix.md`](../docs/wasix.md). E0, E1 and E2 pass. E3 fails,
because `ports.expose` depends on a service worker hosted at
`https://default.local.wasmer.site` that never became ready. E3 is not needed:
E1 is the answer to the architectural question.

## Running it

```bash
cd harness
npm install
npm start                      # vendors the SDK, builds the probes, serves :8099
```

Then open `http://127.0.0.1:8099/?mode=http` in a browser. Chrome is required:
the SDK needs JSPI as well as `SharedArrayBuffer`. Results appear on the page and
on `window.__harness`.

Query parameters:

- `mode` — the sandbox's `network.mode`, default `http`. E3 only applies to `http`.
- `port` — the port the engine listens on, default `9222`.

To drive it headlessly and read the result:

```bash
agent-browser open "http://127.0.0.1:8099/?mode=http"
agent-browser eval "JSON.stringify(window.__harness)"
```

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
