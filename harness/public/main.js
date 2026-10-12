// The experiments.
//
//   E0  does the engine bind loopback inside the sandbox?
//   E1  can a second command in the SAME sandbox reach it over 127.0.0.1?
//   E2  does the SDK observe the guest listener (ports.wait)?
//   E3  can the HOST PAGE reach it (ports.expose + fetch)?
//   E4  can the sandbox reach the public internet at all?
//   E5  can the sandbox do a real HTTPS request (curl, so TLS is in the guest)?
//   E6  does the real engine load and script a page?
//   E7  does whb drive the engine?
//   E8  does agent-browser drive the engine?
//   E9  do repeated page loads keep navigating and evaluating?
//
// E1 is the one that decides the architecture. E4 decides whether crawling from
// inside the sandbox is possible, which is a separate question: the browser
// sandbox has no direct network access, so egress needs a WISP relay.
//
// Results land in `window.__harness` as well as the page.

import { Wasmer } from '/wasmer-sdk/dist/index.js'
import { BrowserSession } from './browser-session.js'

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
  let sandbox, browser, engineProcess
  try {
    const [engineBytes, clientBytes, whbBytes, agentBrowserBytes] = await Promise.all([
      fetch('/engine.wasm').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b)),
      fetch('/loopback-client.wasm').then((r) => r.arrayBuffer()).then((b) => new Uint8Array(b)),
      // Optional: an older public/ may not have it, and E7 is then skipped.
      fetch('/whb.wasm').then((r) => (r.ok ? r.arrayBuffer().then((b) => new Uint8Array(b)) : null)),
      // Optional, and not built here: agent-browser patched for WASIX
      // (experiments/agent-browser). E8 runs only when it is served.
      fetch('/agent-browser.wasm').then((r) => (r.ok ? r.arrayBuffer().then((b) => new Uint8Array(b)) : null)),
    ])
    log(
      `engine.wasm ${engineBytes.byteLength} bytes, loopback-client.wasm ${clientBytes.byteLength} bytes` +
        (whbBytes ? `, whb.wasm ${whbBytes.byteLength} bytes` : ', no whb.wasm'),
    )

    const modules = { engine: engineBytes, client: clientBytes }
    const commands = { engine: { module: 'engine' }, client: { module: 'client' } }
    if (whbBytes) {
      modules.whb = whbBytes
      commands.whb = { module: 'whb' }
    }
    if (agentBrowserBytes) {
      log(`agent-browser.wasm ${agentBrowserBytes.byteLength} bytes`)
      modules['agent-browser'] = agentBrowserBytes
      commands['agent-browser'] = { module: 'agent-browser' }
    }
    const pkg = await wasmer.packages.create({ modules, commands })
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

    sandbox = await wasmer.sandboxes.create({ packages, network })
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
    let engineOutput = ''
    const onLog = ({ process, stream, line }) => {
      if (process === 'engine' && stream === 'stdout') engineOutput += line + '\n'
      if (line.trim()) log(`  [${process}${stream === 'stderr' ? '!' : ''}] ${line}`)
    }
    if (engineKind === 'html' && agentBrowserBytes) {
      browser = new BrowserSession(sandbox, {
        port, traceResources: params.get('resources') === '1', onLog,
      })
      await browser.start()
      log(`browser adapter ready; session=${browser.session}`)
    } else {
      const engineArgs = engineKind ? ['--engine', engineKind, `127.0.0.1:${port}`] : [`127.0.0.1:${port}`]
      const engineEnv = params.get('resources') === '1' ? { WHB_TRACE_RESOURCES: '1' } : {}
      engineProcess = await sandbox.command('engine', engineArgs, { env: engineEnv }).spawn({})
      // ReadableBytes exposes an async generator of lines, not a ReadableStream.
      for (const stream of ['stdout', 'stderr']) {
        void (async () => {
          for await (const line of engineProcess[stream].lines()) onLog({ process: 'engine', stream, line })
        })().catch(() => {})
      }
    }

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

    // ---- E7: whb drives the engine, one command per process ---------------
    // As an agent in the sandbox would: each step is its own process, and the
    // tab and the refs carry over in a state file in the sandbox's filesystem.
    if (engineKind === 'html' && whbBytes) {
      // The sandbox's own filesystem lives under /workspace, and every command
      // in it sees it. A command's /tmp is its own.
      const stateDir = '/workspace/.whb'
      const whb = async (...args) => {
        const out = await sandbox
          .command('whb', ['--cdp', `127.0.0.1:${port}`, ...args], { env: { WHB_STATE_DIR: stateDir } })
          .run({ timeoutMs: 120_000, check: false })
        const text = (out.stdout?.text?.() ?? '') + (out.stderr?.text?.() ?? '')
        for (const line of text.split('\n')) if (line.trim()) log(`  [whb ${args[0]}] ${line}`)
        return text
      }
      try {
        await sandbox.fs.mkdir(stateDir, { recursive: true })
        const opened = await whb('open', pageUrl)
        const kept = await sandbox.fs.readDir(stateDir).then((e) => e.map((x) => x.name).join(','), (e) => `unreadable: ${e}`)
        log(`  [whb] state after open: ${kept}`)
        const snapshot = await whb('snapshot', '-i')
        const title = await whb('eval', 'document.title')
        const refs = (snapshot.match(/ref=e\d+/g) ?? []).length
        const ok = opened.startsWith('\u2713') && refs > 0 && /^".*"\s*$/.test(title.trim())
        record(
          'E7 whb drives the engine in the sandbox',
          ok,
          ok ? `title ${title.trim()}, ${refs} refs` : [opened, snapshot, title].join(' | ').slice(0, 200),
        )
      } catch (e) {
        record('E7 whb drives the engine in the sandbox', false, e?.message ?? String(e))
      }
    }

    // ---- E8: agent-browser through the reusable browser adapter ----------
    if (browser) {
      const ab = async (...args) => {
        const out = await browser.run(args)
        for (const stream of ['stdout', 'stderr']) {
          for (const line of out[stream].split('\n')) if (line.trim()) log(`  [ab ${args[0]} ${stream}] ${line}`)
        }
        return out
      }
      try {
        const opened = await ab('open', pageUrl)
        const snapshot = opened.ok ? await ab('snapshot', '-i') : null
        const title = snapshot?.ok ? await ab('eval', 'document.title') : null
        const refs = (snapshot?.stdout.match(/ref=e\d+/g) ?? []).length
        const ok = opened.ok && snapshot?.ok && title?.ok && refs > 0 && /^".*"\s*$/.test(title.stdout.trim())
        record(
          'E8 agent-browser drives the engine in the sandbox', Boolean(ok),
          ok ? `title ${title.stdout.trim()}, ${refs} refs` : JSON.stringify({ opened, snapshot, title }).slice(0, 300),
        )
      } catch (e) {
        record('E8 agent-browser drives the engine in the sandbox', false, e?.message ?? String(e))
      }
    }

    // ---- E9: repeated heavy loads in the same engine ---------------------
    const loads = Number(params.get('loads') ?? 1)
    if (engineKind === 'html' && loads > 1) {
      try {
        const out = await sandbox
          .command('client', ['cdp', `127.0.0.1:${port}`, pageUrl, String(loads)])
          .run({ timeoutMs: Math.max(60_000, loads * 30_000) })
        const text = out.stdout?.text?.() ?? ''
        for (const line of text.split('\n')) if (line.trim()) log(`  [repeat] ${line}`)
        const completed = (text.match(/\[cdp\] load value:/g) ?? []).length
        record(
          'E9 repeated page loads keep the engine alive',
          out.ok && completed === loads && /\[cdp\] RESULT: page loaded/.test(text),
          `${completed}/${loads} navigations and evaluations completed`,
        )
      } catch (e) {
        record('E9 repeated page loads keep the engine alive', false, e?.message ?? String(e))
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

    // ---- E10: result semantics and ownership, not a scripted CLI wrapper --
    if (browser) {
      try {
        log('adapter check: stdin')
        const input = await browser.run(['eval', '--stdin'], { stdin: '[1,2,3].reduce((a,b) => a+b)' })
        const failed = await browser.run(['not-a-real-command'])
        const clipped = await browser.run(['eval', "'x'.repeat(4096)"], { outputBytes: 128 })
        log('adapter check: abort')
        const controller = new AbortController()
        const cancelling = browser.run(['wait', '10000'], { signal: controller.signal })
        const abortTimer = setTimeout(() => controller.abort(), 200)
        let aborted
        try { aborted = await cancelling } finally { clearTimeout(abortTimer) }
        log('adapter check: timeout')
        const timed = await browser.run(['wait', '10000'], { timeoutMs: 20 })
        log('adapter check: stop')
        await browser.stop()
        await browser.stop()
        const refused = await browser.run(['snapshot']).then(() => false, (e) => e.code === 'SESSION_UNAVAILABLE')
        // SDK TCP cleanup can lag process exit. Use a different port for an
        // immediate fresh instance in this caller-owned sandbox.
        log('adapter check: fresh session')
        const freshPort = port === 65535 ? port - 1 : port + 1
        const fresh = new BrowserSession(sandbox, { port: freshPort, onLog })
        let renewed = false
        try {
          await fresh.start()
          const opened = await fresh.run(['open', 'about:blank'])
          const url = await fresh.run(['get', 'url'])
          renewed = opened.ok && url.ok && url.stdout.trim() === 'about:blank'
        } finally { await fresh.stop() }
        const ok = input.ok && input.stdout.trim() === '6' && !failed.ok && failed.exitCode !== 0 &&
          failed.reason === 'exited' && clipped.ok && clipped.stdoutTruncated && timed.timedOut &&
          aborted.aborted && aborted.reason === 'aborted' && browser.state === 'stopped' && refused && renewed
        record('E10 browser adapter returns results and stops owned processes', ok,
          JSON.stringify({ stdin: input.stdout.trim(), exitCode: failed.exitCode, stderr: Boolean(failed.stderr),
            truncated: clipped.stdoutTruncated, abort: aborted.reason, timeout: timed.reason,
            state: browser.state, refused, freshSession: renewed }))
      } catch (e) {
        record('E10 browser adapter returns results and stops owned processes', false, e?.message ?? String(e))
      }
    }
  } finally {
    // Always clean up, including a partially initialized adapter. The page
    // owns the sandbox/client; BrowserSession only owns its guest processes.
    const errors = []
    for (const [name, close] of [
      ['browser', () => browser?.stop()],
      ['engine', () => engineProcess?.terminate({ gracePeriodMs: 250 })],
      ['sandbox', () => sandbox?.close()],
    ]) {
      log(`closing ${name}`)
      try { await close(); log(`closed ${name}`) } catch (error) { errors.push(error) }
    }
    // SDK 0.19.0 close() can remain pending even after the sandbox closed.
    // Request client shutdown, but bound this separate harness-level wait.
    // The runner closes the Chrome page, releasing its workers afterwards.
    let timer
    try {
      const closed = await Promise.race([
        wasmer.close().then(() => true),
        new Promise((resolve) => { timer = setTimeout(() => resolve(false), 1000) }),
      ])
      log(closed ? 'closed client' : 'SDK client shutdown still pending; page teardown will release it')
    } catch (error) { errors.push(error) } finally { clearTimeout(timer) }
    if (errors.length) throw new AggregateError(errors, 'harness cleanup failed')
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
