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
#   * One Cargo.lock cannot serve both registries. For the packages listed in
#     WASIX_FORKS the WASIX registry carries `+wasix` builds, which crates.io
#     does not have, so each registry fails on the other's lockfile. The
#     committed lockfile is the crates.io one. This script re-pins those
#     packages for the build, then
#     restores the lockfile and removes .cargo/config.toml, because with that
#     file present a native build would fail on the crates.io lockfile. Every
#     cargo-wasix command writes it again.
#   * C code (ring's, QuickJS's) is compiled to wasm objects and archived. The
#     archiver must understand wasm: macOS's own `ar` and `ranlib` do not, and
#     leave an archive with an empty symbol table, which then fails to link
#     with "undefined symbol: ring_core_...". The `cc` crate looks for
#     `llvm-ar` next to the C compiler, and Apple's clang has none, so this
#     script finds one (see find_llvm_archiver) and passes it on. Keeping
#     `sha1` out was deliberate: it reaches libc through cpufeatures, which
#     WASIX would then have to port.
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

# Packages the WASIX registry carries a `+wasix` fork of, that this workspace
# locks. A new dependency on another one fails resolution with "candidate
# versions found which didn't match: X+wasix.1"; add it here.
WASIX_FORKS=(libc cc ring getrandom)

TARGET_DIR="$ROOT/target/wasm32-wasmer-wasi/release"

# The directory of an `llvm-ar` that can archive wasm objects, or nothing.
# In order: wasi-sdk (also what rquickjs-sys reads), PATH, rustup's
# llvm-tools, Homebrew's LLVM, and last the wasi-sdk that rquickjs-sys
# fetched into this target directory for QuickJS.
find_llvm_archiver() {
  local candidates=()
  [[ -n "${WASI_SDK:-}" ]] && candidates+=("$WASI_SDK/bin")
  [[ -n "${WASI_SDK_PATH:-}" ]] && candidates+=("$WASI_SDK_PATH/bin")
  if command -v llvm-ar >/dev/null 2>&1; then
    candidates+=("$(dirname "$(command -v llvm-ar)")")
  fi
  local sysroot host
  if sysroot="$(rustc --print sysroot 2>/dev/null)" && host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"; then
    candidates+=("$sysroot/lib/rustlib/$host/bin")
  fi
  candidates+=(/opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin)
  local fetched
  fetched="$(ls -td "$TARGET_DIR"/build/rquickjs-sys-*/out/wasi-sdk/bin 2>/dev/null | head -1 || true)"
  [[ -n "$fetched" ]] && candidates+=("$fetched")
  local dir
  for dir in "${candidates[@]}"; do
    if [[ -x "$dir/llvm-ar" ]]; then
      echo "$dir"
      return
    fi
  done
}

cargo wasix init >/dev/null
cargo update --quiet "${WASIX_FORKS[@]/#/-p}"

# An archiver the caller chose is used as it is.
if [[ -z "${AR_wasm32_wasmer_wasi:-}" ]]; then
  llvm_bin="$(find_llvm_archiver)"
  if [[ -z "$llvm_bin" ]]; then
    # A clean build fetches wasi-sdk for QuickJS anyway; fetch it first and
    # use its llvm-ar for the rest of the C code too.
    echo "no wasm-capable llvm-ar found; fetching wasi-sdk through rquickjs-sys" >&2
    cargo wasix build --release -p rquickjs-sys >/dev/null
    llvm_bin="$(find_llvm_archiver)"
  fi
  if [[ -z "$llvm_bin" ]]; then
    cat >&2 <<'EOF'
no llvm-ar that understands wasm objects was found. Any one of these works:
  WASI_SDK=/path/to/wasi-sdk-24 scripts/wasix-build.sh
  rustup component add llvm-tools
  brew install llvm
EOF
    exit 1
  fi
  export AR_wasm32_wasmer_wasi="$llvm_bin/llvm-ar"
  if [[ -x "$llvm_bin/llvm-ranlib" ]]; then
    export RANLIB_wasm32_wasmer_wasi="$llvm_bin/llvm-ranlib"
  else
    export RANLIB_wasm32_wasmer_wasi="$llvm_bin/llvm-ar s"
  fi
  echo "archiving C code with $AR_wasm32_wasmer_wasi" >&2
fi

# ring's build script does not rerun when the archiver changes, so an archive
# a wrong `ar` left behind would keep failing the link. One with no members
# is removed together with ring's fingerprints, and ring is built again.
for archive in "$TARGET_DIR"/build/ring-*/out/libring_core_*_.a; do
  [[ -f "$archive" ]] || continue
  # Read the whole listing before judging it: `grep -q` stopping early could
  # SIGPIPE llvm-ar, and pipefail would then call a good archive empty.
  members="$("${AR_wasm32_wasmer_wasi%% *}" t "$archive" 2>/dev/null | grep -v '^__\.SYMDEF' || true)"
  if [[ -z "$members" ]]; then
    echo "removing an empty ring archive left by a non-wasm ar: $archive" >&2
    rm -rf "$TARGET_DIR"/build/ring-* "$TARGET_DIR"/.fingerprint/ring-*
    break
  fi
done

cargo wasix build --release "$@"

WASM="$ROOT/target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm"
if [[ ! -f "$WASM" ]]; then
  echo "expected artefact not found: $WASM" >&2
  exit 1
fi

printf '\nartefact: %s (%s)\n' "$WASM" "$(du -h "$WASM" | cut -f1)"
printf 'run it:   scripts/wasix-run.sh 127.0.0.1:9222\n'
