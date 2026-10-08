# QuickJS on WASIX

Recorded from working runs. Reproduce with `experiments/quickjs`:

```bash
cd experiments/quickjs
WASI_SDK=<path to an extracted wasi-sdk-24> cargo wasix build --release
wasmer run target/wasm32-wasmer-wasi/release/quickjs-probe.wasm
```

## Result

QuickJS runs inside WASIX. `rquickjs` 0.14 with the `full-wasi` and `bindgen`
features compiles `libquickjs.a` for `wasm32-wasmer-wasi`, links it into a WASIX
module, and evaluates real JavaScript:

```
[quickjs] building a custom context
[quickjs] context ready
[quickjs]   arithmetic: 2
[quickjs]   array methods: 2,4,6
[quickjs]   JSON: {"a":1,"b":[2,3]}
[quickjs]   Date construction: 1970-01-01T00:00:00.000Z
[quickjs]   Date.now: true
[quickjs]   RegExp: aBc
[quickjs]   Map: 1
[quickjs]   async functions: function
[quickjs]   Promise: object
[quickjs]   TypedArray: 3
[quickjs]   Proxy: object
[quickjs]   WeakRef: object
[quickjs]   a loop: 499500
```

## The trap: `Context::full` aborts

`Context::full(&runtime)` kills the process with no message. `Runtime::new()`
succeeds, `Context::base()` succeeds, and every individual
`Context::custom::<intrinsic::X>()` succeeds. Only `full` dies.

The cause is a chain of five steps, each of which was checked separately.

**1. `Context::full` calls `JS_NewContext`, and so does the raw FFI.**
`JS_NewContext` aborts too, so this is not a bug in rquickjs's wrapper.

**2. `JS_NewContext` calls `JS_AddPerformance`.** Reading the bundled QuickJS
source:

```c
if (JS_AddIntrinsicBaseObjects(ctx) || ... || JS_AddIntrinsicAToB(ctx) ||
    JS_AddPerformance(ctx)) {
    JS_FreeContext(ctx);
    return NULL;
}
```

**3. `JS_AddPerformance` reads a clock during setup.** It sets
`ctx->time_origin = js__now_ms()` before building the `performance` object.

**4. That clock call aborts on failure.**

```c
static inline uint64_t js__hrtime_ns(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t))
        abort();
    return t.tv_sec * NANOSEC + t.tv_nsec;
}
```

**5. `clock_gettime` fails because the clock id is wrong.** Measured directly:

```
clk=0        rc=0   sec=1791442084   nsec=975516000
clk=1        rc=0   sec=200799       nsec=57483000
clk=2        rc=0   sec=0            nsec=53594000
clk=3        rc=0   sec=0            nsec=382916
clk=1058440  rc=-1  sec=0            nsec=0
```

WASIX libc accepts the WASI preview1 integers 0 through 3. The value the
compiled C actually passed was **1058440**, which is `0x102688`.

## Why the clock id is 1058440

wasi-sdk ships **two** sets of headers in one sysroot:

| Directory | `CLOCK_MONOTONIC` |
|---|---|
| `include/wasm32-wasip1/` | `1` |
| `include/wasm32-wasi/` | `(&_CLOCK_MONOTONIC)` |

The second is the legacy glibc-style encoding, where a clock id is the *address*
of a global object. `(int)&_CLOCK_MONOTONIC` is `0x102688` = 1058440, which is
exactly what the probe printed.

clang maps `--target=wasm32-wasmer-wasi` onto the **legacy** directory:

```
$ echo '#include <time.h>' | clang --target=wasm32-wasmer-wasi \
    --sysroot=$SDK/share/wasi-sysroot -E -H -x c -
. /tmp/wasi-sdk-24/share/wasi-sysroot/include/wasm32-wasi/time.h
.. /tmp/wasi-sdk-24/share/wasi-sysroot/include/wasm32-wasi/__header_time.h
```

So the C half of the build is compiled against a libc convention that the WASIX
half does not implement. Everything else happens to agree, which is why only
this one function breaks.

## What did not work

