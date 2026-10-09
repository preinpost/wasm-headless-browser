# Credentials

How to let a client log in without the client, or anything it writes, ever
holding the password. Implemented in `crates/engine/src/secrets.rs`; checked
end to end by `scripts/verify-login.sh`.

## Using it

Give the engine each credential a name and the one origin allowed to receive
it, and pipe the values in, one line per `--secret`, in order:

```bash
security find-generic-password -s gw-password -w \
  | wasm-headless-browser --engine html --secret gw@https://gw.example.com 127.0.0.1:9222
```

Under WASIX, `scripts/wasix-run.sh` takes the same arguments.

The client then types a placeholder instead of the value:

```bash
agent-browser --cdp http://127.0.0.1:9222 fill @e3 '{{secret:gw}}'
agent-browser --cdp http://127.0.0.1:9222 press Enter
```

Use `fill`. `type` sends one character at a time, so a placeholder typed that
way never matches and is typed literally.

Do not use agent-browser's auth vault. It saves credentials to disk, which is
what this exists to avoid.

## What it guarantees

- **The value only arrives on stdin.** Not as an argument, which `ps` shows;
  not from the environment, which child processes and crash reports inherit;
  not from a file. A terminal on stdin is refused, because it would echo the
  value as it is typed.
- **It is typed only into its origin.** `Input.insertText` replaces
  `{{secret:NAME}}` with the value when the page's origin is the one given,
  scheme, host and port. Anywhere else the method fails with an error naming
  the secret and both origins, and nothing is typed.
- **It never comes back over CDP.** Every result, error and event the engine
  sends, and the target's title and URL, has each value replaced by its
  placeholder. A client that reads the field (`input.value`), snapshots a page
  showing a username, or sees an error message quoting the field gets
  `{{secret:NAME}}`. This holds for events that do not exist yet, such as
  `Network.requestWillBeSent` with a POST body, because the scrub is applied
  to all outgoing traffic rather than method by method.
- **It never reaches a log.** `WHB_TRACE_CDP` logs typed text as its length,
  and everything else it logs has already been scrubbed. The engine writes
  no files at all: cookies and `localStorage` live in memory.
- **It does not outlive the process in freed memory.** Values are overwritten
  when the engine drops them.

## What it does not guarantee

- **Not against a hostile client.** A client that runs page script can leak a
  value a character at a time, for example `value.charCodeAt(0)`, and the
  scrub cannot recognise that. The guarantee is about accidents: a value does
  not end up in a transcript, a snapshot, a log or a bug report.
- **Not against the page.** The page reads the value, as it must to log in,
  and sends it where its own script decides. Binding a secret to an origin is
  what stops it reaching the wrong page.
- **Not in the page's memory.** The page copies the value into its own
  JavaScript strings, which cannot be wiped. They go with the page: a
  navigation discards the document's script context.
- **Short values scrub broadly.** Every occurrence is replaced, so a username
  like `kim` also turns `skim` into `s{{secret:user}}`.

## Leak paths checked

The M4.3 list, and where each stands.

| Path | Status | How it is checked |
|---|---|---|
| CDP results and errors | Scrubbed | `verify-login.sh` reads the field back and gets the placeholder; an engine test throws an error quoting it |
| CDP events | Scrubbed | Same path as results; covered by the engine tests |
| Accessibility tree and snapshots | Scrubbed, and a password field never shows its value | `verify-login.sh` keeps every snapshot and checks the transcript |
| Engine stderr, including `WHB_TRACE_CDP` | Never written | `verify-login.sh` runs traced and checks stderr |
| Disk | Never written | The engine has no file writes |
| Console output from the page | Kept in memory, not sent | Will be scrubbed with everything else once `Runtime.consoleAPICalled` is emitted |
| Screenshots | Not applicable yet | There is no rasterizer. When there is, a password field must paint bullets |
| Crash output | Not checked | Values are not part of any panic message the engine raises, but a panic in a dependency is not controlled |
