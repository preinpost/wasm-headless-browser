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
#     by default.
#   * One Cargo.lock cannot serve both registries. The WASIX registry carries
#     only `+wasix` builds of libc and cc, and crates.io has none of those, so
#     each registry fails on the other's lockfile. The committed lockfile is the
#     crates.io one. This script re-pins those two packages for the build, then
#     restores the lockfile and removes .cargo/config.toml, because with that
#     file present a native build would fail on the crates.io lockfile. Every
#     cargo-wasix command writes it again.
#   * The build needs no C toolchain. Keeping `sha1` out was deliberate: it
#     reaches libc through cpufeatures, which WASIX would then have to port.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

lock_backup="$(mktemp)"
cp Cargo.lock "$lock_backup"
restore() {
  cp "$lock_backup" Cargo.lock
  rm -f "$lock_backup" .cargo/config.toml
  rmdir .cargo 2>/dev/null || true
}
trap restore EXIT

cargo wasix init >/dev/null
cargo update --quiet -p libc -p cc
cargo wasix build --release "$@"

WASM="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "expected artefact not found: $WASM" >&2
  exit 1
fi

printf '\nartefact: %s (%s)\n' "$WASM" "$(du -h "$WASM" | cut -f1)"
printf 'run it:   scripts/wasix-run.sh 127.0.0.1:9222\n'
