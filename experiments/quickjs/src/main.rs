//! Does QuickJS run inside WASIX?
//!
//! Two questions, deliberately separated:
//!
//!   1. Does `Context::full` work? It does NOT, and the reason is a sysroot
//!      mismatch rather than anything about WASIX. See docs/quickjs.md.
//!   2. Does a `Context::custom` context, built from `JS_NewContextRaw` plus an
//!      explicit intrinsic list, evaluate real JavaScript? It does.
//!
//! Build and run:
//!
//!   cargo wasix build --release
//!   wasmer run target/wasm32-wasmer-wasi/release/quickjs-probe.wasm

use std::io::Write;

use rquickjs::{context::intrinsic, Context, Runtime};

fn step(msg: &str) {
    println!("[quickjs] {msg}");
    let _ = std::io::stdout().flush();
}

/// Everything `JS_NewContext` installs, minus `JS_AddPerformance`.
///
/// `JS_AddPerformance` is what aborts, because it is the only intrinsic that
/// reads a clock during setup. A browser engine gets `performance` from its
/// host anyway, so losing it here is not a loss.
type Intrinsics = (
    intrinsic::Eval,
    intrinsic::Json,
    intrinsic::Date,
    intrinsic::RegExp,
    intrinsic::RegExpCompiler,
    intrinsic::MapSet,
    intrinsic::Promise,
    intrinsic::TypedArrays,
    intrinsic::Proxy,
    intrinsic::WeakRef,
    intrinsic::DOMException,
);

const CASES: &[(&str, &str)] = &[
    ("1 + 1", "arithmetic"),
    ("[1,2,3].map(x => x * 2).join(',')", "array methods"),
    ("JSON.stringify({a: 1, b: [2, 3]})", "JSON"),
    ("new Date(0).toISOString()", "Date construction"),
    ("Date.now() > 0", "Date.now"),
    ("'abc'.replace(/b/, 'B')", "RegExp"),
    ("new Map([['a', 1]]).get('a')", "Map"),
    ("typeof (async function () {})", "async functions"),
    ("typeof Promise.resolve(7).then(v => v)", "Promise"),
    ("new Uint8Array([1,2,3]).length", "TypedArray"),
    ("typeof new Proxy({}, {})", "Proxy"),
    ("typeof new WeakRef({})", "WeakRef"),
    ("(() => { let s = 0; for (let i = 0; i < 1000; i++) s += i; return s })()", "a loop"),
    ("typeof atob", "atob (absent by design)"),
];

fn main() {
    step("building a custom context");
    let runtime = Runtime::new().expect("runtime");
    let context = Context::custom::<Intrinsics>(&runtime).expect("context");
    step("context ready");

    context.with(|ctx| {
        for (expr, label) in CASES {
            // Wrap in String() so the result can be read back as text whatever
            // the expression returns.
            let wrapped = format!("String({expr})");
            match ctx.eval::<String, _>(wrapped.as_str()) {
                Ok(value) => step(&format!("  {label}: {value}")),
                Err(e) => step(&format!("  {label}: ERR {e}")),
            }
        }
    });

    step("done");
}
