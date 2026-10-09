#!/usr/bin/env bash
# Runs the WASIX build under Wasmer and exposes it on loopback.
#
#   scripts/wasix-run.sh [addr]                      # default 127.0.0.1:9222
#   scripts/wasix-run.sh --engine html [addr]
#   security find-generic-password -s gw -w | \
#     scripts/wasix-run.sh --engine html --secret gw@https://gw.example.com [addr]
#
# Arguments go to the browser as they are. With --secret, the values must be
# piped in: a guest's stdin always looks like a terminal under Wasmer, so the
# browser cannot refuse a terminal itself, and this script does it instead.
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
WASM="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"

if [[ ! -f "$WASM" ]]; then
  echo "not built yet; run scripts/wasix-build.sh first" >&2
  exit 1
fi

for arg in "$@"; do
  if [[ "$arg" == "--secret" && -t 0 ]]; then
    echo "--secret reads its value from stdin, and stdin is a terminal, which would show it as it is" >&2
    echo "typed. Pipe the values in, one line per --secret, in order." >&2
    exit 2
  fi
done

if (($# == 0)); then
  set -- 127.0.0.1:9222
fi
exec wasmer run --net "$WASM" -- "$@"
