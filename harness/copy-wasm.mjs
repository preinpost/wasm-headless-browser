// Build the two probes this harness needs, straight into public/.
//
// The engine comes from the normal WASIX build. The client is compiled from a
// single-file probe, so no cargo project is involved.
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'

mkdirSync('public', { recursive: true })

const root = new URL('..', import.meta.url).pathname

execFileSync(
  'rustup',
  [
    'run',
    'wasix',
    'rustc',
    '--target',
    'wasm32-wasmer-wasi',
    '-O',
    `${root}scripts/wasix-loopback-client.rs`,
    '-o',
    'public/loopback-client.wasm',
  ],
  { stdio: 'inherit' },
)

copyFileSync(
  `${root}target/wasm32-wasmer-wasi/release/wasm-headless-browser.wasm`,
  'public/engine.wasm',
)

console.log('copied engine.wasm and built loopback-client.wasm into public/')
