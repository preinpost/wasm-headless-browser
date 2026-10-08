// Serve @wasmer/sdk from public/wasmer-sdk/ with its original layout.
//
// The SDK resolves its worker and wasm-bindgen module relative to
// `import.meta.url`, so the files have to stay siblings. Only the main entry is
// bundled, and only to inline its one bare import (wisp-js).
import { build } from 'esbuild'
import { cpSync, rmSync } from 'node:fs'

const src = 'node_modules/@wasmer/sdk'
const out = 'public/wasmer-sdk'

rmSync(out, { recursive: true, force: true })
cpSync(`${src}/pkg`, `${out}/pkg`, { recursive: true })
cpSync(`${src}/dist`, `${out}/dist`, {
  recursive: true,
  filter: (f) => !f.endsWith('.map') && !f.endsWith('.d.ts'),
})

await build({
  entryPoints: [`${src}/dist/index.js`],
  outfile: `${out}/dist/index.js`,
  allowOverwrite: true,
  bundle: true,
  format: 'esm',
  platform: 'browser',
  external: ['../pkg/*'],
  logLevel: 'warning',
})

console.log('vendored @wasmer/sdk into', out)
