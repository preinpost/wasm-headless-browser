import { test } from 'node:test'
import assert from 'node:assert/strict'
import { BrowserSession, BrowserSessionError } from '../public/browser-session.js'

function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

function output({ stdout = '', stderr = '', exitCode = 0, reason = 'exited', truncated = false } = {}) {
  return {
    exitCode, reason,
    stdout: { text: () => stdout, truncated },
    stderr: { text: () => stderr, truncated: false },
  }
}

class FakeProcess {
  done = deferred()
  exited = false
  kills = 0
  terminations = 0
  input = []
  inputClosed = false
  constructor(role, opts = {}) {
    this.role = role
    this.stdout = { lines: async function* () {
      if (role === 'engine' && !opts.noBanner) yield 'discovery: fake engine'
      await this.done.promise
      if (opts.noPipeEOF) await new Promise(() => {})
    }.bind(this) }
    this.stderr = { lines: async function* () {
      if (opts.log) yield opts.log
      await this.done.promise
      if (opts.noPipeEOF) await new Promise(() => {})
    }.bind(this) }
    this.stdin = {
      write: async (data) => {
        if (opts.inputError) throw new Error('stdin failed')
        this.input.push(data)
        if (opts.writeGate) await opts.writeGate.promise
      },
      close: async () => { this.inputClosed = true },
    }
  }
  wait() { return this.done.promise }
  finish(value = {}) {
    if (this.exited) return
    this.exited = true
    this.done.resolve(output(value))
  }
  async kill() {
    this.kills++
    this.finish({ reason: 'terminated', exitCode: 137 })
  }
  async terminate() {
    this.terminations++
    this.finish({ reason: 'terminated', exitCode: 143 })
  }
}

class FakeSandbox {
  calls = []
  processes = []
  files = new Map()
  removed = []
  dirs = new Set(['/workspace'])
  closed = false
  engineExit = false
  daemonExit = false
  noPortFile = false
  noEngineBanner = false
  noPipeEOF = false
  portError
  cli = () => ({ stdout: 'about:blank\n' })
  spawnGate
  constructor() {
    this.fs = {
      mkdir: async (path) => { this.dirs.add(path) },
      readText: async (path) => {
        if (!this.files.has(path)) throw Object.assign(new Error('not found'), { code: 'FILESYSTEM_ERROR' })
        return this.files.get(path)
      },
      remove: async (path) => {
        this.removed.push(path)
        for (const key of this.files.keys()) if (key.startsWith(`${path}/`)) this.files.delete(key)
        for (const key of this.dirs) if (key === path || key.startsWith(`${path}/`)) this.dirs.delete(key)
      },
    }
    this.ports = { wait: async () => {
      if (this.portError) throw this.portError
    } }
  }
  command(selector, argv, commandOptions) {
    return { spawn: async (spawnOptions) => {
      const role = selector === 'engine' ? 'engine' : commandOptions.env.AGENT_BROWSER_DAEMON ? 'daemon' : 'cli'
      const call = { selector, argv: [...argv], commandOptions, spawnOptions, role }
      this.calls.push(call)
      if (role === 'cli' && this.spawnGate) await this.spawnGate.promise
      const opts = role === 'cli' ? this.cli(argv.slice(2), call) : {
        noBanner: role === 'engine' && this.noEngineBanner, noPipeEOF: this.noPipeEOF,
      }
      const proc = new FakeProcess(role, opts)
      call.proc = proc
      this.processes.push(proc)
      if (role === 'daemon' && !this.noPortFile) {
        this.files.set(`${commandOptions.env.AGENT_BROWSER_SOCKET_DIR}/${commandOptions.env.AGENT_BROWSER_SESSION}.port`, '52729')
      }
      if ((role === 'engine' && this.engineExit) || (role === 'daemon' && this.daemonExit)) {
        queueMicrotask(() => proc.finish({ exitCode: 1, stderr: 'service failed' }))
      }
      if (role === 'cli' && !opts.pending) queueMicrotask(() => proc.finish(opts))
      if (role === 'cli' && opts.timeout) {
        const timer = setTimeout(() => proc.finish({ reason: 'timeout', exitCode: 124, stderr: 'partial error' }), spawnOptions.timeoutMs)
        void proc.wait().finally(() => clearTimeout(timer))
      }
      return proc
    } }
  }
  async close() { this.closed = true }
}

