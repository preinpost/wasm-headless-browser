// A browser-specific adapter for a caller-owned @wasmer/sdk sandbox.
// No SDK import, shell parsing, or pi dependency: the sandbox is the seam.

const leases = new WeakMap()
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

export class BrowserSessionError extends Error {
  constructor(code, message, cause) {
    super(message, cause === undefined ? undefined : { cause })
    this.name = 'BrowserSessionError'
    this.code = code
  }
}

function integer(name, value, min, max = 0x7fffffff) {
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new BrowserSessionError('INVALID_ARGUMENT', `${name} must be an integer from ${min} to ${max}`)
  }
  return value
}

function argsArray(argv) {
  if (!Array.isArray(argv) || !argv.length || argv.some((arg) => typeof arg !== 'string' || arg.includes('\0'))) {
    throw new BrowserSessionError('INVALID_ARGUMENT', 'argv must be a nonempty array of strings without NUL bytes')
  }
  // These route commands away from the engine/session owned by this adapter.
  for (const arg of argv) {
    if (/^--(cdp|session|namespace)(=|$)/.test(arg)) {
      throw new BrowserSessionError('INVALID_ARGUMENT', 'CDP, session and namespace are managed by BrowserSession')
    }
  }
  return [...argv]
}

function result(output, aborted, durationMs) {
  const reason = aborted ? 'aborted' : output.reason
  return {
    ok: reason === 'exited' && output.exitCode === 0,
    stdout: output.stdout?.text() ?? '',
    stderr: output.stderr?.text() ?? '',
    exitCode: output.exitCode,
    reason,
    timedOut: reason === 'timeout',
    aborted,
    stdoutTruncated: output.stdout?.truncated ?? false,
    stderrTruncated: output.stderr?.truncated ?? false,
    durationMs,
  }
}

function abortedResult() {
  return result({ exitCode: null, reason: 'aborted' }, true, 0)
}

/**
 * Owns one engine, one agent-browser daemon and its command processes, but
 * never the supplied sandbox. Commands serialize to preserve tab/ref order.
 * A stopped/failed session is terminal; create a new instance to start again.
 */
export class BrowserSession {
  #sandbox
  #options
  #state = 'idle'
  #id = `browser-${crypto.randomUUID()}`
  #root
  #ownsDirectory = false
  #leased = false
  #services = []
  #active = new Set()
  #closing = new AbortController()
  #startPromise
  #stopPromise
  #cleanupPromise
  #queue = Promise.resolve()
  #failure
  #diagnostics = ''

  constructor(sandbox, {
    port = 9222,
    engine = 'engine',
    agentBrowser = 'agent-browser',
    engineKind = 'html',
    startTimeoutMs = 30_000,
    commandTimeoutMs = 90_000,
    outputBytes = 256 * 1024,
    onLog = () => {},
    traceResources = false,
  } = {}) {
    if (typeof sandbox?.command !== 'function' || typeof sandbox?.ports?.wait !== 'function' ||
        ['mkdir', 'readText', 'remove'].some((name) => typeof sandbox?.fs?.[name] !== 'function')) {
      throw new BrowserSessionError('INVALID_ARGUMENT', 'a sandbox with command, fs and ports.wait is required')
    }
    integer('port', port, 1, 65535)
    integer('startTimeoutMs', startTimeoutMs, 1)
    integer('commandTimeoutMs', commandTimeoutMs, 1)
    integer('outputBytes', outputBytes, 1)
    if (typeof onLog !== 'function' || !['html', 'fake'].includes(engineKind)) {
      throw new BrowserSessionError('INVALID_ARGUMENT', 'invalid onLog or engineKind')
    }
    this.#sandbox = sandbox
    this.#options = { port, engine, agentBrowser, engineKind, startTimeoutMs, commandTimeoutMs, outputBytes, onLog, traceResources }
    this.#root = `/workspace/.browser-sessions/${this.#id}`
  }

