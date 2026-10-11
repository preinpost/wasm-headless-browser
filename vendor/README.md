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
patch -p1 -d rquickjs-sys < patches/0002-proxy-global-reference-error.patch
```

| Patch | Why |
|---|---|
| `0002-proxy-global-reference-error.patch` | A bare global name that reaches an exotic object on the global object's prototype chain asks its `has` before its `get`, so an undeclared name is still a `ReferenceError`. Upstream marks the spot `XXX: should pass throw_ref_error` and returns whatever `get` gives, `undefined`. The engine puts a proxy there for HTML's named access (`window.menu` for `<div id=menu>`). The patch also makes `build.rs` rerun when `quickjs/` changes; without that, a patched C file was not recompiled. |
| `0001-function-caller.patch` | `Function.prototype.caller` returns the calling function for sloppy functions, as V8 does. Upstream returns `undefined`. the grid library finds overridden methods through `this._super.caller`, and without it the reference site's bundle stops at load. |
