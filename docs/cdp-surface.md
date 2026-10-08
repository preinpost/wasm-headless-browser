# The CDP surface a client actually uses

Everything on this page was measured, not guessed. The method of measurement:

1. Launch Chrome with `--remote-debugging-port=9222`.
2. Put a logging proxy in front of it (`tools/cdp-log-proxy/proxy.ts`) and point
   `agent-browser --cdp` at the proxy.
3. Run a normal browsing loop and record every WebSocket frame.

Raw material:

| File | Contents |
|---|---|
| `capture/frames.jsonl` | Every HTTP path and every WebSocket frame, in order |
| `capture/required-expressions.txt` | Every `Runtime.evaluate` expression, deduplicated |

Environment: Chrome 154.0.8037.98, agent-browser 0.38.2, macOS.

## Headline numbers

Measured over `connect` → `open` → `snapshot` → `click` → `type` → `eval` →
`get text` → `is visible` → `scroll` → `back` → `forward` → `reload` →
`press` → `tab new` → `tab list` → `cookies get` → `screenshot`:

- **24 distinct CDP methods sent**
- **25 distinct events received**
- **0 error responses**

That is a small protocol surface. A Chromium-compatible engine sounds
enormous; the part a client actually drives is not.

## Transport shape

- **One WebSocket per client session.** Both recorded sessions opened exactly
  one socket and multiplexed everything over it.
- **`sessionId` on almost every message.** 81 of 108 sent messages carried one.
  The client uses `Target.setAutoAttach` with `flatten: true`, so target-scoped
  commands travel over the browser-level socket.
- **Session-scoped responses echo the `sessionId`.** Confirmed on 85 of 85
  session-scoped responses. Omitting it breaks the client's request matching.
- **Only one HTTP endpoint was needed:** `GET /json/version`. Targets are
  discovered through `Target.getTargets`, not `/json/list`.
- Ids are 32-character uppercase hex: target ids, session ids and browser
  context ids all follow this shape.

## Methods sent

| Method | Count | M0 answers |
|---|---:|---|
| `Runtime.evaluate` | 20 | rule set |
| `Browser.getVersion` | 18 | yes |
| `WebMCP.enable` | 9 | yes (blank) |
| `Runtime.runIfWaitingForDebugger` | 8 | yes (blank) |
| `Network.enable` | 6 | yes (blank) |
| `Page.enable` | 6 | yes (blank) |
| `Runtime.enable` | 6 | yes |
| `Target.setAutoAttach` | 6 | yes |
| `Page.getFrameTree` | 4 | yes |
| `Target.attachToTarget` | 4 | yes |
| `Input.dispatchMouseEvent` | 3 | yes (blank) |
| `Page.navigate` | 2 | yes |
| `Target.getTargets` | 2 | yes |
| `Target.setDiscoverTargets` | 2 | yes |
| `Input.dispatchKeyEvent` | 2 | yes (blank) |
| `Input.insertText` | 2 | yes (blank) |
| `Accessibility.enable` | 1 | yes |
| `Accessibility.getFullAXTree` | 1 | yes |
| `DOM.enable` | 1 | yes (blank) |
| `Network.getCookies` | 1 | yes |
| `Page.captureScreenshot` | 1 | yes |
| `Page.reload` | 1 | yes |
| `Target.createTarget` | 1 | yes |
| `Runtime.callFunctionOn` | 1 | partial |

"Blank" means the method returns `{}` and the client carries on. Unknown
methods also return `{}` rather than an error, because clients probe optional
domains constantly and an error response makes them abort more often than a
blank one does.

## Events received

Grouped by how much work they imply.

**Cheap, and M0 emits them:** `Target.targetCreated`,
`Target.targetDestroyed`, `Target.attachedToTarget`, `Target.targetInfoChanged`,
`Page.frameStartedLoading`, `Page.frameNavigated`, `Page.loadEventFired`,
`Page.domContentEventFired`, `Runtime.executionContextCreated`,
`Runtime.executionContextsCleared`, `Accessibility.loadComplete`.

**Requires a real network stack:** `Network.requestWillBeSent`,
`Network.requestWillBeSentExtraInfo`, `Network.responseReceived`,
`Network.responseReceivedExtraInfo`, `Network.dataReceived`,
`Network.loadingFinished`, `Network.requestServedFromCache`,
`Network.policyUpdated`. The client used these to build HAR files and route
requests. M0 emits none of them.

**Cosmetic:** `Page.frameStartedNavigating`, `Page.frameRequestedNavigation`,
`Page.frameScheduledNavigation`, `Page.frameClearedScheduledNavigation`,
`DOM.documentUpdated`.

## What the client asks JavaScript to do

This is the part that decides how much engine is really needed. The full list is
in `capture/required-expressions.txt`. Summarised:

**Trivial:**

```js
1
1+1
location.href
document.title
document.querySelector("a")
document.body
window.scrollBy(0, 300)
history.back()
history.forward()
```

**Cheap but real:** `Runtime.callFunctionOn` against a returned `objectId`, for
`this.focus()`. This forces remote object handles.

**Expensive.** Two expressions do most of the damage:

1. A scan that walks `document.body.querySelectorAll('*')` and, for each
   element, calls `getComputedStyle(el)`, reads `tagName`, `getAttribute`,
   `hasAttribute`, `onclick`, `textContent`, `parentElement`, `closest`,
   `hidden`, `checked`, `indeterminate`, and `getBoundingClientRect()`. It also
   calls `setAttribute` to tag its own results.

2. A click-point resolver that calls `getBoundingClientRect()`, compares against
   `window.innerHeight` and `document.documentElement.clientHeight`, calls
   `scrollIntoView`, and then hit-tests with `document.elementFromPoint()`, with
   a nested walk into same-origin `iframe` `contentDocument`.

## What this implies for the engine

- **`getBoundingClientRect()` is the single feature that forces layout.** Not
  pixels: a client only asks whether the box is non-empty and where its centre
  is. Approximate boxes from a simple block or flex layout are sufficient.
- **`getComputedStyle()` is the single feature that forces CSS.** The scan only
  reads `cursor`, `display`, and `visibility`, but a general implementation
  means a cascade.
- **`elementFromPoint()` requires a real hit-test,** which requires real boxes.
- **Anything that evaluates arbitrary expression strings requires a JavaScript
  engine.** There is no way to answer expression 1 above with a pattern match
  once the page is not the one you hard-coded. M0 gets away with a rule set
  precisely because its page is fixed.
- **Click automation is therefore the expensive feature, and crawling is not.**
  Navigation, the accessibility tree, and `Runtime.evaluate` are cheap;
  clicking is what drags in CSS, layout, and hit-testing.

## Reproducing

```bash
# Chrome with a debugging port
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --remote-debugging-port=9222 --user-data-dir=/tmp/cdpprofile about:blank &

# logging proxy in front of it
UP="ws://127.0.0.1:9222/devtools/browser/<id>" bun run tools/cdp-log-proxy/proxy.ts

# drive the proxy
agent-browser --cdp http://127.0.0.1:9223 open https://example.com
agent-browser --cdp http://127.0.0.1:9223 snapshot
```

Note that `agent-browser --cdp` takes a bare port or a full URL. A
`host:port` pair without a scheme is rejected.
