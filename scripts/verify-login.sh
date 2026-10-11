#!/usr/bin/env bash
# Login-flow acceptance test: read the page, fill it, submit, and land signed in,
# with the credentials kept from the client.
#
#   ./scripts/verify-login.sh                 # native build
#   MODE=wasix ./scripts/verify-login.sh      # the WASIX module under Wasmer
#
# The site is fixtures/login: a page its own script renders, a fetch() login,
# an HttpOnly session cookie, and a signed-in page that reads it back. That is
# the shape of a real React login site without its credentials.
#
# The credentials go to the engine the way they would for real: piped into its
# stdin, bound to the site's origin with --secret. The client types
# {{secret:NAME}} and never sees a value. The engine runs with WHB_TRACE_CDP=1,
# its noisiest, and every word the client got back is kept, so the leak checks
# at the end look at everything that could carry one.
#
# The wrong password is typed as plain text and submitted by clicking the
# button; the right one is a placeholder submitted by pressing Enter, so both
# input paths are covered.
#
# A HAR is recorded throughout, as a client would to debug a login, and the
# network log is read back: the login POST carries the password, so the
# request body is where a leak through Network.* events would show.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${MODE:-native}"
CDP_PORT="${CDP_PORT:-9360}"
SITE_PORT="${SITE_PORT:-8140}"
SESSION="login-verify-$$"
BASE="http://127.0.0.1:${CDP_PORT}"
SITE="http://127.0.0.1:${SITE_PORT}"
USER_NAME="kim"
PASSWORD="hunter2"

export AGENT_BROWSER_SESSION="$SESSION"
export AGENT_BROWSER_NAMESPACE="$SESSION"

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

# Runs agent-browser and keeps what it said, for the leak checks.
ab() {
  local out
  out="$(agent-browser --cdp "$BASE" "$@" 2>&1 || true)"
  TRANSCRIPT+="$out"$'\n'
  printf '%s' "$out"
}

# The ref agent-browser gave a role and name in a snapshot.
ref_of() {
  grep -o "$1 \"$2\" \[ref=e[0-9]*" <<<"$3" | grep -o 'e[0-9]*$' | head -1
}

SERVER_ERR="$(mktemp)"
HAR_DIR="$(mktemp -d)"
start_engine() {
  local command
  case "$MODE" in
    native) command=("$ROOT/target/release/wasm-headless-browser") ;;
    # Wasmer does not pass the host's environment through; --env does.
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
  agent-browser --cdp "$BASE" close >/dev/null 2>&1 || true
  for pid in "${SERVER_PID:-}" "${SITE_PID:-}"; do
    [[ -n "$pid" ]] && { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }
  done
  rm -f "$SERVER_ERR"
  rm -rf "$HAR_DIR"
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
check "HAR recording starts" "HAR recording started" "$(ab network har start)"
check "open reads the title" "Fixture Login" "$(ab open "$SITE/")"
snap="$(ab snapshot -i)"
check "the user field is there" 'textbox "User"' "$snap"
check "the password field is there" 'textbox "Password"' "$snap"
check "the submit button is there" 'button "Sign in"' "$snap"

echo "a wrong password, typed, submitted by clicking"
ab fill "@$(ref_of textbox User "$snap")" "$USER_NAME" >/dev/null
ab fill "@$(ref_of textbox Password "$snap")" wrong-password >/dev/null
check "clicking the button submits" "Done" "$(ab click "@$(ref_of button 'Sign in' "$snap")")"
check "the page reports the failure" 'Invalid credentials' "$(ab snapshot)"
check_absent "the failed attempt left no sid" "sid=" "$(ab cookies get)"

echo "the right credentials, by placeholder, submitted with Enter"
snap="$(ab snapshot -i)"
check "a secret fills a script-owned input" "Done" "$(ab fill "@$(ref_of textbox User "$snap")" '{{secret:fixture_user}}')"
check "the tree shows the placeholder, not the user" 'textbox "User".*{{secret:fixture_user}}' "$(ab snapshot -i)"
ab fill "@$(ref_of textbox Password "$snap")" '{{secret:fixture_pw}}' >/dev/null
check "reading the field back gives the placeholder" "{{secret:fixture_pw}}" "$(ab eval "document.getElementById('password').value")"
check "Enter submits" "Done" "$(ab press Enter)"
check "the signed-in page renders, its name scrubbed" 'heading "Welcome, {{secret:fixture_user}}"' "$(ab snapshot)"
check "the url moved with pushState" "$SITE/home" "$(ab get url)"
check "the session cookie is in the jar" "sid=" "$(ab cookies get)"
check_absent "page script cannot see the HttpOnly cookie" "sid=" "$(ab eval 'document.cookie')"

shot_dir="$HAR_DIR/shot.png"
check "a screenshot of the signed-in page is saved" "Screenshot saved" "$(ab screenshot "$shot_dir")"
check "and is a PNG" "PNG" "$(file "$shot_dir" 2>&1)"

echo "the network log"
requests="$(ab network requests)"
check "the document is in it" "GET $SITE/ (Document) 200" "$requests"
check "so is the script" "GET $SITE/app.js (Script) 200" "$requests"
check "so is the login POST" "POST $SITE/api/login (Fetch) 200" "$requests"
login_id="$(grep "POST $SITE/api/login (Fetch) 200" <<<"$requests" | grep -o '^\[[^]]*' | tr -d '[')"
detail="$(ab --json network request "$login_id")"
check "its body went out with placeholders" '"postData":"{\\"user\\":\\"{{secret:fixture_user}}\\",\\"password\\":\\"{{secret:fixture_pw}}\\"}"' "$detail"
check "the response body is there" '"responseBody":"{\\"ok\\":true}"' "$detail"

echo "the session survives a reload"
check "reopening /home is still signed in" "Fixture Login" "$(ab open "$SITE/home")"
check "the cookie authenticated the API call" 'heading "Welcome, ' "$(ab snapshot)"

echo "a secret stays with its origin"
ab open "data:text/html,<title>Elsewhere</title><input id=x aria-label=Elsewhere>" >/dev/null
check "another origin's field refuses it" "is for $SITE" \
  "$(ab fill '#x' '{{secret:fixture_pw}}')"
check "and stays empty" '""' "$(ab eval "document.getElementById('x').value")"

check "the HAR is saved" "HAR saved" "$(ab network har stop "$HAR_DIR/login.har")"
TRANSCRIPT+="$(cat "$HAR_DIR/login.har" 2>/dev/null)"$'\n'
check "the HAR has the login POST" '"url": *"'"$SITE"'/api/login"' "$(cat "$HAR_DIR/login.har" 2>/dev/null)"

echo "what must not leak"
check_absent "the client never saw the password, HAR included" "$PASSWORD" "$TRANSCRIPT"
check_absent "the client never saw the user name" "Welcome, $USER_NAME" "$TRANSCRIPT"
check_absent "the engine's stderr, traced, does not carry it" "$PASSWORD" "$(cat "$SERVER_ERR")"
check "the trace did run" "Input.insertText" "$(cat "$SERVER_ERR")"

echo
if ((fail > 0)); then
  echo "LOGIN FAILED (${MODE}): ${pass} passed, ${fail} failed"
  exit 1
fi
echo "LOGIN PASSED (${MODE}): ${pass} checks"