async function ready(t, options = {}) {
  const sandbox = new FakeSandbox()
  const browser = new BrowserSession(sandbox, options)
  t.after(() => browser.stop())
  await browser.start()
  return { sandbox, browser }
}

async function until(condition) {
  const deadline = Date.now() + 1000
  while (!condition()) {
    assert.ok(Date.now() < deadline, 'test condition timed out')
    await new Promise((resolve) => setTimeout(resolve, 1))
  }
}

const code = (expected) => (error) => error instanceof BrowserSessionError && error.code === expected

test('startup is idempotent and proves the daemon can reach the engine', async (t) => {
  const sandbox = new FakeSandbox()
  const browser = new BrowserSession(sandbox)
  t.after(() => browser.stop())
  const first = browser.start()
  assert.equal(browser.state, 'starting')
  assert.equal(browser.start(), first)
  assert.equal(await first, browser)
  assert.equal(await browser.start(), browser)
  assert.equal(browser.state, 'ready')
  assert.equal(sandbox.calls.filter((c) => c.role === 'engine').length, 1)
  assert.deepEqual(sandbox.calls.find((c) => c.role === 'cli').argv, ['--cdp', browser.cdpUrl, 'get', 'url'])
})

test('startup owns a unique HOME/socket directory, session and endpoint', async (t) => {
  const { browser, sandbox } = await ready(t, { port: 9333, traceResources: true })
  assert.equal(browser.cdpUrl, 'http://127.0.0.1:9333')
  const engine = sandbox.calls.find((c) => c.role === 'engine')
  assert.deepEqual(engine.argv, ['--engine', 'html', '127.0.0.1:9333'])
  assert.equal(engine.commandOptions.env.WHB_TRACE_RESOURCES, '1')
  const daemon = sandbox.calls.find((c) => c.role === 'daemon')
  const cli = sandbox.calls.find((c) => c.role === 'cli')
  assert.equal(daemon.commandOptions.env.AGENT_BROWSER_DAEMON, '1')
  assert.ok(!('AGENT_BROWSER_DAEMON' in cli.commandOptions.env))
  assert.equal(cli.commandOptions.env.AGENT_BROWSER_SESSION, browser.session)
  assert.equal(cli.commandOptions.env.AGENT_BROWSER_NAMESPACE, '')
  assert.ok(cli.commandOptions.env.HOME.startsWith('/workspace/.browser-sessions/'))
  assert.equal(cli.commandOptions.cwd, cli.commandOptions.env.HOME)
})

test('returns separate output, status, duration and truncation flags', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = () => ({ stdout: 'tree', stderr: 'warning', truncated: true })
  const result = await browser.run(['snapshot', '-i'], { outputBytes: 100 })
  assert.equal(result.ok, true)
  assert.equal(result.stdout, 'tree')
  assert.equal(result.stderr, 'warning')
  assert.equal(result.exitCode, 0)
  assert.equal(result.reason, 'exited')
  assert.equal(result.timedOut, false)
  assert.equal(result.aborted, false)
  assert.equal(result.stdoutTruncated, true)
  assert.equal(result.stderrTruncated, false)
  assert.ok(result.durationMs >= 0)
  assert.equal(sandbox.calls.at(-1).spawnOptions.outputBytes, 100)
  assert.equal(sandbox.calls.at(-1).spawnOptions.stdout, 'capture')
})

test('nonzero exits are data, not exceptions', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = () => ({ exitCode: 2, stdout: 'partial', stderr: 'unknown command' })
  const result = await browser.run(['not-a-command'])
  assert.equal(result.ok, false)
  assert.equal(result.exitCode, 2)
  assert.equal(result.stdout, 'partial')
  assert.equal(result.stderr, 'unknown command')
  assert.equal(browser.state, 'ready')
})

test('timeout is distinguished from a nonzero exit and keeps partial output', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = () => ({ pending: true, timeout: true })
  const result = await browser.run(['wait', '10000'], { timeoutMs: 5 })
  assert.equal(result.reason, 'timeout')
  assert.equal(result.timedOut, true)
  assert.equal(result.ok, false)
  assert.equal(result.stderr, 'partial error')
})

