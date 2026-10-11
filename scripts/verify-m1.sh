#!/usr/bin/env bash
# M1 acceptance test: fetch a real page over HTTP, parse it, and snapshot it.
#
#   ./scripts/verify-m1.sh
#
# What this proves that verify-m0.sh cannot: the engine holds a real document.
# The fixture page is served by a plain HTTP server, so the fetch path, the
# parser, the selector engine and the accessibility projection all run for real.
#
# The fixture carries an inline script, so the page's own JavaScript runs too,
# and `eval` goes through the real engine.
#
# Layout is approximate, so clicks and visibility are checked, not pixels.
#
# Screenshots are checked for what they are, PNGs of the right size, not for
# their pixels; the engine's own tests look at pixels.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CDP_PORT="${CDP_PORT:-9350}"
SITE_PORT="${SITE_PORT:-8123}"
SESSION="m1-verify-$$"
BASE="http://127.0.0.1:${CDP_PORT}"

export AGENT_BROWSER_SESSION="$SESSION"
export AGENT_BROWSER_NAMESPACE="$SESSION"

pass=0
fail=0

check() {
  local what="$1" expected="$2" actual="$3"
  if grep -q -- "$expected" <<<"$actual"; then
    printf '  ok   %s\n' "$what"
    pass=$((pass + 1))
  else
    printf '  FAIL %s\n       expected to contain: %s\n       got: %s\n' \
      "$what" "$expected" "$(tr '\n' '|' <<<"$actual" | cut -c1-300)"
    fail=$((fail + 1))
  fi
}

check_absent() {
  local what="$1" unexpected="$2" actual="$3"
  if grep -q -- "$unexpected" <<<"$actual"; then
    printf '  FAIL %s\n       should not contain: %s\n' "$what" "$unexpected"
    fail=$((fail + 1))
  else
    printf '  ok   %s\n' "$what"
    pass=$((pass + 1))
  fi
}

