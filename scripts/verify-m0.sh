#!/usr/bin/env bash
# M0 acceptance test.
#
# Starts the browser, drives it with the real agent-browser CLI, and asserts on
# what comes back. Every expectation here was first measured against Chrome —
# see docs/capture/frames.jsonl.
#
#   ./scripts/verify-m0.sh                 # native build
#   MODE=wasix ./scripts/verify-m0.sh      # the WASIX module under Wasmer
#
# The same checks run in both modes, on purpose: if the WASIX build diverged,
# this is where it would show.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-9333}"
MODE="${MODE:-native}"
SESSION="m0-verify-$$"
BASE="http://127.0.0.1:${PORT}"

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

start_server() {
  case "$MODE" in
    native)
      cargo build --manifest-path "$ROOT/Cargo.toml" --quiet
      "$ROOT/target/debug/wasm-headless-browser" "127.0.0.1:${PORT}" &
      ;;
    wasix)
      "$ROOT/scripts/wasix-build.sh" >/dev/null
      wasmer run --net \
        "$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm" \
        -- "127.0.0.1:${PORT}" &
      ;;
    *)
      echo "unknown MODE '$MODE' (expected native or wasix)" >&2
      exit 2
      ;;
  esac
  SERVER_PID=$!
}

cleanup() {
  agent-browser --cdp "$BASE" close >/dev/null 2>&1 || true
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "mode: ${MODE}"
echo "starting wasm-headless-browser on ${PORT}"
start_server

for _ in $(seq 1 100); do
  curl -sf "$BASE/json/version" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "discovery"
check "GET /json/version advertises a websocket" '"webSocketDebuggerUrl"' "$(curl -s "$BASE/json/version")"

echo "driving with agent-browser"
open_out="$(agent-browser --cdp "$BASE" open https://example.com 2>&1 || true)"
check "open reports the page title" "Sandbox Login" "$open_out"
check "open reports the page url" "https://example.com" "$open_out"

url_out="$(agent-browser --cdp "$BASE" get url 2>&1 || true)"
check "get url" "https://example.com" "$url_out"

snap_out="$(agent-browser --cdp "$BASE" snapshot 2>&1 || true)"
check "snapshot has a heading" 'heading "Sandbox Login"' "$snap_out"
check "snapshot has a paragraph" 'paragraph "Sign in to continue' "$snap_out"
check "snapshot has a form" "form" "$snap_out"
check "snapshot has a named textbox" 'textbox "User"' "$snap_out"
check "snapshot has a password textbox" 'textbox "Password"' "$snap_out"
check "snapshot has a button" 'button "Sign in"' "$snap_out"
check "snapshot has a link" 'link "More info"' "$snap_out"

eval_out="$(agent-browser --cdp "$BASE" eval "document.title" 2>&1 || true)"
check "eval returns a value" '"Sandbox Login"' "$eval_out"

scroll_out="$(agent-browser --cdp "$BASE" scroll down 300 2>&1 || true)"
check "scroll is accepted" "Done" "$scroll_out"

back_out="$(agent-browser --cdp "$BASE" back 2>&1 || true)"
check "back moves through history" "about:blank" "$back_out"

forward_out="$(agent-browser --cdp "$BASE" forward 2>&1 || true)"
check "forward moves back" "https://example.com" "$forward_out"

shot="/tmp/whb-verify-shot-$$.png"
agent-browser --cdp "$BASE" screenshot "$shot" >/dev/null 2>&1 || true
if [[ -s "$shot" ]] && file -b "$shot" | grep -q "^PNG image data"; then
  printf '  ok   screenshot is a png (%s)\n' "$(file -b "$shot" | cut -d, -f1-2)"
  pass=$((pass + 1))
else
  printf '  FAIL screenshot is a png\n'
  fail=$((fail + 1))
fi
rm -f "$shot"

echo
if ((fail > 0)); then
  echo "M0 FAILED (${MODE}): ${pass} passed, ${fail} failed"
  exit 1
fi
echo "M0 PASSED (${MODE}): ${pass} checks"
