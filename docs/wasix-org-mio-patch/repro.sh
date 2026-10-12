#!/usr/bin/env bash
# Runs the repro under Wasmer twice: against mio as the WASIX registry
# publishes it, then with mio-wasix-nonblocking.patch applied to it.
#
# Needs cargo-wasix and Wasmer. Cargo.lock pins the versions reported
# (tokio 1.47.0+wasix.1, mio 1.0.3+wasix.1); delete it to try newer forks.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
WASM=target/wasm32-wasmer-wasi/release/repro.wasm

# The patched build rewrites Cargo.lock; put the pinned one back afterwards.
cp Cargo.lock .Cargo.lock.pinned 2>/dev/null || true
restore() { [[ -f .Cargo.lock.pinned ]] && mv .Cargo.lock.pinned Cargo.lock; }
trap restore EXIT

cargo wasix init >/dev/null            # points crates.io at the WASIX registry
[[ -f Cargo.lock ]] || cargo update --quiet

mio="$(awk '/^name = "mio"$/ { getline; gsub(/"/, "", $3); print $3 }' Cargo.lock)"

echo "== mio $mio, as published"
build() { cargo wasix build --release "$@" >build.log 2>&1 || { cat build.log; exit 1; }; }

build
wasmer run --net "$WASM"

echo
echo "== mio $mio with mio-wasix-nonblocking.patch"
src="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"mio-$mio" | head -1)"
rm -rf mio-patched
cp -R "$src" mio-patched
patch --quiet -p1 -d mio-patched <mio-wasix-nonblocking.patch
build --config 'patch.crates-io.mio.path="mio-patched"'
wasmer run --net "$WASM"
