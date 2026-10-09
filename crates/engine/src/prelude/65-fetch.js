// Part 6b: requests. `fetch`, `XMLHttpRequest`, `AbortController`, and the
// connections the engine cannot make yet, which fail rather than hang.
//
// Every request goes through the engine's client, so it carries the page's
// cookies and stores the ones it gets back. `fetch` and async XHR start the
// request at once on a thread of the engine's and collect the response in the
// task that delivers it, so the download overlaps the script that runs
// meanwhile, and events still come in a fixed order. Two known gaps: bodies cross into Rust as UTF-8 strings, so binary
// uploads and downloads are mangled; and cross-origin requests are not
// checked against CORS.

function parseRawHeaders(raw) {
  const out = [];
  for (const line of String(raw).split('\r\n')) {
    const colon = line.indexOf(':');
    if (colon > 0) out.push([line.slice(0, colon).trim().toLowerCase(), line.slice(colon + 1).trim()]);
  }
  return out;
}

// One request through the engine. Status 0 means it failed, and
// `statusText` says why. `kind` is the CDP resource type it is reported as.
function performRequest(method, url, headers, body, kind = 'Other') {
  if (String(url).startsWith('blob:')) {
    const blob = blobUrls.get(String(url));
    if (!blob) return { status: 0, statusText: 'unknown blob URL', url, headers: [], body: '' };
    return { status: 200, statusText: 'OK', url, headers: [['content-type', blob.type]], body: utf8Decode(blobBytes.get(blob)) };
  }
  return responseFrom(api.http(String(method), String(url), headers, body ?? '', kind));
}

// [`performRequest`], started now. Returns a function that waits for the
// response, for the task that uses it; `finish.cancel()` gives up on it.
function startRequest(method, url, headers, body, kind = 'Other') {
  if (String(url).startsWith('blob:')) {
    const response = performRequest(method, url, headers, body, kind);
    return Object.assign(() => response, { cancel() {} });
  }
  const handle = api.httpStart(String(method), String(url), headers, body ?? '', kind);
  return Object.assign(() => responseFrom(api.httpFinish(handle)), { cancel: () => api.httpDrop(handle) });
}

function responseFrom(raw) {
  return { status: Number(raw[0]), statusText: raw[1], url: raw[2], headers: parseRawHeaders(raw[3]), body: raw[4] };
}

// A request body as text, with the content type it implies.
function encodeBody(body) {
  if (body === null || body === undefined) return { text: '', contentType: null };
  if (typeof body === 'string') return { text: body, contentType: 'text/plain;charset=UTF-8' };
  if (body instanceof URLSearchParams) return { text: body.toString(), contentType: 'application/x-www-form-urlencoded;charset=UTF-8' };
  if (body instanceof FormData) return encodeMultipart(formEntries.get(body));
  if (body instanceof Blob) return { text: utf8Decode(blobBytes.get(body)), contentType: body.type || null };
  const bytes = bytesOf(body);
  if (bytes) return { text: utf8Decode(bytes), contentType: null };
  if (nodeIds.has(body)) return { text: api.outerHTML(own(body)), contentType: 'text/html;charset=UTF-8' };
  return { text: String(body), contentType: 'text/plain;charset=UTF-8' };
}

// ---- Response and Request ---------------------------------------------------

const bodies = new WeakMap();

function consume(owner, convert) {
  const state = bodies.get(owner);
  if (state.used) return Promise.reject(new TypeError('Failed to execute: body stream already read'));
  state.used = true;
  try {
    return Promise.resolve(convert(state.text));
  } catch (e) {
    return Promise.reject(e);
  }
}

const BodyMethods = {
  get bodyUsed() {
    return bodies.get(this).used;
  },
  get body() {
    return null;
  },
  text() {
    return consume(this, (t) => t);
  },
  json() {
    return consume(this, (t) => JSON.parse(t));
  },
  arrayBuffer() {
    return consume(this, (t) => utf8Encode(t).buffer);
  },
  bytes() {
    return consume(this, (t) => utf8Encode(t));
  },
  blob() {
    return consume(this, (t) => new Blob([t], { type: this.headers.get('content-type') ?? '' }));
  },
  formData() {
    return consume(this, (t) => {
      const data = new FormData();
      for (const [k, v] of parseQuery(t)) data.append(k, v);
      return data;
    });
  },
};

