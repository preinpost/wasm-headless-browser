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
  `{{secret:NAME}}`. The scrub is applied to all outgoing traffic rather
  than method by method, so it covers `Network.requestWillBeSent` with the
  login POST's body, `Network.getResponseBody`, and whatever is added later.
- **Encoded copies are caught too.** A page sends the value escaped in a
  JSON string or percent-encoded in a form, so `p@ss"1` goes out as
  `p@ss\"1` or `p%40ss%221`. The scrub looks for those forms as well as the
  value as typed. A response body sent base64-encoded is scrubbed as bytes
  before it is encoded, where the string scrub could not see it.
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
- **Other encodings are not caught.** Base64 of the value inside a page's
  own request, a hash of it, or a value split across two strings goes out as
  it is. The JSON and form encodings are the ones a login form produces.
- **A routed request changed by the client goes as the client wrote it.**
  The client sees a login `POST` with the placeholder in it. Continued as it
  is, the real value goes to the server; continued with a `postData` of the
  client's own, that is what goes, and a placeholder in it is not filled in.
  A client that answers a request itself (`Fetch.fulfillRequest`) can serve
  a page of its choosing under the secret's origin, which is a hostile
  client, above.
- **A client's HAR file is written by the client.** Its `postData` holds the
  placeholder, as `verify-login.sh` checks, but where the file goes is the
  client's business.
- **Short values scrub broadly.** Every occurrence is replaced, so a username
  like `kim` also turns `skim` into `s{{secret:user}}`.

- **What a person types into the dashboard is not a secret to the engine.**
  Typed key by key in `scripts/watch.sh`'s live view, a password travels as
  key events from agent-browser to the engine, in memory, and the field
  shows bullets. To have it scrubbed from what the engine sends, give the
  same value with `--secret` too.

## Leak paths checked

The M4.3 list, and where each stands.

| Path | Status | How it is checked |
|---|---|---|
| CDP results and errors | Scrubbed | `verify-login.sh` reads the field back and gets the placeholder; an engine test throws an error quoting it |
| CDP events | Scrubbed | Same path as results; covered by the engine tests |
| `Network.*` events and response bodies | Scrubbed, encoded forms included | `verify-login.sh` records a HAR through the login and checks the POST's `postData` and the HAR file on disk; an engine test uses a password with a quote in it |
| `Fetch.requestPaused` (`network route`) | Scrubbed: the paused request's `postData` holds the placeholder | It goes out through the same scrub as other events, though it is sent while a command runs; an engine test routes a login `POST` and reads the paused body |
| Accessibility tree and snapshots | Scrubbed, and a password field never shows its value | `verify-login.sh` keeps every snapshot and checks the transcript |
| Engine stderr, including `WHB_TRACE_CDP` | Never written | `verify-login.sh` runs traced and checks stderr |
| Disk | Never written | The engine has no file writes |
| Console output and uncaught exceptions | Scrubbed: `Runtime.consoleAPICalled` and `Runtime.exceptionThrown` go out through the same exit | An engine test logs the password and gets the placeholder |
| Screencast frames | Painted like screenshots | An engine test runs a screencast; frames come from the same painter |
| Binary payloads | Scrubbed before encoding, then left alone | A screenshot's `data`, a frame's `data` and a base64 response body skip the string scrub, which could match a short secret inside base64 by chance and corrupt the payload. An engine test uses a secret taken from a screenshot's own base64 |
| Screenshots | A password field paints a bullet per character; other text is scrubbed before it is painted, so a username shows as its placeholder | An engine test paints the secret and a same-length other value and compares the images, and paints a page showing the username against one showing the placeholder |
| Crash output | Not checked | Values are not part of any panic message the engine raises, but a panic in a dependency is not controlled |
