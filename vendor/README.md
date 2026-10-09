# Vendored dependencies

## rquickjs-sys 0.14.0

`rquickjs-sys` builds QuickJS (quickjs-ng) from C source. A copy lives here so
the C source can be patched, and the workspace `Cargo.toml` points at it with
`[patch.crates-io]`.

Only what `build.rs` compiles is kept from the published crate:

- `quickjs/`: the four `.c` files and the headers listed in `build.rs`, and
  QuickJS's `LICENSE`. Its tests, docs and tools are left out.
- `src/`, `wasm-shim/`, `build.rs`, `quickjs.bind.h`, `Cargo.toml` as published.
- `vendor/wasi-libc` is left out. `build.rs` needs it only for
  `wasm32-unknown-unknown`, which this project does not build.

### Patches

Each file in `patches/` is a diff against the published crate, applied
already. To upgrade rquickjs, copy the new `rquickjs-sys` the same way, then
apply them again from this directory:

```bash
patch -p1 -d rquickjs-sys < patches/0001-function-caller.patch
```

| Patch | Why |
|---|---|
| `0001-function-caller.patch` | `Function.prototype.caller` returns the calling function for sloppy functions, as V8 does. Upstream returns `undefined`. the grid library finds overridden methods through `this._super.caller`, and without it the reference site's bundle stops at load. |
