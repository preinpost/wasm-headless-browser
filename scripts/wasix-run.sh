#!/usr/bin/env bash
# Runs the WASIX build under Wasmer and exposes it on loopback.
#
#   scripts/wasix-run.sh [addr]        # default 127.0.0.1:9222
#
# `--net` is not optional. Wasmer denies socket creation by default, and the
# guest sees that as EOPNOTSUPP (os error 58) on the first bind, which looks
# like a WASIX limitation but is a runtime policy. Narrow it if you prefer:
#
#   wasmer run --net=ipv4:allow=127.0.0.1:9222/in ...
#
# Because the listener really is on the host's loopback, a client running
# outside the sandbox can reach in. That is how scripts/verify-m0.sh drives it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADDR="${1:-127.0.0.1:9222}"
WASM="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"

if [[ ! -f "$WASM" ]]; then
  echo "not built yet; run scripts/wasix-build.sh first" >&2
  exit 1
fi

exec wasmer run --net "$WASM" -- "$ADDR"
