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
# Not covered, and asserted as such: layout. Screenshots and input are expected
# to fail with a message that says why.
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
check "root web area is named from <title>" "generic" "$snap"
check "aria-label names the navigation" 'navigation "Main"' "$snap"
check "link names come from text" 'link "Docs"' "$snap"
check "headings report their level" 'heading "Real Page Fixture" \[level=1' "$snap"
check "a label names its textbox" 'textbox "Search terms"' "$snap"
check "a password field is a textbox" 'textbox "Password"' "$snap"
check "a checkbox reports its state" 'checkbox "Remember me".*: true' "$snap"
check "a button name from content" 'button "Go"' "$snap"
check "aria-label names a button" 'button "Close dialog"' "$snap"
check "list items" 'listitem "first item"' "$snap"
check "table semantics" 'table' "$snap"
check "column headers" 'columnheader "Name"' "$snap"
check "cells" 'cell "alpha"' "$snap"
check "alt text names an image" 'img "A described image"' "$snap"

echo "the page's own script"
check "an inline script mutated the document" 'heading "Added by page script" \[level=2' "$snap"
check "eval runs real JavaScript" '^42$' "$(agent-browser --cdp "$BASE" eval '6 * 7' 2>&1 || true)"
check "eval sees the script's global" 'true' \
  "$(agent-browser --cdp "$BASE" eval 'typeof fixtureScriptRan !== "undefined" && fixtureScriptRan' 2>&1 || true)"
check "eval reads the document" 'Added by page script' \
  "$(agent-browser --cdp "$BASE" eval "document.querySelector('#scripted h2').textContent" 2>&1 || true)"

echo "what must stay out"
check_absent "hidden subtrees are excluded" "you should not see this" "$snap"

echo "what must be reported rather than faked"
shot="$(agent-browser --cdp "$BASE" screenshot /tmp/whb-m1-shot.png 2>&1 || true)"
check "screenshot explains that layout is missing" "not implemented yet" "$shot"
check "screenshot mentions layout" "no layout" "$shot"
if [[ -f /tmp/whb-m1-shot.png ]]; then
  printf '  FAIL a screenshot file was written despite the error\n'
  fail=$((fail + 1))
  rm -f /tmp/whb-m1-shot.png
else
  printf '  ok   no screenshot file was written\n'
  pass=$((pass + 1))
fi

echo
if ((fail > 0)); then
  echo "M1 FAILED: ${pass} passed, ${fail} failed"
  exit 1
fi
echo "M1 PASSED: ${pass} checks"