test('passes argv literally without a shell and snapshots queued arguments', async (t) => {
  const { browser, sandbox } = await ready(t)
  const argv = ['eval', '"a b"; // $(touch /tmp/nope)']
  const execution = browser.run(argv)
  argv[1] = 'changed'
  await execution
  assert.deepEqual(sandbox.calls.at(-1).argv.slice(2), ['eval', '"a b"; // $(touch /tmp/nope)'])
})

test('stdin is written and closed; byte input is copied', async (t) => {
  const { browser, sandbox } = await ready(t)
  const bytes = new Uint8Array([1, 2, 3])
  const execution = browser.run(['eval', '--stdin'], { stdin: bytes })
  bytes[0] = 9
  await execution
  const call = sandbox.calls.at(-1)
  assert.equal(call.spawnOptions.stdin, 'pipe')
  assert.deepEqual(call.proc.input, [new Uint8Array([1, 2, 3])])
  assert.equal(call.proc.inputClosed, true)
})

test('abort can return while the SDK stdin writer is still pending', async (t) => {
  const { browser, sandbox } = await ready(t)
  const gate = deferred()
  sandbox.cli = () => ({ pending: true, writeGate: gate })
  const controller = new AbortController()
  const execution = browser.run(['eval', '--stdin'], { stdin: '1', signal: controller.signal })
  await until(() => sandbox.calls.at(-1)?.proc?.input.length === 1)
  controller.abort()
  assert.equal((await execution).reason, 'aborted')
  gate.resolve()
})

test('timeout can return while the SDK stdin writer is still pending', async (t) => {
  const { browser, sandbox } = await ready(t)
  const gate = deferred()
  sandbox.cli = () => ({ pending: true, timeout: true, writeGate: gate })
  const execution = browser.run(['eval', '--stdin'], { stdin: '1', timeoutMs: 5 })
  assert.equal((await execution).timedOut, true)
  gate.resolve()
})

test('serializes commands and runs the next after a failed exit', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = (argv) => argv[0] === 'open' ? { pending: true } : { stdout: 'snapshot' }
  const first = browser.run(['open', 'https://example.com'])
  const next = browser.run(['snapshot', '-i'])
  await until(() => sandbox.calls.at(-1)?.argv[2] === 'open')
  assert.equal(sandbox.calls.filter((c) => c.role === 'cli').length, 2)
  sandbox.calls.at(-1).proc.finish({ exitCode: 1 })
  assert.equal((await first).ok, false)
  assert.equal((await next).stdout, 'snapshot')
})

test('abort before execution does not spawn a command', async (t) => {
  const { browser, sandbox } = await ready(t)
  const controller = new AbortController()
  controller.abort()
  const count = sandbox.calls.length
  const result = await browser.run(['snapshot'], { signal: controller.signal })
  assert.equal(result.reason, 'aborted')
  assert.equal(result.exitCode, null)
  assert.equal(sandbox.calls.length, count)
})

test('abort during execution kills only the command process', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = () => ({ pending: true })
  const controller = new AbortController()
  const execution = browser.run(['wait', '10000'], { signal: controller.signal })
  await until(() => sandbox.calls.at(-1)?.proc?.role === 'cli' && sandbox.calls.length > 3)
  const command = sandbox.calls.at(-1).proc
  controller.abort()
  const result = await execution
  assert.equal(result.aborted, true)
  assert.equal(result.reason, 'aborted')
  assert.equal(command.kills + command.terminations, 1)
  assert.ok(sandbox.processes.filter((p) => p.role !== 'cli').every((p) => !p.exited))
})

test('abort during spawn kills the late process', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.spawnGate = deferred()
  sandbox.cli = () => ({ pending: true })
  const controller = new AbortController()
  const execution = browser.run(['snapshot'], { signal: controller.signal })
  await until(() => sandbox.calls.length === 4)
  controller.abort()
  sandbox.spawnGate.resolve()
  assert.equal((await execution).reason, 'aborted')
  assert.equal(sandbox.calls.at(-1).proc.exited, true)
})

