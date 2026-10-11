#!/usr/bin/env bash
# Opens a page in the engine and shows it live in agent-browser's dashboard,
# where a person can watch it and click and type into it.
#
#   ./scripts/watch.sh https://gw.example.com
#   MODE=wasix ./scripts/watch.sh https://gw.example.com
#   security find-generic-password -s whb-gw -w | \
#     ./scripts/watch.sh https://gw.example.com --secret gw@https://gw.example.com
#
# Arguments after the URL go to the engine. Values for --secret are read from
# this script's stdin, as the engine reads them; see docs/credentials.md.
#
# The dashboard (http://localhost:4848 by default) streams the page through
# Page.startScreencast and sends clicks and keys back as Input events. The
# agent-browser session is named `watch`, so commands run with
# `--session watch` act on the page being watched:
#
#   agent-browser --session watch --cdp http://127.0.0.1:9222 snapshot -i
#
# The viewport is set to agent-browser's own default, 1280x720 (VIEWPORT to
# change it). The dashboard turns a click into page coordinates with the
# viewport agent-browser believes in, so the engine has to be told it: left
# at the engine's 800x600, every click lands 1.6 times too far right and down.
#
# Ctrl-C stops the engine, and the dashboard if this script started it; one
# that was already running is used and left running.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
URL="${1:?usage: watch.sh URL [engine arguments...]}"
shift
MODE="${MODE:-native}"
CDP_PORT="${CDP_PORT:-9222}"
DASHBOARD_PORT="${DASHBOARD_PORT:-4848}"
BASE="http://127.0.0.1:${CDP_PORT}"
read -r VIEW_WIDTH VIEW_HEIGHT <<<"${VIEWPORT:-1280 720}"
export AGENT_BROWSER_SESSION="${AGENT_BROWSER_SESSION:-watch}"

case "$MODE" in
  native)
    cargo build --release --manifest-path "$ROOT/Cargo.toml" --quiet
    engine=("$ROOT/target/release/wasm-headless-browser")
    ;;
  wasix)
    "$ROOT/scripts/wasix-build.sh" >/dev/null
    engine=("$ROOT/scripts/wasix-run.sh")
    ;;
  *)
    echo "unknown MODE '$MODE' (expected native or wasix)" >&2
    exit 2
    ;;
esac

STARTED_DASHBOARD=""
cleanup() {
  agent-browser --cdp "$BASE" close >/dev/null 2>&1 || true
  # Only a dashboard this script started: one already running may be
  # showing someone else's sessions.
  [[ -n "$STARTED_DASHBOARD" ]] && { agent-browser dashboard stop >/dev/null 2>&1 || true; }
  [[ -n "${ENGINE_PID:-}" ]] && { kill "$ENGINE_PID" 2>/dev/null || true; wait "$ENGINE_PID" 2>/dev/null || true; }
}
trap cleanup EXIT INT TERM

# Stdin is passed on explicitly: a background job's would otherwise be
# /dev/null, and --secret values would never arrive.
"${engine[@]}" --engine html "$@" "127.0.0.1:${CDP_PORT}" <&0 >/dev/null &
ENGINE_PID=$!
for _ in $(seq 1 300); do
  curl -sf "$BASE/json/version" >/dev/null 2>&1 && break
  kill -0 "$ENGINE_PID" 2>/dev/null || { echo "the engine stopped; is port ${CDP_PORT} free?" >&2; exit 1; }
  sleep 0.1
done

agent-browser --cdp "$BASE" set viewport "$VIEW_WIDTH" "$VIEW_HEIGHT" >/dev/null
agent-browser --cdp "$BASE" open "$URL"
# One dashboard serves every session: use the one running, wherever it is.
if curl -s -m 2 -o /dev/null "http://localhost:${DASHBOARD_PORT}/"; then
  :
elif started="$(agent-browser dashboard start --port "$DASHBOARD_PORT" 2>&1)"; then
  STARTED_DASHBOARD=1
else
  running="$(grep -o 'port [0-9]*' <<<"$started" | grep -o '[0-9]*' | head -1)"
  [[ -n "$running" ]] || { echo "$started" >&2; exit 1; }
  DASHBOARD_PORT="$running"
fi
echo
echo "watch and drive it at  http://localhost:${DASHBOARD_PORT}   (session: ${AGENT_BROWSER_SESSION})"
echo "Ctrl-C to stop"
wait "$ENGINE_PID"