class Response {
  constructor(body = null, init = {}) {
    const { text, contentType } = encodeBody(body);
    bodies.set(this, { text, used: false });
    this.status = init.status ?? 200;
    this.statusText = init.statusText ?? '';
    this.headers = new Headers(init.headers);
    if (contentType && !this.headers.has('content-type')) this.headers.set('content-type', contentType);
    this.type = 'default';
    this.url = '';
    this.redirected = false;
  }
  get ok() {
    return this.status >= 200 && this.status < 300;
  }
  clone() {
    const copy = new Response(bodies.get(this).text, this);
    copy.url = this.url;
    copy.redirected = this.redirected;
    copy.type = this.type;
    return copy;
  }
  static json(data, init = {}) {
    const headers = new Headers(init.headers);
    if (!headers.has('content-type')) headers.set('content-type', 'application/json');
    return new Response(JSON.stringify(data), { ...init, headers });
  }
  static error() {
    const r = new Response(null, { status: 0 });
    r.type = 'error';
    return r;
  }
  static redirect(url, status = 302) {
    return new Response(null, { status, headers: { location: String(new URL(url, api.url())) } });
  }
}
mixin(Response, BodyMethods);
tagClass(Response, 'Response');

class Request {
  constructor(input, init = {}) {
    const from = input instanceof Request ? input : null;
    const url = from ? from.url : resolveUrl(input);
    if (!url) throw new TypeError(`Failed to construct 'Request': Invalid URL '${input}'`);
    this.url = url;
    this.method = String(init.method ?? from?.method ?? 'GET').toUpperCase();
    this.headers = new Headers(init.headers ?? from?.headers);
    this.credentials = init.credentials ?? from?.credentials ?? 'same-origin';
    this.mode = init.mode ?? from?.mode ?? 'cors';
    this.cache = init.cache ?? 'default';
    this.redirect = init.redirect ?? 'follow';
    this.referrer = init.referrer ?? 'about:client';
    this.signal = init.signal ?? from?.signal ?? null;
    let text = from ? bodies.get(from).text : '';
    if (init.body !== undefined && init.body !== null) {
      if (this.method === 'GET' || this.method === 'HEAD') {
        throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      }
      const encoded = encodeBody(init.body);
      text = encoded.text;
      if (encoded.contentType && !this.headers.has('content-type')) this.headers.set('content-type', encoded.contentType);
    }
    bodies.set(this, { text, used: false });
  }
  clone() {
    return new Request(this);
  }
}
mixin(Request, BodyMethods);
tagClass(Request, 'Request');

const abortError = (signal) => signal.reason ?? domError('AbortError', 'The operation was aborted.');

function fetch(input, init) {
  return new Promise((resolve, reject) => {
    let request;
    try {
      request = new Request(input, init);
    } catch (e) {
      reject(e);
      return;
    }
    const signal = request.signal;
    if (signal && signal.aborted) {
      reject(abortError(signal));
      return;
    }
    const finish = startRequest(request.method, request.url, flatHeaders(request.headers), bodies.get(request).text, 'Fetch');
    const handle = schedule(() => {
      const r = finish();
      if (r.status === 0) {
        reject(new TypeError(`Failed to fetch: ${r.statusText}`));
        return;
      }
      const response = new Response(r.body, { status: r.status, statusText: r.statusText, headers: r.headers });
      response.url = r.url;
      response.redirected = r.url !== request.url;
      response.type = 'basic';
      resolve(response);
    }, 0);
    if (signal) {
      signal.addEventListener('abort', () => {
        cancel(handle);
        finish.cancel();
        reject(abortError(signal));
      }, { once: true });
    }
  });
}

// ---- AbortController --------------------------------------------------------

const abortState = new WeakMap();
class AbortSignal extends EventTarget {
  constructor() {
    illegalConstructor();
  }
  get aborted() {
    return abortState.get(this).aborted;
  }
  get reason() {
    return abortState.get(this).reason;
  }
  throwIfAborted() {
    if (this.aborted) throw this.reason;
  }
  static abort(reason) {
    const signal = newSignal();
    signalAbort(signal, reason);
    return signal;
  }
  static timeout(ms) {
    const signal = newSignal();
    schedule(() => signalAbort(signal, domError('TimeoutError', 'signal timed out')), ms);
    return signal;
  }
  static any(signals) {
    const signal = newSignal();
    for (const s of signals) {
      if (s.aborted) {
        signalAbort(signal, s.reason);
        break;
      }
      s.addEventListener('abort', () => signalAbort(signal, s.reason), { once: true });
    }
    return signal;
  }
}
tagClass(AbortSignal, 'AbortSignal');