  get state() { return this.#state }
  get session() { return this.#id }
  get cdpUrl() { return `http://127.0.0.1:${this.#options.port}` }

  start() {
    if (this.#state === 'starting') return this.#startPromise
    if (this.#state === 'ready') return Promise.resolve(this)
    if (this.#state !== 'idle') return Promise.reject(this.#unavailable())
    const ports = leases.get(this.#sandbox) ?? new Map()
    if (ports.has(this.#options.port)) {
      return Promise.reject(new BrowserSessionError('PORT_OWNED', 'another BrowserSession owns this sandbox port'))
    }
    ports.set(this.#options.port, this)
    leases.set(this.#sandbox, ports)
    this.#leased = true
    this.#state = 'starting'
    this.#startPromise = this.#start()
    return this.#startPromise
  }

  async #start() {
    const deadline = performance.now() + this.#options.startTimeoutMs
    const remaining = () => {
      this.#assertAlive('starting')
      const left = Math.ceil(deadline - performance.now())
      if (left <= 0) throw new BrowserSessionError('START_TIMEOUT', 'browser startup timed out')
      return left
    }
    try {
      await this.#sandbox.fs.mkdir('/workspace/.browser-sessions', { recursive: true })
      remaining()
      await this.#sandbox.fs.mkdir(this.#root)
      this.#ownsDirectory = true
      remaining()
      await this.#sandbox.fs.mkdir(`${this.#root}/socket`)
      remaining()
      const env = this.#options.traceResources ? { WHB_TRACE_RESOURCES: '1' } : {}
      const engine = await this.#spawnService('engine', this.#options.engine,
        ['--engine', this.#options.engineKind, `127.0.0.1:${this.#options.port}`], env)
      // The binary prints discovery only after its own bind succeeded. A
      // listener belonging to unrelated code in this sandbox is not readiness.
      while (!engine.bound) await sleep(Math.min(remaining(), 50))
      // Short waits let stop interrupt startup without leaving a long SDK
      // listener wait behind. Binding is necessary, but not sufficient.
      while (true) {
        const left = remaining()
        try {
          await this.#sandbox.ports.wait(this.#options.port, { timeoutMs: Math.min(left, 100) })
          remaining()
          break
        } catch (error) {
          if (error.code !== 'TIMEOUT') throw error
          await sleep(Math.min(remaining(), 10))
        }
      }
      remaining()
      await this.#spawnService('daemon', this.#options.agentBrowser, [], {
        ...this.#env(), AGENT_BROWSER_DAEMON: '1',
      })
      const portFile = `${this.#root}/socket/${this.#id}.port`
      while (true) {
        const left = remaining()
        const port = await this.#sandbox.fs.readText(portFile).catch((error) => {
          if (error.code !== 'FILESYSTEM_ERROR') throw error
          return ''
        })
        remaining()
        if (port.trim()) {
          integer('daemon port', Number(port.trim()), 1, 65535)
          break
        }
        await sleep(Math.min(left, 50))
      }
      // A port file alone may be stale or precede readiness. Prove that the
      // daemon can answer a command against this engine before returning.
      const ready = await this.#execute(['get', 'url'], { timeoutMs: remaining(), outputBytes: this.#options.outputBytes }, 'starting')
      remaining()
      if (!ready.ok) throw new BrowserSessionError('START_FAILED', `daemon readiness failed: ${ready.stderr || ready.stdout || ready.reason}`)
      this.#state = 'ready'
      return this
    } catch (error) {
      if (this.#state === 'starting') this.#state = 'failed'
      this.#failure = error
      this.#closing.abort()
      try { await this.#cleanup() } catch (cleanupError) {
        throw new AggregateError([error, cleanupError], 'browser startup and cleanup failed')
      }
      throw new BrowserSessionError(error.code ?? 'START_FAILED', `${error.message}${this.#diagnostics ? `\n${this.#diagnostics}` : ''}`, error)
    }
  }

  #env() {
    return {
      HOME: this.#root,
      AGENT_BROWSER_SOCKET_DIR: `${this.#root}/socket`,
      AGENT_BROWSER_SESSION: this.#id,
      AGENT_BROWSER_NAMESPACE: '',
    }
  }

  run(argv, { timeoutMs = this.#options.commandTimeoutMs, outputBytes = this.#options.outputBytes, stdin, signal } = {}) {
    try {
      const args = argsArray(argv)
      integer('timeoutMs', timeoutMs, 1)
      integer('outputBytes', outputBytes, 1)
      if (stdin !== undefined && typeof stdin !== 'string' && !(stdin instanceof Uint8Array)) {
        throw new BrowserSessionError('INVALID_ARGUMENT', 'stdin must be a string or Uint8Array')
      }
      if (signal !== undefined && !(signal instanceof AbortSignal)) {
        throw new BrowserSessionError('INVALID_ARGUMENT', 'signal must be an AbortSignal')
      }
      this.#assertAlive('ready')
      // Snapshot byte input too: a caller may mutate it while queued.
      const input = stdin instanceof Uint8Array ? stdin.slice() : stdin
      const execution = this.#queue.then(() => signal?.aborted ? abortedResult() :
        this.#execute(args, { timeoutMs, outputBytes, stdin: input, signal }, 'ready'))
      this.#queue = execution.catch(() => {})
      return execution
    } catch (error) {
      return Promise.reject(error)
    }
  }

  async #execute(argv, { timeoutMs, outputBytes, stdin, signal }, expectedState) {
    this.#assertAlive(expectedState)
    if (signal?.aborted) return abortedResult()
    const start = performance.now()
    const proc = await this.#sandbox.command(this.#options.agentBrowser,
      ['--cdp', this.cdpUrl, ...argv], { cwd: this.#root, env: this.#env() })
      .spawn({ timeoutMs, outputBytes, stdin: stdin === undefined ? 'closed' : 'pipe', stdout: 'capture', stderr: 'capture' })
    const record = this.#track(proc)
    this.#active.add(record)
    let aborted = false
    let cancelError
    const cancel = (byCaller) => {
      if (record.exited) return
      aborted ||= byCaller
      void this.#terminate(record).catch((error) => { cancelError = error })
    }
    const onAbort = () => cancel(true)
    const onStop = () => cancel(false)
    signal?.addEventListener('abort', onAbort, { once: true })
    this.#closing.signal.addEventListener('abort', onStop, { once: true })
    // Covers stop/abort while spawn was still acquiring the process.
    if (this.#closing.signal.aborted) onStop()
    if (signal?.aborted) onAbort()
    try {
      const input = async () => {
        if (stdin === undefined || this.#closing.signal.aborted || signal?.aborted) return
        try {
          await proc.stdin.write(stdin)
          await proc.stdin.close()
        } catch (error) {
          if (!this.#closing.signal.aborted && !signal?.aborted && record.output?.reason !== 'timeout') throw error
        }
      }
      // A killed/timed-out process may exit while an SDK stdin write is
      // pending. Its output must not wait indefinitely for that writer.
      const output = await Promise.race([record.wait, input().then(() => record.wait)])
      if (cancelError) throw cancelError
      return result(output, aborted, performance.now() - start)
    } finally {
      signal?.removeEventListener('abort', onAbort)
      this.#closing.signal.removeEventListener('abort', onStop)
      // Input/SDK failures must not leave a command running unobserved.
      if (!record.exited) await this.#terminate(record)
      this.#active.delete(record)
    }
  }

  #track(proc) {
    const record = { proc, exited: false, output: null, drains: [] }
    record.wait = proc.wait({ check: false }).then((output) => {
      record.exited = true
      record.output = output
      return output
    }, (error) => {
      record.error = error
      throw error
    })
    // A service may exit before readiness or cleanup starts awaiting it.
    void record.wait.catch(() => {})
    return record
  }

  async #spawnService(name, selector, argv, env) {
    const proc = await this.#sandbox.command(selector, argv, { cwd: this.#root, env })
      .spawn({ stdout: 'pipe', stderr: 'pipe', outputBytes: this.#options.outputBytes })
    const record = this.#track(proc)
    record.name = name
    this.#services.push(record)
    for (const stream of ['stdout', 'stderr']) {
      record.drains.push((async () => {
        const lines = proc[stream].lines()
        const ended = record.wait.then(() => ({ done: true }), () => ({ done: true }))
        while (!record.exited && !record.error) {
          // SDK 0.19.0 can leave a pipe read pending after process exit.
          // Process lifetime, not an absent EOF, bounds this observer.
          const { value: line, done } = await Promise.race([lines.next(), ended])
          if (done) break
          if (name === 'engine' && stream === 'stdout' && /\bdiscovery\s*:/.test(line)) record.bound = true
          this.#diagnostics = (this.#diagnostics + `[${name} ${stream}] ${line}\n`).slice(-8192)
          try { this.#options.onLog({ process: name, stream, line }) } catch { /* Observers cannot own lifecycle. */ }
        }
      })().catch(() => {}))
    }
    void record.wait.then(() => this.#serviceEnded(record), () => this.#serviceEnded(record))
    return record
  }

  #serviceEnded(record) {
    if (this.#state !== 'ready') return
    this.#failure = new BrowserSessionError('SERVICE_EXITED', `${record.name} exited unexpectedly`)
    this.#state = 'failed'
    this.#closing.abort()
    void this.#cleanup().catch(() => {})
  }

  #unavailable() {
    return new BrowserSessionError('SESSION_UNAVAILABLE', `browser session is ${this.#state}`, this.#failure)
  }

  #assertAlive(expectedState) {
    if (this.#state !== expectedState || this.#closing.signal.aborted) throw this.#unavailable()
    const ended = this.#services.find((service) => service.exited || service.error)
    if (ended) throw new BrowserSessionError('SERVICE_EXITED', `${ended.name} stopped`, ended.error)
  }

  stop() {
    if (this.#stopPromise) return this.#stopPromise
    this.#state = 'stopping'
    this.#closing.abort()
    this.#stopPromise = (async () => {
      // A late spawn must register its handle before cleanup takes ownership.
      await this.#startPromise?.catch(() => {})
      try {
        await this.#cleanup()
        this.#state = 'stopped'
      } catch (error) {
        this.#state = 'failed'
        this.#failure = error
        throw error
      }
    })()
    return this.#stopPromise
  }

  #terminate(record) {
    if (record.exited) return Promise.resolve()
    // stop and an AbortSignal may race; send only one termination request.
    record.termination ??= record.proc.terminate({ gracePeriodMs: 250 }).catch(() => record.proc.kill())
    return record.termination
  }

  #cleanup() {
    if (this.#cleanupPromise) return this.#cleanupPromise
    this.#cleanupPromise = (async () => {
      const errors = []
      for (const command of this.#active) {
        try { await this.#terminate(command) } catch (error) { errors.push(error) }
      }
      // Includes queued commands and any spawn still pending when stop began.
      await this.#queue
      for (const service of [...this.#services].reverse()) {
        try {
          await this.#terminate(service)
          await service.wait
          await Promise.all(service.drains)
        } catch (error) { errors.push(error) }
      }
      if (errors.length) throw new AggregateError(errors, 'browser processes could not be stopped')
      if (this.#ownsDirectory) {
        await this.#sandbox.fs.remove(this.#root, { recursive: true })
        this.#ownsDirectory = false
      }
      if (this.#leased) {
        leases.get(this.#sandbox)?.delete(this.#options.port)
        this.#leased = false
      }
      // Do not retain SDK process handles after confirmed exit/cleanup.
      this.#services = []
      this.#active.clear()
    })()
    return this.#cleanupPromise
  }
}