test('stop kills an active command, rejects queued commands, and is idempotent', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.files.set('/workspace/keep.txt', 'not owned by the adapter')
  sandbox.cli = () => ({ pending: true })
  const active = browser.run(['wait', '10000'])
  const queued = browser.run(['snapshot'])
  const rejected = assert.rejects(queued, code('SESSION_UNAVAILABLE'))
  await until(() => sandbox.calls.length === 4)
  const stopping = browser.stop()
  assert.equal(browser.stop(), stopping)
  assert.equal((await active).reason, 'terminated')
  await rejected
  await stopping
  assert.equal(browser.state, 'stopped')
  assert.ok(sandbox.processes.every((p) => p.exited))
  assert.equal(sandbox.closed, false)
  assert.equal(sandbox.removed.length, 1)
  assert.ok(sandbox.removed[0].endsWith(browser.session))
  assert.equal(sandbox.files.get('/workspace/keep.txt'), 'not owned by the adapter')
  await assert.rejects(browser.run(['snapshot']), code('SESSION_UNAVAILABLE'))
  await assert.rejects(browser.start(), code('SESSION_UNAVAILABLE'))
})

test('stop during command spawn also owns and kills the late process', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.spawnGate = deferred()
  sandbox.cli = () => ({ pending: true })
  const execution = browser.run(['snapshot'])
  await until(() => sandbox.calls.length === 4)
  const stopped = browser.stop()
  sandbox.spawnGate.resolve()
  assert.equal((await execution).reason, 'terminated')
  await stopped
  assert.ok(sandbox.processes.every((p) => p.exited))
})

test('stopping an idle session does not touch the sandbox', async () => {
  const sandbox = new FakeSandbox()
  const browser = new BrowserSession(sandbox)
  await browser.stop()
  assert.equal(browser.state, 'stopped')
  assert.equal(sandbox.calls.length, 0)
  assert.equal(sandbox.removed.length, 0)
})

test('stop during startup cleans a late engine spawn without starting a daemon', async () => {
  const sandbox = new FakeSandbox()
  const gate = deferred()
  const command = sandbox.command.bind(sandbox)
  sandbox.command = (...args) => {
    const built = command(...args)
    return { spawn: async (options) => {
      if (args[0] === 'engine') { sandbox.engineSpawning = true; await gate.promise }
      return built.spawn(options)
    } }
  }
  const browser = new BrowserSession(sandbox)
  const started = browser.start()
  const rejected = assert.rejects(started, code('SESSION_UNAVAILABLE'))
  await until(() => sandbox.engineSpawning)
  const stopped = browser.stop()
  gate.resolve()
  await rejected
  await stopped
  assert.equal(browser.state, 'stopped')
  assert.ok(sandbox.processes.every((p) => p.exited))
  assert.ok(sandbox.processes.every((p) => p.role !== 'daemon'))
})

test('two adapters cannot own the same port; stopping releases the lease', async () => {
  const sandbox = new FakeSandbox()
  const first = new BrowserSession(sandbox)
  const second = new BrowserSession(sandbox)
  try {
    await first.start()
    await assert.rejects(second.start(), code('PORT_OWNED'))
    await first.stop()
    await second.start()
    assert.notEqual(first.session, second.session)
  } finally {
    await first.stop()
    await second.stop()
  }
})

for (const role of ['engine', 'daemon']) {
  test(`early ${role} exit rolls back startup and releases resources`, async () => {
    const sandbox = new FakeSandbox()
    sandbox[`${role}Exit`] = true
    const browser = new BrowserSession(sandbox)
    await assert.rejects(browser.start(), code('SERVICE_EXITED'))
    assert.equal(browser.state, 'failed')
    assert.ok(sandbox.processes.every((p) => p.exited))
    assert.equal(sandbox.removed.length, 1)
    await browser.stop()
  })
}

test('an unrelated listener cannot count as the owned engine being ready', async () => {
  const sandbox = new FakeSandbox()
  sandbox.noEngineBanner = true
  const browser = new BrowserSession(sandbox, { startTimeoutMs: 20 })
  // ports.wait resolves, but the engine has not announced a successful bind.
  await assert.rejects(browser.start(), code('START_TIMEOUT'))
  assert.ok(sandbox.processes.every((p) => p.exited))
  assert.ok(sandbox.calls.every((c) => c.role === 'engine'))
  await browser.stop()
})

test('missing daemon port file times out and cleans up both services', async () => {
  const sandbox = new FakeSandbox()
  sandbox.noPortFile = true
  const browser = new BrowserSession(sandbox, { startTimeoutMs: 200 })
  await assert.rejects(browser.start(), code('START_TIMEOUT'))
  assert.deepEqual(sandbox.processes.map((p) => p.role), ['engine', 'daemon'])
  assert.ok(sandbox.processes.every((p) => p.exited))
  await browser.stop()
})