function newSignal() {
  const signal = Object.create(AbortSignal.prototype);
  abortState.set(signal, { aborted: false, reason: undefined });
  return signal;
}

function signalAbort(signal, reason) {
  const state = abortState.get(signal);
  if (state.aborted) return;
  state.aborted = true;
  state.reason = reason === undefined ? domError('AbortError', 'signal is aborted without reason') : reason;
  fire(signal, 'abort');
}

class AbortController {
  constructor() {
    this.signal = newSignal();
  }
  abort(reason) {
    signalAbort(this.signal, reason);
  }
}
tagClass(AbortController, 'AbortController');

// ---- XMLHttpRequest ---------------------------------------------------------

class XMLHttpRequestEventTarget extends EventTarget {}
tagClass(XMLHttpRequestEventTarget, 'XMLHttpRequestEventTarget');
class XMLHttpRequestUpload extends XMLHttpRequestEventTarget {}
tagClass(XMLHttpRequestUpload, 'XMLHttpRequestUpload');

const xhrState = new WeakMap();
const XHR_STATES = { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 };

class XMLHttpRequest extends XMLHttpRequestEventTarget {
  constructor() {
    super();
    xhrState.set(this, { readyState: 0, method: 'GET', url: '', async: true, headers: [], response: null, sent: false, handle: 0, finish: null });
    this.upload = new XMLHttpRequestUpload();
    this.responseType = '';
    this.timeout = 0;
    this.withCredentials = false;
  }
  get readyState() {
    return xhrState.get(this).readyState;
  }
  get status() {
    return xhrState.get(this).response?.status ?? 0;
  }
  get statusText() {
    return xhrState.get(this).response?.statusText ?? '';
  }
  get responseURL() {
    return xhrState.get(this).response?.url ?? '';
  }
  get responseText() {
    if (this.responseType !== '' && this.responseType !== 'text') {
      throw domError('InvalidStateError', `The value is only accessible if the object's 'responseType' is '' or 'text' (was '${this.responseType}').`);
    }
    const s = xhrState.get(this);
    return s.readyState >= 3 && s.response ? s.response.body : '';
  }
  get response() {
    const s = xhrState.get(this);
    const text = this.responseType === '' || this.responseType === 'text';
    if (s.readyState !== 4 || !s.response) return text ? this.responseText : null;
    const body = s.response.body;
    switch (this.responseType) {
      case 'json':
        try {
          return JSON.parse(body);
        } catch (e) {
          return null;
        }
      case 'arraybuffer':
        return utf8Encode(body).buffer;
      case 'blob':
        return new Blob([body], { type: this.getResponseHeader('content-type') ?? '' });
      case 'document':
        return null;
      default:
        return body;
    }
  }
  get responseXML() {
    return null;
  }
  open(method, url, async = true) {
    const s = xhrState.get(this);
    if (s.handle) cancel(s.handle);
    const resolved = resolveUrl(url);
    if (!resolved) throw domError('SyntaxError', `Failed to execute 'open' on 'XMLHttpRequest': Invalid URL`);
    Object.assign(s, { method: String(method).toUpperCase(), url: resolved, async: async !== false, headers: [], response: null, sent: false, handle: 0, readyState: 1 });
    fire(this, 'readystatechange');
  }
  setRequestHeader(name, value) {
    const s = xhrState.get(this);
    if (s.readyState !== 1 || s.sent) throw domError('InvalidStateError', "Failed to execute 'setRequestHeader' on 'XMLHttpRequest': The object's state must be OPENED.");
    s.headers.push(String(name), String(value));
  }
  getResponseHeader(name) {
    const s = xhrState.get(this);
    if (!s.response || s.readyState < 2) return null;
    const values = s.response.headers.filter(([k]) => k === String(name).toLowerCase()).map(([, v]) => v);
    return values.length ? values.join(', ') : null;
  }
  getAllResponseHeaders() {
    const s = xhrState.get(this);
    if (!s.response || s.readyState < 2) return '';
    return s.response.headers.filter(([k]) => k !== 'set-cookie').map(([k, v]) => `${k}: ${v}\r\n`).join('');
  }
  overrideMimeType() {}
  send(body = null) {
    const s = xhrState.get(this);
    if (s.readyState !== 1 || s.sent) throw domError('InvalidStateError', "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.");
    s.sent = true;
    const hasBody = s.method !== 'GET' && s.method !== 'HEAD';
    const { text, contentType } = hasBody ? encodeBody(body) : { text: '', contentType: null };
    if (contentType && !s.headers.some((h, i) => i % 2 === 0 && h.toLowerCase() === 'content-type')) s.headers.push('Content-Type', contentType);
    if (!s.async) {
      const r = performRequest(s.method, s.url, s.headers, text, 'XHR');
      finishXhr(this, r);
      if (r.status === 0) throw domError('NetworkError', `Failed to execute 'send' on 'XMLHttpRequest': Failed to load '${s.url}'.`);
      return;
    }
    const finish = startRequest(s.method, s.url, s.headers, text, 'XHR');
    s.finish = finish;
    dispatch(this, new ProgressEvent('loadstart'));
    s.handle = schedule(() => {
      s.handle = 0;
      s.finish = null;
      finishXhr(this, finish());
    }, 0);
  }
  abort() {
    const s = xhrState.get(this);
    if (s.handle) {
      cancel(s.handle);
      s.handle = 0;
      s.finish?.cancel();
      s.finish = null;
    }
    if (s.sent && s.readyState !== 4) {
      s.readyState = 4;
      s.sent = false;
      fire(this, 'readystatechange');
      dispatch(this, new ProgressEvent('abort'));
      dispatch(this, new ProgressEvent('loadend'));
    }
    s.readyState = 0;
  }
}
tagClass(XMLHttpRequest, 'XMLHttpRequest');
for (const [name, value] of Object.entries(XHR_STATES)) {
  Object.defineProperty(XMLHttpRequest, name, { value });
  Object.defineProperty(XMLHttpRequest.prototype, name, { value });
}

