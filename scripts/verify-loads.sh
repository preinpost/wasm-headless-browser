#!/usr/bin/env bash
# Repeated heavy loads: 80 assets, 1600 inherited CSS variables, 800 spans.
# The client keeps one tab/WebSocket for LOADS navigations, repeated for TABS
# tabs, so both document resets and additional script threads are exercised.
#
#   ./scripts/verify-loads.sh
#   MODE=wasix ./scripts/verify-loads.sh
#   MODE=wasix PAGE_URL=https://example.org/ LOADS=10 TABS=3 ./scripts/verify-loads.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${MODE:-native}"
LOADS="${LOADS:-10}"
TABS="${TABS:-3}"
CDP_PORT="${CDP_PORT:-9380}"
SITE_PORT="${SITE_PORT:-8160}"
WORK="$(mktemp -d)"
cleanup() {
  for pid in "${ENGINE_PID:-}" "${SITE_PID:-}"; do
    if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  done
  rm -rf "$WORK"
}
trap cleanup EXIT
case "$MODE" in
  native)
    cargo build --release --manifest-path "$ROOT/Cargo.toml" --quiet
    engine=("$ROOT/target/release/wasm-headless-browser") ;;
  wasix)
    "$ROOT/scripts/wasix-build.sh" >"$WORK/build.log" 2>&1 || { tail -40 "$WORK/build.log"; exit 1; }
    engine=(wasmer run --net "$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm" --) ;;
  *) echo "unknown MODE: $MODE" >&2; exit 2 ;;
esac
rustc -O "$ROOT/scripts/wasix-loopback-client.rs" -o "$WORK/client"
python3 "$ROOT/fixtures/fetch-stress/server.py" "$SITE_PORT" >"$WORK/site.log" 2>&1 &
SITE_PID=$!
"${engine[@]}" --engine html "127.0.0.1:$CDP_PORT" >"$WORK/engine.log" 2>&1 &
ENGINE_PID=$!
ready=false
for _ in $(seq 1 400); do
  if curl -sf "http://127.0.0.1:$CDP_PORT/json/version" >/dev/null && curl -sf "http://127.0.0.1:$SITE_PORT/metrics" >/dev/null; then
    ready=true; break
  fi
  if ! kill -0 "$ENGINE_PID" 2>/dev/null || ! kill -0 "$SITE_PID" 2>/dev/null; then break; fi
  sleep 0.1
done
if [[ "$ready" != true ]]; then
  echo "servers did not start" >&2
  tail -30 "$WORK/engine.log" "$WORK/site.log"
  exit 1
fi
PAGE="${PAGE_URL:-http://127.0.0.1:$SITE_PORT/}"
echo "mode=$MODE, $TABS tabs x $LOADS loads: $PAGE"
for tab in $(seq 1 "$TABS"); do
  # Bound a stuck client even when a Wasmer panic leaves the engine alive.
  if ! python3 - "$WORK/client" "127.0.0.1:$CDP_PORT" "$PAGE" "$LOADS" "${LOAD_TIMEOUT:-180}" >"$WORK/client.log" 2>&1 <<'PY'
import subprocess, sys
try:
    result = subprocess.run([sys.argv[1], "cdp", *sys.argv[2:5]], timeout=float(sys.argv[5]))
    sys.exit(result.returncode)
except subprocess.TimeoutExpired:
    print("client timed out", flush=True)
    sys.exit(1)
PY
  then
    echo "FAIL tab $tab"
    tail -30 "$WORK/client.log" "$WORK/engine.log"
    exit 1
  fi
  if [[ -z "${PAGE_URL:-}" ]]; then
    count="$(grep -c 'load value:.*loaded\\\":40' "$WORK/client.log" || true)"
    if [[ "$count" != "$LOADS" ]]; then
      echo "FAIL: expected all 40 scripts on every load, got $count/$LOADS" >&2
      tail -30 "$WORK/client.log" "$WORK/engine.log"
      exit 1
    fi
  fi
  echo "  ok tab $tab: $LOADS loads, evaluation after every load"
done
if grep -Eq 'panicked|overflow' "$WORK/engine.log"; then
  tail -30 "$WORK/engine.log"; exit 1
fi
if [[ -z "${PAGE_URL:-}" ]]; then
  curl -sf "http://127.0.0.1:$SITE_PORT/metrics" | python3 -c '
import json, sys
m = json.load(sys.stdin)
loads = int(sys.argv[1]) * int(sys.argv[2])
assert m == {"pages": loads, "assets": loads * 80}, m
print("  ok {} asset requests, none lost or duplicated".format(m["assets"]))
' "$LOADS" "$TABS"
fi
echo "LOADS PASSED: $((LOADS * TABS)) navigations in one engine"
