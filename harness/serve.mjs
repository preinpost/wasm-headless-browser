// Static server for the harness page, with the isolation headers the Wasmer SDK
// needs. SharedArrayBuffer requires a cross-origin isolated page, and without
// it the SDK refuses to start.
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import { extname, join, normalize } from 'node:path'

const PORT = Number(process.env.PORT ?? 8099)
const ROOT = join(import.meta.dirname, 'public')

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
  '.css': 'text/css; charset=utf-8',
}

createServer(async (req, res) => {
  const url = new URL(req.url, `http://localhost:${PORT}`)
  const path = url.pathname === '/' ? '/index.html' : url.pathname
  const file = join(ROOT, normalize(path).replace(/^(\.\.[/\\])+/, ''))

  try {
    const body = await readFile(file)
    res.writeHead(200, {
      'Content-Type': TYPES[extname(file)] ?? 'application/octet-stream',
      // The SDK needs SharedArrayBuffer, which needs these two.
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
      'Cache-Control': 'no-store',
    })
    res.end(body)
  } catch {
    res.writeHead(404, { 'Content-Type': 'text/plain' })
    res.end('not found')
  }
}).listen(PORT, () => {
  console.log(`harness on http://127.0.0.1:${PORT}/`)
})
