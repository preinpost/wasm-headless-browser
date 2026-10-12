#!/usr/bin/env bash
# Runs the browser-sandbox harness and asserts on the result.
#
# Without this, checking E0-E5 means opening the page by hand and reading
# `window.__harness`. The page does the work; this only drives and judges it.
#
#   ./scripts/verify-harness.sh                    # loopback and egress probes
#   OUTBOUND=login.example.com:443 CURL=https://login.example.com/ \
#     ./scripts/verify-harness.sh                  # a specific public target
#   ENGINE=html ./scripts/verify-harness.sh        # the real engine, which also
#                                                  # loads a page (E6)
#   ENGINE=html PAGE_URL=https://example.org/ ./scripts/verify-harness.sh
#
# Requirements: the harness dependencies installed (cd harness && npm install),
# a WASIX build present, and Chrome reachable by agent-browser. The SDK needs
# JSPI, so Safari and Firefox will not do. The page loads public/engine.wasm,
# which is refreshed from the WASIX build whenever the two differ.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS="$ROOT/harness"
PORT="${HARNESS_PORT:-8099}"
WISP_PORT="${WISP_PORT:-5010}"
OUTBOUND="${OUTBOUND:-example.com:80}"
OUTBOUND_PATH="${OUTBOUND_PATH:-/}"
CURL_URL="${CURL:-}"
ENGINE_KIND="${ENGINE:-}"
PAGE_URL="${PAGE_URL:-https://example.com/}"
SESSION="harness-verify-$$"

export AGENT_BROWSER_SESSION="$SESSION"
export AGENT_BROWSER_NAMESPACE="$SESSION"

pass=0
fail=0
declare -a EXPECTED=(
  "E0 engine binds loopback in the sandbox"
  "E1 sandbox-internal client reaches the engine"
  "E2 ports.wait observes the listener"
  "E4 sandbox reaches the public internet"
)

cleanup() {
  agent-browser close >/dev/null 2>&1 || true
  [[ -n "${SERVER_PID:-}" ]] && { kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; }
  [[ -n "${WISP_PID:-}" ]] && { kill "$WISP_PID" 2>/dev/null || true; wait "$WISP_PID" 2>/dev/null || true; }
}
trap cleanup EXIT

if [[ ! -d "$HARNESS/node_modules" ]]; then
  echo "installing harness dependencies"
  (cd "$HARNESS" && npm install --silent)
fi

BUILT="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"
if [[ ! -f "$BUILT" ]]; then
  echo "no WASIX build at $BUILT; run scripts/wasix-build.sh first" >&2
  exit 1
fi
if [[ ! -f "$HARNESS/public/engine.wasm" || ! -f "$HARNESS/public/loopback-client.wasm" \
  || "$ROOT/scripts/wasix-loopback-client.rs" -nt "$HARNESS/public/loopback-client.wasm" ]]; then
  echo "building the WASIX probes"
  (cd "$HARNESS" && node copy-wasm.mjs)
elif ! cmp -s "$BUILT" "$HARNESS/public/engine.wasm"; then
  # Only copied when missing, the page would go on testing an old engine.
  echo "refreshing public/engine.wasm from the WASIX build"
  cp "$BUILT" "$HARNESS/public/engine.wasm"
fi
echo "engine.wasm: $(wc -c <"$HARNESS/public/engine.wasm" | tr -d ' ') bytes"

echo "starting wisp relay on 127.0.0.1:${WISP_PORT}"
(cd "$HARNESS" && PORT="$WISP_PORT" node wisp-server.mjs) >/tmp/whb-wisp.log 2>&1 &
WISP_PID=$!

echo "starting harness on 127.0.0.1:${PORT}"
(cd "$HARNESS" && PORT="$PORT" node serve.mjs) >/tmp/whb-harness.log 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:${PORT}/" >/dev/null 2>&1 && break
  sleep 0.1
done

PAGE="http://127.0.0.1:${PORT}/?mode=wisp&wisp=ws://127.0.0.1:${WISP_PORT}/&outbound=${OUTBOUND}&outboundPath=${OUTBOUND_PATH}"
if [[ -n "$ENGINE_KIND" ]]; then
  PAGE="${PAGE}&engine=${ENGINE_KIND}"
fi
if [[ "$ENGINE_KIND" == "html" ]]; then
  PAGE="${PAGE}&page=${PAGE_URL}"
  EXPECTED+=("E6 real engine loads a page in the sandbox")
fi
if [[ -n "$CURL_URL" ]]; then
  PAGE="${PAGE}&curl=${CURL_URL}"
  EXPECTED+=("E5 sandbox completes an HTTPS request")
fi

echo "opening the harness page"
agent-browser open "$PAGE" >/dev/null 2>&1 || true

# Poll until the page reports it is done. The probes include a curl download, a
# registry fetch and, for the real engine, a page load in the sandbox, so this
# is not instant.
RESULTS=""
for _ in $(seq 1 240); do
  if [[ "$(agent-browser eval "window.__harnessDone === true" 2>/dev/null || echo '')" == "true" ]]; then
    RESULTS="$(agent-browser eval "JSON.stringify(window.__harness)" 2>/dev/null || echo '')"
    break
  fi
  sleep 1
done

if [[ -z "$RESULTS" ]]; then
  echo "the harness never reported done; last page state:" >&2
  agent-browser eval "document.getElementById('log').textContent" 2>&1 | tail -20 >&2 || true
  echo "FAILED: harness did not finish" >&2
  exit 1
fi

echo "results"
for key in "${EXPECTED[@]}"; do
  verdict="$(python3 -c '
import json, sys
raw, key = sys.argv[1], sys.argv[2]
# agent-browser prints the eval result as a JSON string, so unwrap twice.
try:
    outer = json.loads(raw)
    data = json.loads(outer) if isinstance(outer, str) else outer
except Exception:
    print("FAIL\tunparseable result")
    sys.exit(0)
r = (data.get("results") or {}).get(key)
if r is None:
    print("FAIL\tmissing from results")
elif r.get("ok"):
    print("ok\t" + str(r.get("detail", ""))[:160])
else:
    print("FAIL\t" + str(r.get("detail", ""))[:160])
' "$RESULTS" "$key")"
  status="${verdict%%$'\t'*}"
  detail="${verdict#*$'\t'}"
  if [[ "$status" == "ok" ]]; then
    printf '  ok   %s\n       %s\n' "$key" "$detail"
    pass=$((pass + 1))
  else
    printf '  FAIL %s\n       %s\n' "$key" "$detail"
    fail=$((fail + 1))
  fi
done

echo
if ((fail > 0)); then
  echo "HARNESS FAILED: ${pass} passed, ${fail} failed"
  exit 1
fi
echo "HARNESS PASSED: ${pass} checks"
