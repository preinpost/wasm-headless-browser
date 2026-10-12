# Finding an llvm-ar that can archive wasm objects. Sourced by
# wasix-build.sh and build-agent-browser-wasix.sh.
#
# macOS's own `ar` and `ranlib` cannot read wasm objects: they leave an
# archive with an empty symbol table, and the link then fails with
# "undefined symbol: ring_core_...". The `cc` crate looks for `llvm-ar` next
# to the C compiler, and Apple's clang has none, so the build passes one in
# AR_wasm32_wasmer_wasi and RANLIB_wasm32_wasmer_wasi.

# The directory of a wasm-capable `llvm-ar`, or nothing. In order: wasi-sdk
# (also what rquickjs-sys reads), PATH, rustup's llvm-tools, Homebrew's LLVM,
# and last the wasi-sdk rquickjs-sys fetched into the engine's target
# directory, $1.
find_llvm_archiver() {
  local target_dir="${1:-}"
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
  if [[ -n "$target_dir" ]]; then
    local fetched
    fetched="$(ls -td "$target_dir"/build/rquickjs-sys-*/out/wasi-sdk/bin 2>/dev/null | head -1 || true)"
    [[ -n "$fetched" ]] && candidates+=("$fetched")
  fi
  local dir
  for dir in "${candidates[@]}"; do
    if [[ -x "$dir/llvm-ar" ]]; then
      echo "$dir"
      return
    fi
  done
}

# Exports AR_ and RANLIB_wasm32_wasmer_wasi for the llvm-ar in $1.
use_llvm_archiver() {
  local llvm_bin="$1"
  export AR_wasm32_wasmer_wasi="$llvm_bin/llvm-ar"
  if [[ -x "$llvm_bin/llvm-ranlib" ]]; then
    export RANLIB_wasm32_wasmer_wasi="$llvm_bin/llvm-ranlib"
  else
    export RANLIB_wasm32_wasmer_wasi="$llvm_bin/llvm-ar s"
  fi
  echo "archiving C code with $AR_wasm32_wasmer_wasi" >&2
}

LLVM_AR_HELP='no llvm-ar that understands wasm objects was found. Any one of these works:
  WASI_SDK=/path/to/wasi-sdk-24 <the build script>
  rustup component add llvm-tools
  brew install llvm'