Each of these was tried and had no effect; the clock id stayed at 1058440.

| Attempt | Why it fails |
|---|---|
| `-DCLOCK_MONOTONIC=1` | The header `#define`s it again afterwards, and a later define wins. Verified present on the clang command line with `-vv`. |
| `--target=wasm32-wasip1` in `CFLAGS_wasm32_wasmer_wasi` | The `cc` crate appends its own `--target` afterwards, so it is overridden. |
| `-I .../include/wasm32-wasip1` | The header still resolved to the legacy directory. |

## The workaround that works today

`JS_NewContextRaw` plus an explicit intrinsic list skips `JS_AddPerformance`.
Every intrinsic `JS_NewContext` installs was called individually through raw FFI
and returned 0:

```
JS_AddIntrinsicBaseObjects -> 0
JS_AddIntrinsicDate -> 0
JS_AddIntrinsicEval -> 0
JS_AddIntrinsicRegExp -> 0
JS_AddIntrinsicJSON -> 0
JS_AddIntrinsicProxy -> 0
JS_AddIntrinsicMapSet -> 0
JS_AddIntrinsicTypedArrays -> 0
JS_AddIntrinsicPromise -> 0
JS_AddIntrinsicWeakRef -> 0
JS_AddIntrinsicAToB -> 0
```

So the engine builds its context with `Context::custom::<Intrinsics>`, where
`Intrinsics` is that list minus `Performance`. The cost is small and arguably
correct: `performance` is a host facility in a browser, so the engine should
supply it from the sandbox rather than let QuickJS read the wall clock. `atob`
and `btoa` also come from `JS_AddIntrinsicAToB`, which `custom` cannot reach
because rquickjs exposes no intrinsic type for it; they can be added as host
functions.

`Date.now()` is unaffected, because `Date` uses a different time path
(`js__gettimeofday_us`), and the probe confirms it returns a sane value.

## The proper fix

Compile QuickJS against the **WASIX sysroot** instead of wasi-sdk's, using
`wasixcc`:

```bash
cargo install cargo-wasix
curl -fsSL https://wasix.cc | sh
. ~/.wasixcc/env
RQUICKJS_SYS_NO_WASI_SDK=1 \
  CC_wasm32_wasmer_wasi="$WASIXCC" \
  ... cargo wasix build --release
```

That removes the mismatch at the source rather than working around it, and it
should make `Context::full` and `performance.now()` work. It has not been tried
yet, and the install is a large one: `wasix.cc` fetches an LLVM and binaryen
sysroot into `~/.wasixcc` and appends an init block to the shell rc files.

Until then the workaround is sufficient, so this is a choice rather than a
blocker.

## A separate rquickjs bug, unrelated to WASIX

`rquickjs::context::intrinsic::Performance` maps to
`JS_AddIntrinsicPerformance`, which **does not exist** in the QuickJS that
rquickjs 0.14 bundles. The real symbol is `JS_AddPerformance`. Calling
`intrinsic::Performance` therefore invokes a stale name. It is worth not using
that type whether or not the sysroot is fixed.

## Build notes

- `bindgen` is required, not optional. rquickjs ships pre-generated bindings for
  `wasm32-wasip1` and `wasm32-wasip2` but not for `wasm32-wasmer-wasi`, and the
  fallback path looks the bindings up by exact target triple: `bindings/wasm32-wasmer-wasi.rs`.
  Without the feature the build fails at `include!`. With it, bindgen generates
  them from `quickjs.bind.h`; the output was checked and contains the expected
  declarations, including `JS_AddIntrinsicAToB` and `JS_NewContext`.
- `bindgen` needs libclang on the build host. `/Library/Developer/CommandLineTools/usr/lib/libclang.dylib`
  was found automatically.
- rquickjs-sys downloads a wasi-sdk (~200 MB) into `OUT_DIR` on every clean
  build unless `WASI_SDK` points at an extracted one. Set `WASI_SDK` to a stable
  path to avoid repeating that download.
- The probe module is about 1.1 MB, against 272 KB for the engine without a
  JavaScript engine.
