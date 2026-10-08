// The experiment.
//
// Question: inside the @wasmer/sdk browser sandbox, can a WASIX process listen
// on loopback and be reached?
//
//   E0  does the engine bind at all inside the sandbox?
//   E1  can a second command in the SAME sandbox reach it over 127.0.0.1?
//       This is the one that decides the architecture. If it works, the engine
//       and its client can both live in the sandbox.
//   E2  does the SDK observe the guest listener (ports.wait)?
//   E3  can the HOST PAGE reach it (ports.expose + fetch)?
//       Only meaningful with network mode "http".
//
// Results land in `window.__harness` as well as the page, so a driver can read
// them without scraping text.

import { Wasmer } from '/wasmer-sdk/dist/index.js'

const el = document.getElementById('log')
const results = {}

function log(line = '', cls = '') {
  const span = document.createElement('span')
  if (cls) span.className = cls
  span.textContent = line + '\n'
  el.appendChild(span)
  console.log('[harness] ' + line)
}

function record(name, ok, detail) {
  results[name] = { ok, detail }
  log(`${ok ? 'PASS' : 'FAIL'}  ${name}: ${detail}`, ok ? 'ok' : 'bad')
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function main() {
  const params = new URLSearchParams(location.search)
  const mode = params.get('mode') ?? 'http'
  const port = Number(params.get('port') ?? 9222)

  log(`mode=${mode} port=${port}`)
  log(`crossOriginIsolated=${window.crossOriginIsolated}`)
  log(`JSPI=${typeof WebAssembly.Suspending === 'function'}`)

  if (!window.crossOriginIsolated) {
    record('isolation', false, 'page is not cross-origin isolated')
    return
  }

  const wasmer = new Wasmer()
  const [engineBytes, clientBytes] = await Promise.all([
    fetch('/engine.wasm').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b)),
    fetch('/loopback-client.wasm').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b)),
  ])
  log(`engine.wasm ${engineBytes.byteLength} bytes, loopback-client.wasm ${clientBytes.byteLength} bytes`)

  const pkg = await wasmer.packages.create({
    modules: { engine: engineBytes, client: clientBytes },
    commands: { engine: { module: 'engine' }, client: { module: 'client' } },
  })
  log(`package ${pkg.id}; commands=${JSON.stringify(pkg.commands ?? {})}`)

  const sandbox = await wasmer.sandboxes.create({
    packages: [pkg],
    network: { mode },
  })
  log('sandbox up')

  // ---- E0: start the engine, watch its banner --------------------------
  const proc = await sandbox.command('engine', [`127.0.0.1:${port}`]).spawn({})
  let engineOutput = ''

  // ReadableBytes exposes an async generator of lines, not a ReadableStream.
  void (async () => {
    for await (const line of proc.stdout.lines()) {
      engineOutput += line + '\n'
      if (line.trim()) log(`  [engine] ${line}`)
    }
  })().catch(() => {})
  void (async () => {
    for await (const line of proc.stderr.lines()) {
      if (line.trim()) log(`  [engine!] ${line}`)
    }
  })().catch(() => {})

  await sleep(2000)
  record(
    'E0 engine binds loopback in the sandbox',
    /discovery/.test(engineOutput),
    engineOutput.trim() ? engineOutput.trim().split('\n')[0] : 'no output from the engine',
  )

  // ---- E2: does the SDK see the listener? ------------------------------
  if (sandbox.ports?.wait) {
    try {
      await sandbox.ports.wait(port, { timeoutMs: 5000 })
      record('E2 ports.wait observes the listener', true, `port ${port} is listening`)
    } catch (e) {
      record('E2 ports.wait observes the listener', false, e?.message ?? String(e))
    }
  } else {
    record('E2 ports.wait observes the listener', false, 'sandbox.ports.wait is not exposed')
  }

  // ---- E1: reach it from inside the same sandbox -----------------------
  try {
    const out = await sandbox.command('client', [`127.0.0.1:${port}`]).run({ timeoutMs: 30_000 })
    const text = out.stdout?.text?.() ?? ''
    for (const line of text.split('\n')) if (line.trim()) log(`  [client] ${line}`)
    const reached = /RESULT: got \d+ bytes back/.test(text) && /webSocketDebuggerUrl/.test(text)
    record(
      'E1 sandbox-internal client reaches the engine',
      reached,
      reached ? 'HTTP response came back over 127.0.0.1' : text.trim().split('\n').slice(-2).join(' | ') || 'no output',
    )
  } catch (e) {
    record('E1 sandbox-internal client reaches the engine', false, e?.message ?? String(e))
  }

  // ---- E3: can the host page reach in? ---------------------------------
  if (mode === 'http' && sandbox.ports?.expose) {
    try {
      const server = await sandbox.ports.expose(port, { timeoutMs: 10_000 })
      log(`exposed at ${server?.url ?? '(unknown url)'}`)
      const res = await fetch(new URL('/json/version', server.url))
      const body = await res.text()
      record(
        'E3 host page reaches the engine',
        res.ok && /webSocketDebuggerUrl/.test(body),
        `HTTP ${res.status} ${body.slice(0, 80)}`,
      )
    } catch (e) {
      record('E3 host page reaches the engine', false, e?.message ?? String(e))
    }
  } else {
    record('E3 host page reaches the engine', false, `skipped (mode=${mode})`)
  }

  log('')
  log('summary: ' + JSON.stringify(results, null, 1))
}

main()
  .catch((e) => {
    log('HARNESS ERROR: ' + (e?.stack ?? String(e)), 'bad')
    results.error = { ok: false, detail: String(e?.message ?? e) }
  })
  .finally(() => {
    window.__harness = { done: true, results }
  })
