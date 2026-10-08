#!/usr/bin/env bash
# Builds the browser for WASIX (wasm32-wasmer-wasi) and prints the artefact.
#
# Requires the WASIX toolchain:
#
#   cargo install cargo-wasix      # fetches a WASIX-patched Rust toolchain on
#                                  # first use
#   brew install wasmer            # a runtime that can execute WASIX modules
#
# Notes:
#   * cargo-wasix writes .cargo/config.toml, which points crates.io at the WASIX
#     overlay registry. That file is gitignored so a clone builds from crates.io
#     by default. Native builds work either way, because everything this project
#     depends on is in both registries.
#   * The build needs no C toolchain. Keeping `sha1` out was deliberate: it
#     reaches libc through cpufeatures, which WASIX would then have to port.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo wasix build --release "$@"

WASM="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "expected artefact not found: $WASM" >&2
  exit 1
fi

printf '\nartefact: %s (%s)\n' "$WASM" "$(du -h "$WASM" | cut -f1)"
printf 'run it:   scripts/wasix-run.sh 127.0.0.1:9222\n'
