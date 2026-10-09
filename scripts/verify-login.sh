#!/usr/bin/env bash
# Login-flow acceptance test: read the page, fill it, submit, and land signed in.
#
#   ./scripts/verify-login.sh                 # native build
#   MODE=wasix ./scripts/verify-login.sh      # the WASIX module under Wasmer
#
# The site is fixtures/login: a page its own script renders, a fetch() login,
# an HttpOnly session cookie, and a signed-in page that reads it back. That is
# the shape of the reference site without its credentials.
#
# What it proves: agent-browser's `fill` reaches script-owned inputs,
# `press Enter` submits through the page's own handler, the session survives
# a reload, and the password never shows up in a snapshot or an error.
#
# The wrong password is submitted by clicking the button, the right one by
# pressing Enter, so both paths are covered.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${MODE:-native}"
CDP_PORT="${CDP_PORT:-9360}"
SITE_PORT="${SITE_PORT:-8140}"
SESSION="login-verify-$$"
BASE="http://127.0.0.1:${CDP_PORT}"
SITE="http://127.0.0.1:${SITE_PORT}"
PASSWORD="hunter2"

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

ab() {
  agent-browser --cdp "$BASE" "$@" 2>&1 || true
}

# The ref agent-browser gave the textbox with this name in the last snapshot.
ref_of() {
  grep -o "textbox \"$1\" \[ref=e[0-9]*" <<<"$2" | grep -o 'e[0-9]*$' | head -1
}

SERVER_ERR="$(mktemp)"
start_engine() {
  case "$MODE" in
    native)
      "$ROOT/target/release/wasm-headless-browser" --engine html "127.0.0.1:${CDP_PORT}" 2>"$SERVER_ERR" >/dev/null &
      ;;
    wasix)
      wasmer run --net "$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm" \
        -- --engine html "127.0.0.1:${CDP_PORT}" 2>"$SERVER_ERR" >/dev/null &
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
  for pid in "${SERVER_PID:-}" "${SITE_PID:-}"; do
    [[ -n "$pid" ]] && { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  done
  rm -f "$SERVER_ERR"
}
trap cleanup EXIT

echo "mode: ${MODE}"
case "$MODE" in
  native) cargo build --release --manifest-path "$ROOT/Cargo.toml" --quiet ;;
  wasix) "$ROOT/scripts/wasix-build.sh" >/dev/null ;;
esac

python3 "$ROOT/fixtures/login/server.py" "$SITE_PORT" &
SITE_PID=$!
start_engine

# Wasmer cannot rebind a port for about 30 seconds after an earlier run
# released it; see verify-m0.sh.
for _ in $(seq 1 450); do
  curl -sf "$BASE/json/version" >/dev/null 2>&1 && curl -sf "$SITE/" >/dev/null 2>&1 && break
  if ! kill -0 "$SERVER_PID" 2>/dev/null && grep -q AddrInUse "$SERVER_ERR"; then
    : >"$SERVER_ERR"
    sleep 1
    start_engine
  fi
  sleep 0.1
done

echo "the page its script renders"
check "open reads the title" "Fixture Login" "$(ab open "$SITE/")"
snap="$(ab snapshot -i)"
check "the user field is there" 'textbox "User"' "$snap"
check "the password field is there" 'textbox "Password"' "$snap"
check "the submit button is there" 'button "Sign in"' "$snap"

echo "a wrong password"
ab fill "@$(ref_of User "$snap")" kim >/dev/null
ab fill "@$(ref_of Password "$snap")" wrong-password >/dev/null
check "clicking the button submits" "Done" "$(ab click "@$(grep -o 'button "Sign in" \[ref=e[0-9]*' <<<"$snap" | grep -o 'e[0-9]*$')")"
check "the page reports the failure" 'alert.*Invalid credentials\|Invalid credentials' "$(ab snapshot)"
check_absent "the failed attempt left no sid" "sid=" "$(ab cookies get)"

echo "the right password"
snap="$(ab snapshot -i)"
check "fill reaches a script-owned input" "Done" "$(ab fill "@$(ref_of User "$snap")" kim)"
check "the typed user shows in the tree" 'textbox "User".*: kim' "$(ab snapshot -i)"
ab fill "@$(ref_of Password "$snap")" "$PASSWORD" >/dev/null
check_absent "the password is not in the tree" "$PASSWORD" "$(ab snapshot)"
check "Enter submits" "Done" "$(ab press Enter)"
signed_in="$(ab snapshot)"
check "the signed-in page renders" 'heading "Welcome, kim"' "$signed_in"
check "the url moved with pushState" "$SITE/home" "$(ab get url)"
check "the session cookie is in the jar" "sid=" "$(ab cookies get)"
check_absent "page script cannot see the HttpOnly cookie" "sid=" "$(ab eval 'document.cookie')"

echo "the session survives a reload"
check "reopening /home is still signed in" "Fixture Login" "$(ab open "$SITE/home")"
check "the cookie authenticated the API call" 'heading "Welcome, kim"' "$(ab snapshot)"

echo "what must not leak"
check_absent "no snapshot carried the password" "$PASSWORD" "$signed_in$(ab snapshot)"
check_absent "the engine's stderr does not carry it" "$PASSWORD" "$(cat "$SERVER_ERR")"

echo
if ((fail > 0)); then
  echo "LOGIN FAILED (${MODE}): ${pass} passed, ${fail} failed"
  exit 1
fi
echo "LOGIN PASSED (${MODE}): ${pass} checks"