test('a readiness command failure rolls back both services', async () => {
  const sandbox = new FakeSandbox()
  sandbox.cli = () => ({ exitCode: 1, stderr: 'no CDP connection' })
  const browser = new BrowserSession(sandbox)
  await assert.rejects(browser.start(), (e) => code('START_FAILED')(e) && /no CDP connection/.test(e.message))
  assert.ok(sandbox.processes.every((p) => p.exited))
  await browser.stop()
})

test('an unexpected service exit fails the session and stops its peer', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.processes.find((p) => p.role === 'daemon').finish({ exitCode: 1 })
  await until(() => browser.state === 'failed')
  await assert.rejects(browser.run(['snapshot']), code('SESSION_UNAVAILABLE'))
  await browser.stop()
  assert.ok(sandbox.processes.every((p) => p.exited))
})

test('observer exceptions do not break startup or cleanup', async (t) => {
  const { browser } = await ready(t, { onLog: () => { throw new Error('observer failed') } })
  assert.equal(browser.state, 'ready')
  await browser.stop()
  assert.equal(browser.state, 'stopped')
})

test('confirmed process exit ends readers even when SDK pipes never send EOF', { timeout: 1000 }, async () => {
  const sandbox = new FakeSandbox()
  sandbox.noPipeEOF = true
  const browser = new BrowserSession(sandbox)
  await browser.start()
  await browser.stop()
  assert.equal(browser.state, 'stopped')
  assert.ok(sandbox.processes.every((p) => p.exited))
})

test('failed graceful termination falls back to killing the service', async (t) => {
  const { browser, sandbox } = await ready(t)
  const daemon = sandbox.processes.find((p) => p.role === 'daemon')
  daemon.terminate = async () => { throw new Error('signal failed') }
  await browser.stop()
  assert.equal(daemon.kills, 1)
  assert.equal(browser.state, 'stopped')
})

test('a cleanup failure still stops other services and does not claim success', async () => {
  const sandbox = new FakeSandbox()
  const browser = new BrowserSession(sandbox)
  await browser.start()
  const daemon = sandbox.processes.find((p) => p.role === 'daemon')
  const engine = sandbox.processes.find((p) => p.role === 'engine')
  daemon.terminate = async () => { throw new Error('signal failed') }
  const kill = daemon.kill.bind(daemon)
  daemon.kill = async () => { throw new Error('kill failed') }
  await assert.rejects(browser.stop(), AggregateError)
  assert.equal(engine.exited, true)
  assert.equal(browser.state, 'failed')
  assert.equal(sandbox.removed.length, 0, 'keep state until every owned process has stopped')
  await kill()
})

test('stdin failure kills the command instead of leaking it', async (t) => {
  const { browser, sandbox } = await ready(t)
  sandbox.cli = () => ({ pending: true, inputError: true })
  await assert.rejects(browser.run(['eval', '--stdin'], { stdin: 'x' }), /stdin failed/)
  assert.equal(sandbox.calls.at(-1).proc.exited, true)
})

for (const argv of ['open https://example.com', [], ['eval', 3], ['eval', 'a\0b'], ['--cdp=other', 'open'], ['open', '--session', 'other']]) {
  test(`rejects invalid or routing argv ${JSON.stringify(argv)}`, async (t) => {
    const { browser, sandbox } = await ready(t)
    const count = sandbox.calls.length
    await assert.rejects(browser.run(argv), code('INVALID_ARGUMENT'))
    assert.equal(sandbox.calls.length, count)
  })
}

test('validates timeouts, output limits and lifecycle before spawning', async () => {
  const sandbox = new FakeSandbox()
  assert.throws(() => new BrowserSession(sandbox, { port: 0 }), code('INVALID_ARGUMENT'))
  assert.throws(() => new BrowserSession(sandbox, { startTimeoutMs: -1 }), code('INVALID_ARGUMENT'))
  const browser = new BrowserSession(sandbox)
  await assert.rejects(browser.run(['snapshot']), code('SESSION_UNAVAILABLE'))
  await assert.rejects(browser.run(['snapshot'], { timeoutMs: 0 }), code('INVALID_ARGUMENT'))
  await assert.rejects(browser.run(['snapshot'], { outputBytes: 0 }), code('INVALID_ARGUMENT'))
  assert.equal(sandbox.calls.length, 0)
  await browser.stop()
})
