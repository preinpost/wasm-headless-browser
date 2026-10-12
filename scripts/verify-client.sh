#!/usr/bin/env bash
# The login flow driven by whb, the in-tree client, instead of agent-browser.
#
#   ./scripts/verify-client.sh                 # native engine and client
#   MODE=wasix ./scripts/verify-client.sh      # both as WASIX modules under Wasmer
#
# In WASIX mode every whb command is its own `wasmer run`, as an agent in a
# sandbox would run it. The commands share nothing but the engine and a state
# directory mounted into each run, which is what this checks they need.
#
# The credentials reach the engine as in verify-login.sh: on its stdin, bound
# to the site's origin. whb types {{secret:NAME}} and must never see a value;
# everything it printed is kept and searched for one at the end.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${MODE:-native}"
CDP_PORT="${CDP_PORT:-9370}"
SITE_PORT="${SITE_PORT:-8150}"
SITE="http://127.0.0.1:${SITE_PORT}"
USER_NAME="kim"
PASSWORD="hunter2"

STATE_DIR="$(mktemp -d)"
SERVER_ERR="$(mktemp)"
SHOT_DIR="$(mktemp -d)"
pass=0
fail=0
TRANSCRIPT=""

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

# Runs whb and keeps what it said, errors included, for the leak checks.
whb() {
  local out
  case "$MODE" in
    native)
      out="$(WHB_STATE_DIR="$STATE_DIR" WHB_CDP="$CDP_PORT" "$ROOT/target/release/whb" "$@" 2>&1 || true)" ;;
    wasix)
      out="$(wasmer run --net --volume "$STATE_DIR:/state" --env WHB_STATE_DIR=/state --env "WHB_CDP=$CDP_PORT" \
        "$ROOT/target/wasm32-wasmer-wasi/release/whb.wasm" -- "$@" 2>&1 || true)" ;;
  esac
  TRANSCRIPT+="$out"$'\n'
  printf '%s' "$out"
}

ref_of() {
  grep -o "$1 \"$2\" \[ref=e[0-9]*" <<<"$3" | grep -o 'e[0-9]*$' | head -1
}

start_engine() {
  local command
  case "$MODE" in
    native) command=("$ROOT/target/release/wasm-headless-browser") ;;
    wasix) command=(wasmer run --net --env WHB_TRACE_CDP=1 "$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm" --) ;;
    *)
      echo "unknown MODE '$MODE' (expected native or wasix)" >&2
      exit 2
      ;;
  esac
  printf '%s\n%s\n' "$USER_NAME" "$PASSWORD" | WHB_TRACE_CDP=1 "${command[@]}" --engine html \
    --secret "fixture_user@${SITE}" --secret "fixture_pw@${SITE}" "127.0.0.1:${CDP_PORT}" \
    2>>"$SERVER_ERR" >/dev/null &
  SERVER_PID=$!
}