function finishXhr(xhr, r) {
  const s = xhrState.get(xhr);
  if (r.status === 0) {
    s.readyState = 4;
    fire(xhr, 'readystatechange');
    dispatch(xhr, new ProgressEvent('error'));
    dispatch(xhr, new ProgressEvent('loadend'));
    return;
  }
  s.response = r;
  const size = { lengthComputable: true, loaded: r.body.length, total: r.body.length };
  s.readyState = 2;
  fire(xhr, 'readystatechange');
  s.readyState = 3;
  fire(xhr, 'readystatechange');
  dispatch(xhr, new ProgressEvent('progress', size));
  s.readyState = 4;
  fire(xhr, 'readystatechange');
  dispatch(xhr, new ProgressEvent('load', size));
  dispatch(xhr, new ProgressEvent('loadend', size));
}

// ---- connections the engine cannot make yet ----------------------------------
//
// There is no WebSocket client and no worker runtime. Both fail the way a
// refused connection fails, asynchronously, so a page's retry logic sees an
// error instead of waiting forever.

class WebSocket extends EventTarget {
  constructor(url, protocols) {
    super();
    this.url = String(url);
    this.readyState = 0;
    this.protocol = '';
    this.extensions = '';
    this.bufferedAmount = 0;
    this.binaryType = 'blob';
    schedule(() => {
      if (this.readyState === 3) return;
      this.readyState = 3;
      console.warn(`WebSocket connection to '${this.url}' failed: WebSocket is not implemented in this engine`);
      fire(this, 'error');
      dispatch(this, new CloseEvent('close', { code: 1006, wasClean: false }));
    }, 0);
  }
  send() {
    if (this.readyState === 0) throw domError('InvalidStateError', "Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.");
  }
  close() {
    this.readyState = 3;
  }
}
tagClass(WebSocket, 'WebSocket');
Object.entries({ CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 }).forEach(([name, value]) => {
  Object.defineProperty(WebSocket, name, { value });
  Object.defineProperty(WebSocket.prototype, name, { value });
});

class Worker extends EventTarget {
  constructor(url) {
    super();
    schedule(() => {
      console.warn(`Worker '${url}' was not started: workers are not implemented in this engine`);
      dispatch(this, new ErrorEvent('error', { message: 'workers are not implemented in this engine' }));
    }, 0);
  }
  postMessage() {}
  terminate() {}
}
tagClass(Worker, 'Worker');
