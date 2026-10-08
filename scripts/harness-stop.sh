#!/usr/bin/env bash
# Stops processes this repo's harness may have left on its ports.
#
#   scripts/harness-stop.sh
#
# verify-harness.sh cleans up after itself; this is for the cases where the
# harness was started by hand and abandoned.
set -uo pipefail

PORT="${HARNESS_PORT:-8099}"
WISP_PORT="${WISP_PORT:-5010}"

stop_port() {
  local port="$1" label="$2"
  local pids
  pids="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)"
  if [[ -z "$pids" ]]; then
    echo "nothing listening on $port ($label)"
    return
  fi
  # Only kill it if it is one of ours; another project may own the port.
  for pid in $pids; do
    if ps -p "$pid" -o command= 2>/dev/null | grep -qE 'harness/(serve|wisp-server)\.mjs|node (serve|wisp-server)\.mjs'; then
      echo "stopping $pid on $port ($label)"
      kill "$pid" 2>/dev/null || true
    else
      echo "leaving $pid on $port alone; its command does not look like ours:"
      ps -p "$pid" -o command= 2>/dev/null | sed 's/^/  /'
    fi
  done
}

stop_port "$PORT" "harness"
stop_port "$WISP_PORT" "wisp relay"