cleanup() {
  for pid in "${SERVER_PID:-}" "${SITE_PID:-}"; do
    [[ -n "$pid" ]] && { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  done
  rm -rf "$STATE_DIR" "$SHOT_DIR"
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
  curl -sf "http://127.0.0.1:${CDP_PORT}/json/version" >/dev/null 2>&1 && curl -sf "$SITE/" >/dev/null 2>&1 && break
  if ! kill -0 "$SERVER_PID" 2>/dev/null && grep -q AddrInUse "$SERVER_ERR"; then
    : >"$SERVER_ERR"
    sleep 1
    start_engine
  fi
  sleep 0.1
done

echo "reading the page"
check "the client prints its version" "whb " "$(whb --version)"
check "open reads the title" "Fixture Login" "$(whb open "$SITE/")"
snap="$(whb snapshot -i)"
check "the user field has a ref" 'textbox "User" \[ref=e' "$snap"
check "the password field has a ref" 'textbox "Password" \[ref=e' "$snap"
check "the submit button has a ref" 'button "Sign in" \[ref=e' "$snap"
check "the full tree nests the form" '^- form' "$(whb snapshot)"

echo "a wrong password, typed, submitted by clicking"
# Not the user name: the engine scrubs a secret's value from all it says, so
# typing "$USER_NAME" here would read back as its placeholder.
whb fill "@$(ref_of textbox User "$snap")" someone >/dev/null
whb fill "@$(ref_of textbox User "$snap")" another >/dev/null
whb fill "@$(ref_of textbox Password "$snap")" wrong-password >/dev/null
check "clicking the button submits" "Done" "$(whb click "@$(ref_of button 'Sign in' "$snap")")"
check "the page reports the failure" 'Invalid credentials' "$(whb snapshot)"
check "fill replaces, not appends" '^"another"$' "$(whb eval "document.getElementById('user').value")"

echo "the right credentials, by placeholder, submitted with Enter"
snap="$(whb snapshot -i)"
check "a secret fills a script-owned input" "Done" "$(whb fill "@$(ref_of textbox User "$snap")" '{{secret:fixture_user}}')"
check "the tree shows the placeholder, not the user" 'textbox "User".*{{secret:fixture_user}}' "$(whb snapshot -i)"
whb fill "@$(ref_of textbox Password "$snap")" '{{secret:fixture_pw}}' >/dev/null
check "a CSS selector works as a target" "{{secret:fixture_pw}}" "$(whb get value '#password')"
check "Enter submits" "Done" "$(whb press Enter)"
check "the signed-in page renders, its name scrubbed" 'heading "Welcome, {{secret:fixture_user}}"' "$(whb snapshot)"
check "the url moved with pushState" "$SITE/home" "$(whb get url)"
check_absent "page script cannot see the HttpOnly cookie" "sid=" "$(whb eval 'document.cookie')"

echo "refs and sessions"
snap="$(whb snapshot -i)"
first_ref="$(grep -o 'ref=e[0-9]*' <<<"$snap" | head -1 | cut -d= -f2)"
whb eval "location.href = '$SITE/'" >/dev/null
check "a ref from before a navigation is refused" "page changed since the snapshot" "$(whb click "@$first_ref")"
check "a malformed ref is refused as a ref" "not a ref" "$(whb click @nope)"
check "an unknown ref is refused" "unknown ref @e999" "$(whb click @e999)"
check "a selector that matches nothing says so" "no element matches" "$(whb click '#nothing-here')"
check "a second session has its own tab" "about:blank" "$(whb --session other get url)"
check "and the first keeps its page" "$SITE/" "$(whb get url)"
check "the session survives a reload" "Welcome, " "$(whb open "$SITE/home" >/dev/null; whb snapshot)"

echo "screenshots and closing"
shot="$SHOT_DIR/shot.png"
# The file is written where whb runs: under Wasmer that is the mounted state
# directory, so the screenshot goes there.
case "$MODE" in
  native) check "a screenshot is saved" "Screenshot saved" "$(whb screenshot "$shot")" ;;
  wasix)
    check "a screenshot is saved" "Screenshot saved" "$(whb screenshot /state/shot.png)"
    shot="$STATE_DIR/shot.png" ;;
esac
check "and is a PNG" "PNG" "$(file "$shot" 2>&1)"
check "close closes the tab" "Closed" "$(whb close)"
check "the next command opens a fresh one" "about:blank" "$(whb get url)"

echo "what must not leak"
check_absent "the client never saw the password" "$PASSWORD" "$TRANSCRIPT"
check_absent "the client never saw the user name" "Welcome, $USER_NAME" "$TRANSCRIPT"
check_absent "nor did its state files" "$PASSWORD" "$(cat "$STATE_DIR"/*.json 2>/dev/null)"
check "the engine trace did run" "Input.insertText" "$(cat "$SERVER_ERR")"

echo
if ((fail > 0)); then
  echo "CLIENT FAILED (${MODE}): ${pass} passed, ${fail} failed"
  exit 1
fi
echo "CLIENT PASSED (${MODE}): ${pass} checks"