cleanup() {
  agent-browser --cdp "$BASE" close >/dev/null 2>&1 || true
  for pid in "${SERVER_PID:-}" "${SITE_PID:-}"; do
    [[ -n "$pid" ]] && { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  done
}
trap cleanup EXIT

echo "building"
cargo build --manifest-path "$ROOT/Cargo.toml" --quiet

echo "serving fixtures/index.html on 127.0.0.1:${SITE_PORT}"
(cd "$ROOT/fixtures" && exec python3 -m http.server "$SITE_PORT" --bind 127.0.0.1) \
  >/tmp/whb-m1-site.log 2>&1 &
SITE_PID=$!

echo "starting the engine with --engine html on ${CDP_PORT}"
"$ROOT/target/debug/wasm-headless-browser" --engine html "127.0.0.1:${CDP_PORT}" \
  >/tmp/whb-m1.log 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  curl -sf "$BASE/json/version" >/dev/null 2>&1 && break
  sleep 0.1
done
for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:${SITE_PORT}/" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "fetching and parsing"
open_out="$(agent-browser --cdp "$BASE" open "http://127.0.0.1:${SITE_PORT}/" 2>&1 || true)"
check "open reads the real title" "Real Page Fixture" "$open_out"
check "open reports the real url" "http://127.0.0.1:${SITE_PORT}/" "$open_out"

check "get title" "Real Page Fixture" "$(agent-browser --cdp "$BASE" get title 2>&1 || true)"
check "get url" "http://127.0.0.1:${SITE_PORT}/" "$(agent-browser --cdp "$BASE" get url 2>&1 || true)"

echo "accessibility tree from the parsed document"
snap="$(agent-browser --cdp "$BASE" snapshot 2>&1 || true)"
check "plain boxes are left out, their children kept, as in Chrome" '^- navigation "Main"' "$snap"
check "aria-label names the navigation" 'navigation "Main"' "$snap"
check "link names come from text" 'link "Docs"' "$snap"
check "headings report their level" 'heading "Real Page Fixture" \[level=1' "$snap"
check "a label names its textbox" 'textbox "Search terms"' "$snap"
check "a password field is a textbox" 'textbox "Password"' "$snap"
check "a checkbox reports its state" 'checkbox "Remember me".*checked=true' "$snap"
check "a button name from content" 'button "Go"' "$snap"
check "aria-label names a button" 'button "Close dialog"' "$snap"
check "list items" 'listitem \[level=1' "$snap"
check "their text is their children's" 'StaticText "first item"' "$snap"
check "table semantics" 'table' "$snap"
check "column headers" 'columnheader "Name"' "$snap"
check "cells" 'cell "alpha"' "$snap"
check "alt text names an image" 'image "A described image"' "$snap"

echo "the page's own script"
check "an inline script mutated the document" 'heading "Added by page script" \[level=2' "$snap"
check "eval runs real JavaScript" '^42$' "$(agent-browser --cdp "$BASE" eval '6 * 7' 2>&1 || true)"
check "eval sees the script's global" 'true' \
  "$(agent-browser --cdp "$BASE" eval 'typeof fixtureScriptRan !== "undefined" && fixtureScriptRan' 2>&1 || true)"
check "eval reads the document" 'Added by page script' \
  "$(agent-browser --cdp "$BASE" eval "document.querySelector('#scripted h2').textContent" 2>&1 || true)"

echo "console and errors"
check "the page's console.log reaches the client" '\[log\] fixture script ran {checks: 1}' "$(agent-browser --cdp "$BASE" console 2>&1 || true)"
check "an uncaught exception is reported" "fixture uncaught" "$(agent-browser --cdp "$BASE" --json errors 2>&1 || true)"

echo "history"
agent-browser --cdp "$BASE" open "http://127.0.0.1:${SITE_PORT}/?page=2" >/dev/null 2>&1 || true
check "back returns to the first page" "http://127.0.0.1:${SITE_PORT}/\$" \
  "$(agent-browser --cdp "$BASE" back >/dev/null 2>&1; agent-browser --cdp "$BASE" get url 2>&1 || true)"
check "the page's script runs again after back" 'heading "Added by page script"' \
  "$(agent-browser --cdp "$BASE" snapshot 2>&1 || true)"
check "forward goes to the second page" "page=2" \
  "$(agent-browser --cdp "$BASE" forward >/dev/null 2>&1; agent-browser --cdp "$BASE" get url 2>&1 || true)"

echo "layout and clicks"
check "a visible field is visible" "true" "$(agent-browser --cdp "$BASE" is visible '#q' 2>&1 || true)"
check "a hidden element is not" "false" "$(agent-browser --cdp "$BASE" is visible 'div[hidden]' 2>&1 || true)"
check "click lands on the checkbox" "Done" "$(agent-browser --cdp "$BASE" click '#remember' 2>&1 || true)"
check "and toggles it" "false" "$(agent-browser --cdp "$BASE" eval "document.getElementById('remember').checked" 2>&1 || true)"
check "clicking its label toggles it back" "true" \
  "$(agent-browser --cdp "$BASE" click 'label[for=remember]' >/dev/null 2>&1; agent-browser --cdp "$BASE" eval "document.getElementById('remember').checked" 2>&1 || true)"

echo "cookies"
check "a cookie can be set" "Done" "$(agent-browser --cdp "$BASE" cookies set whb_token abc 2>&1 || true)"
check "the jar returns it" "whb_token=abc" "$(agent-browser --cdp "$BASE" cookies get 2>&1 || true)"
check "page script sees the jar" "whb_token=abc; whb_lang=ko" \
  "$(agent-browser --cdp "$BASE" eval "document.cookie = 'whb_lang=ko'; document.cookie" 2>&1 || true)"
check "a script cookie reaches the jar" "whb_lang=ko" "$(agent-browser --cdp "$BASE" cookies get 2>&1 || true)"
agent-browser --cdp "$BASE" cookies clear >/dev/null 2>&1 || true
check_absent "clear empties the jar" "whb_" "$(agent-browser --cdp "$BASE" cookies get 2>&1 || true)"

echo "request interception (network route)"
ROUTE="http://127.0.0.1:${SITE_PORT}/route/"
ab() { agent-browser --cdp "$BASE" "$@" 2>&1 || true; }
ab network route "**/route/pic.png" --abort >/dev/null
ab network route "**/route/data.json" --body '{"mock": true}' >/dev/null
check "a page loads while its requests wait for the client" "Route Fixture" "$(ab open "$ROUTE")"
check "a fetch gets the client's body, with no content type as in Chrome" '200 null {"mock": true}' "$(ab get text '#out')"
check "an aborted image fails" "img error" "$(ab get text '#img')"
check "the abort is a failed request" "pic.png (Image)\$" "$(ab network requests --filter pic.png)"
ab network unroute "**/route/data.json" >/dev/null
ab reload >/dev/null
check "unroute lets the fetch through again" '200 application/json {"real":true}' "$(ab get text '#out')"
ab network route "**/route/style.css" >/dev/null
ab reload >/dev/null
check "a route with no action passes the request on" "rgb(255, 0, 0)\|red" \
  "$(ab eval "getComputedStyle(document.getElementById('out')).color")"
ab network unroute >/dev/null
ab reload >/dev/null
check "unroute with no url removes every route" "img loaded" "$(ab get text '#img')"
ab open "http://127.0.0.1:${SITE_PORT}/" >/dev/null

echo "what must stay out"
check_absent "hidden subtrees are excluded" "you should not see this" "$snap"

echo "screenshots"
SHOTS="$(mktemp -d)"
png_size() {
  python3 -c 'import struct,sys; d=open(sys.argv[1],"rb").read(24); print("PNG %dx%d" % struct.unpack(">II", d[16:24]) if d[:8]==b"\x89PNG\r\n\x1a\n" else "not a PNG")' "$1" 2>&1
}
check "a screenshot is saved" "Screenshot saved" "$(agent-browser --cdp "$BASE" screenshot "$SHOTS/view.png" 2>&1 || true)"
check "it is the viewport, as PNG" "PNG 800x600" "$(png_size "$SHOTS/view.png")"
agent-browser --cdp "$BASE" screenshot --full "$SHOTS/full.png" >/dev/null 2>&1 || true
check "a full-page one is as wide" "PNG 800x" "$(png_size "$SHOTS/full.png")"
check "annotation labels the refs" "\[[0-9]*\] @e[0-9]* " "$(agent-browser --cdp "$BASE" screenshot --annotate "$SHOTS/annotated.png" 2>&1 || true)"
rm -rf "$SHOTS"

echo
if ((fail > 0)); then
  echo "M1 FAILED: ${pass} passed, ${fail} failed"
  exit 1
fi
echo "M1 PASSED: ${pass} checks"
