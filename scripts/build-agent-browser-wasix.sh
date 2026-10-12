#!/usr/bin/env bash
# Builds agent-browser for WASIX, patched to run in the same sandbox as the
# engine, and prints the module's path.
#
#   ./scripts/build-agent-browser-wasix.sh
#   ENGINE=html AGENT_BROWSER_WASM=target/agent-browser-wasix/agent-browser.wasm \
#     ./scripts/verify-harness.sh                  # E8: it drives the engine
#
# What it does, in target/agent-browser-wasix:
#   1. clones vercel-labs/agent-browser at $AGENT_BROWSER_REF (v0.38.2) and
#      applies experiments/agent-browser/wasix.patch;
#   2. resolves the lockfile against the WASIX registry, which carries forks
#      of tokio, mio, socket2, hyper, reqwest and others;
#   3. copies the mio fork it picked, applies mio-wasix-nonblocking.patch, and
#      uses that copy through [patch.crates-io]. Unpatched, connected sockets
#      stay blocking and the daemon's first wait on the engine hangs;
#   4. builds with a wasm-capable llvm-ar (scripts/lib/llvm-ar.sh).
#
# The patches are written against v0.38.2 and mio 1.0.3+wasix.1; another
# version stops here, at the patch that no longer applies.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${WORK:-$ROOT/target/agent-browser-wasix}"
REF="${AGENT_BROWSER_REF:-v0.38.2}"
SRC="$WORK/src"
PATCHES="$ROOT/experiments/agent-browser"

# shellcheck source=lib/llvm-ar.sh
source "$ROOT/scripts/lib/llvm-ar.sh"

mkdir -p "$WORK"
if [[ ! -d "$SRC/.git" ]]; then
  echo "cloning agent-browser $REF"
  git clone --quiet --depth 1 --branch "$REF" https://github.com/vercel-labs/agent-browser.git "$SRC"
fi

# From a clean tree each time, so the patches apply and nothing is doubled.
git -C "$SRC" checkout --quiet -- .
git -C "$SRC" apply "$PATCHES/wasix.patch"

cd "$SRC/cli"
# It sits under this repository's target/, so cargo would take it for an
# undeclared member of this workspace; it is a workspace of its own.
printf '\n[workspace]\n' >>Cargo.toml
cargo wasix init >/dev/null
# A per-package update cannot move only some crates to their forks: the
# graph resolves as a whole or not at all.
cargo update --quiet

mio_version="$(awk '/^name = "mio"$/ { getline; gsub(/"/, "", $3); print $3 }' Cargo.lock | head -1)"
cargo fetch --quiet
mio_src="$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"mio-$mio_version" 2>/dev/null | head -1 || true)"
if [[ -z "$mio_src" ]]; then
  echo "mio $mio_version was resolved but its source is not in the cargo registry" >&2
  exit 1
fi
rm -rf "$WORK/mio"
cp -R "$mio_src" "$WORK/mio"
patch --quiet -p1 -d "$WORK/mio" <"$PATCHES/mio-wasix-nonblocking.patch"
printf '\n[patch.crates-io]\nmio = { path = "%s" }\n' "$WORK/mio" >>Cargo.toml

if [[ -z "${AR_wasm32_wasmer_wasi:-}" ]]; then
  llvm_bin="$(find_llvm_archiver "$ROOT/target/wasm32-wasmer-wasi/release")"
  if [[ -z "$llvm_bin" ]]; then
    echo "$LLVM_AR_HELP" >&2
    echo "  or run scripts/wasix-build.sh first: it fetches a wasi-sdk this script finds" >&2
    exit 1
  fi
  use_llvm_archiver "$llvm_bin"
fi

cargo wasix build --release

cp target/wasm32-wasmer-wasi/release/agent-browser.wasm "$WORK/agent-browser.wasm"
printf '\nartefact: %s (%s bytes)\n' "$WORK/agent-browser.wasm" "$(wc -c <"$WORK/agent-browser.wasm" | tr -d ' ')"
