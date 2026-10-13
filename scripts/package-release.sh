#!/usr/bin/env bash
# Stages the release artefact and nothing else.
#
# Copies wasm-headless-browser.wasm into dist/ and writes SHA256SUMS beside it.
# whb.wasm, agent-browser.wasm and loopback-client.wasm are harness inputs.
# They stay in target/ or harness/public/ and are not copied here.
#
#   scripts/wasix-build.sh
#   scripts/package-release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"
DEST="$ROOT/dist"

if [[ ! -f "$SRC" ]]; then
  echo "not built: $SRC" >&2
  echo "run scripts/wasix-build.sh first" >&2
  exit 1
fi

mkdir -p "$DEST"
cp "$SRC" "$DEST/wasm-headless-browser.wasm"

if command -v shasum >/dev/null 2>&1; then
  (cd "$DEST" && shasum -a 256 wasm-headless-browser.wasm > SHA256SUMS)
elif command -v sha256sum >/dev/null 2>&1; then
  (cd "$DEST" && sha256sum wasm-headless-browser.wasm > SHA256SUMS)
else
  echo "neither shasum nor sha256sum is available" >&2
  exit 1
fi

printf 'release artefact: %s (%s)\n' "$DEST/wasm-headless-browser.wasm" "$(du -h "$DEST/wasm-headless-browser.wasm" | cut -f1)"
printf 'checksum:         %s\n' "$DEST/SHA256SUMS"

WHB="$ROOT/target/wasm32-wasmer-wasi/release/whb.wasm"
AGENT="$ROOT/target/agent-browser-wasix/agent-browser.wasm"
if [[ -f "$WHB" || -f "$AGENT" ]]; then
  printf 'left out:\n'
  # A failing [[ ]] must not be the script's exit status under set -e.
  if [[ -f "$WHB" ]]; then
    printf '  %s (test client)\n' "$WHB"
  fi
  if [[ -f "$AGENT" ]]; then
    printf '  %s (harness fixture)\n' "$AGENT"
  fi
fi
