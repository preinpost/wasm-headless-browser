// The experiments.
//
//   E0  does the engine bind loopback inside the sandbox?
//   E1  can a second command in the SAME sandbox reach it over 127.0.0.1?
//   E2  does the SDK observe the guest listener (ports.wait)?
//   E3  can the HOST PAGE reach it (ports.expose + fetch)?
//   E4  can the sandbox reach the public internet at all?
//   E5  can the sandbox do a real HTTPS request (curl, so TLS is in the guest)?
//
// E1 is the one that decides the architecture. E4 decides whether crawling from
// inside the sandbox is possible, which is a separate question: the browser
// sandbox has no direct network access, so egress needs a WISP relay.
//
// Results land in `window.__harness` as well as the page.

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
  // `fake` is the binary's default (the M0 page model); `html` is the real
  // engine, with QuickJS and TLS, which is what the sandbox will run.
  const engineKind = params.get('engine')
  // The page the real engine loads in E6.
  const pageUrl = params.get('page') ?? 'https://example.com/'
  const wispUrl = params.get('wisp')
  const outbound = params.get('outbound')
  const outboundPath = params.get('outboundPath') ?? '/'
  const curlUrl = params.get('curl')

  log(`mode=${mode} port=${port} engine=${engineKind ?? 'default'}`)
  if (wispUrl) log(`wisp=${wispUrl}`)
  if (outbound) log(`outbound=${outbound}${outboundPath}`)
  if (curlUrl) log(`curl=${curlUrl}`)
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

  const network = mode === 'wisp' && wispUrl ? { mode, url: wispUrl } : { mode }
  log(`network=${JSON.stringify(network)}`)

  // Packages are fixed when the sandbox is created, so curl has to be fetched
  // first if the E5 probe is wanted.
  const packages = [pkg]
  let curlCommand = null
  if (curlUrl) {
    log('loading curl/curl from the registry...')
    const [curlPkg] = await wasmer.packages.loadMany(['curl/curl'])
    packages.push(curlPkg)
    curlCommand = curlPkg.commands?.[0] ?? 'curl'
    log(`curl package ready; commands=${JSON.stringify(curlPkg.commands ?? [])}`)
  }

  const sandbox = await wasmer.sandboxes.create({ packages, network })
  log('sandbox up')

  // ---- E4 first: egress does not depend on anything the engine does ------
  if (outbound) {
    const host = outbound.split(':')[0]
    try {
      const out = await sandbox
        .command('client', [outbound, outboundPath, host])
        .run({ timeoutMs: 60_000 })
      const text = out.stdout?.text?.() ?? ''
      for (const line of text.split('\n')) if (line.trim()) log(`  [egress] ${line}`)
      // A TLS port will not answer a plaintext request, so treat a completed
      // TCP connect as the signal too.
      const httpOk = /status: HTTP\/1\.[01] (200|301|302|403)/.test(text)
      const tcpOk = /connected on attempt \d+/.test(text)
      const ok = httpOk || tcpOk
      record(
        'E4 sandbox reaches the public internet',
        ok,
        httpOk
          ? (text.match(/status: .*/)?.[0] ?? 'HTTP response received')
          : tcpOk
            ? 'TCP connected (target speaks TLS, plaintext probe is expected to stall)'
            : (text.trim().split('\n').slice(-2).join(' | ') || 'no output'),
      )
    } catch (e) {
      record('E4 sandbox reaches the public internet', false, e?.message ?? String(e))
    }
  }

  // ---- E5: a real HTTPS request, with TLS inside the guest --------------
  if (curlUrl && curlCommand) {
    try {
      const out = await sandbox
        .command(curlCommand, ['-sS', '-i', '-L', '--max-time', '30', curlUrl])
        .run({ timeoutMs: 90_000 })
      const text = out.stdout?.text?.() ?? ''
      const err = (out.stderr?.text?.() ?? '').trim()
      if (err) log(`  [curl!] ${err.slice(0, 400)}`)

      const statusLine = text.split('\n')[0].trim()
      const headerBlock = text.split('\r\n\r\n')[0] ?? ''
      const title = /<title[^>]*>([^<]*)</i.exec(text)?.[1]?.trim() ?? ''
      const server = /^server:.*$/im.exec(headerBlock)?.[0]?.trim() ?? ''
      const ctype = /^content-type:.*$/im.exec(headerBlock)?.[0]?.trim() ?? ''
      const length = text.length

      log(`  [curl] ${statusLine}`)
      if (server) log(`  [curl] ${server}`)
      if (ctype) log(`  [curl] ${ctype}`)
      log(`  [curl] body ${length} bytes${title ? `, title: ${JSON.stringify(title)}` : ', no <title>'}`)
      // Showing a slice settles the question the probe was asked. Header
      // parsing above is best-effort; this is the raw truth.
      log(`  [curl] first 360 chars: ${JSON.stringify(text.slice(0, 360))}`)

      const code = (statusLine.match(/HTTP\/[\d.]+ (\d\d\d)/) ?? [])[1] ?? ''
      record(
        'E5 sandbox completes an HTTPS request',
        /^2\d\d$/.test(code) || /^3\d\d$/.test(code),
        statusLine
          ? `${statusLine}${title ? ` / title ${JSON.stringify(title)}` : ''}${length ? ` / ${length} bytes` : ''}`
          : err.slice(0, 200) || 'no output',
      )
    } catch (e) {
      record('E5 sandbox completes an HTTPS request', false, e?.message ?? String(e))
    }
  }

  // ---- E0: start the engine, watch its banner --------------------------
  const engineArgs = engineKind ? ['--engine', engineKind, `127.0.0.1:${port}`] : [`127.0.0.1:${port}`]
  const proc = await sandbox.command('engine', engineArgs).spawn({})
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
    const out = await sandbox
      .command('client', [`127.0.0.1:${port}`, '/json/version', `127.0.0.1:${port}`])
      .run({ timeoutMs: 30_000 })
    const text = out.stdout?.text?.() ?? ''
    for (const line of text.split('\n')) if (line.trim()) log(`  [client] ${line}`)
    const reached = /status: HTTP\/1\.[01] 200/.test(text) && /webSocketDebuggerUrl/.test(text)
    record(
      'E1 sandbox-internal client reaches the engine',
      reached,
      reached
        ? 'HTTP response came back over 127.0.0.1'
        : text.trim().split('\n').slice(-2).join(' | ') || 'no output',
    )
  } catch (e) {
    record('E1 sandbox-internal client reaches the engine', false, e?.message ?? String(e))
  }

  // ---- E6: the real engine loads and scripts a page ---------------------
  // A CDP client inside the sandbox drives the engine over a WebSocket on
  // loopback. The engine fetches the page through the WISP relay, with TLS
  // in the guest, and runs its JavaScript in QuickJS.
  if (engineKind === 'html') {
    try {
      const out = await sandbox
        .command('client', ['cdp', `127.0.0.1:${port}`, pageUrl])
        .run({ timeoutMs: 120_000 })
      const text = out.stdout?.text?.() ?? ''
      for (const line of text.split('\n')) if (line.trim()) log(`  [cdp] ${line}`)
      const loaded = /\[cdp\] RESULT: page loaded/.test(text)
      record(
        'E6 real engine loads a page in the sandbox',
        loaded,
        loaded
          ? (text.match(/\[cdp\] value: .*/)?.[0] ?? 'page loaded')
          : text.trim().split('\n').slice(-1)[0] || 'no output',
      )
    } catch (e) {
      record('E6 real engine loads a page in the sandbox', false, e?.message ?? String(e))
    }
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
    // Also expose a bare boolean: `eval "window.__harnessDone"` returns `true`
    // with no quoting, which is what a shell driver wants to test.
    window.__harnessDone = true
  })
