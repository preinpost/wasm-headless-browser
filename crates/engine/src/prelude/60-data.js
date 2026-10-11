// Part 6: data types the network code carries. `URL`, `URLSearchParams`,
// UTF-8, `Blob`, `File`, `FormData` and `Headers`.

// ---- URL --------------------------------------------------------------------
//
// Parsing is the WHATWG parser in Rust (`url` crate). Setters rebuild the
// string and parse it again, which is correct for http(s) and close enough
// for the rest.

const urlParts = new WeakMap();
const urlParams = new WeakMap();
const paramsOwner = new WeakMap();
const FIELDS = ['href', 'protocol', 'username', 'password', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'origin'];

function parseUrl(input, base) {
  return api.urlParts(String(input), base === undefined ? undefined : String(base));
}

class URL {
  constructor(url, base) {
    const parts = parseUrl(url, base instanceof URL ? base.href : base);
    if (!parts) throw new TypeError(`Failed to construct 'URL': Invalid URL`);
    urlParts.set(this, parts);
  }
  static canParse(url, base) {
    return !!parseUrl(url, base === undefined ? undefined : String(base));
  }
  static parse(url, base) {
    try {
      return new URL(url, base);
    } catch (e) {
      return null;
    }
  }
  static createObjectURL(blob) {
    const url = `blob:${location.origin}/${crypto.randomUUID()}`;
    blobUrls.set(url, blob);
    return url;
  }
  static revokeObjectURL(url) {
    blobUrls.delete(String(url));
  }
  get searchParams() {
    let params = urlParams.get(this);
    if (!params) {
      params = new URLSearchParams(this.search);
      paramsOwner.set(params, this);
      urlParams.set(this, params);
    }
    return params;
  }
  toString() {
    return this.href;
  }
  toJSON() {
    return this.href;
  }
}
tagClass(URL, 'URL');

function setUrlField(url, field, value) {
  const p = Object.fromEntries(FIELDS.map((f, i) => [f, urlParts.get(url)[i]]));
  value = String(value);
  if (field === 'href') {
    const parts = parseUrl(value);
    if (!parts) throw new TypeError(`Failed to set the 'href' property on 'URL': Invalid URL`);
    urlParts.set(url, parts);
  } else {
    if (field === 'protocol') p.protocol = value.endsWith(':') ? value : value + ':';
    else if (field === 'host') p.host = value;
    else if (field === 'hostname') p.host = p.port ? `${value}:${p.port}` : value;
    else if (field === 'port') p.host = value ? `${p.hostname}:${value}` : p.hostname;
    else if (field === 'pathname') p.pathname = value.startsWith('/') ? value : '/' + value;
    else if (field === 'search') p.search = value === '' || value === '?' ? '' : value.startsWith('?') ? value : '?' + value;
    else if (field === 'hash') p.hash = value === '' || value === '#' ? '' : value.startsWith('#') ? value : '#' + value;
    else if (field === 'username') p.username = value;
    else if (field === 'password') p.password = value;
    const userinfo = p.username ? p.username + (p.password ? ':' + p.password : '') + '@' : '';
    const rebuilt = parseUrl(`${p.protocol}//${userinfo}${p.host}${p.pathname}${p.search}${p.hash}`);
    if (rebuilt) urlParts.set(url, rebuilt);
  }
  // Keep `searchParams` in step without a second round trip.
  const params = urlParams.get(url);
  if (params) paramLists.set(params, parseQuery(url.search));
}

FIELDS.forEach((field, index) => {
  Object.defineProperty(URL.prototype, field, {
    get() {
      return urlParts.get(this)[index];
    },
    set: field === 'origin' ? undefined : function (value) {
      setUrlField(this, field, value);
    },
    enumerable: true,
    configurable: true,
  });
});

// ---- URLSearchParams --------------------------------------------------------

const paramLists = new WeakMap();

function decodeForm(text) {
  try {
    return decodeURIComponent(text.replace(/\+/g, ' '));
  } catch (e) {
    return text.replace(/\+/g, ' ');
  }
}

// application/x-www-form-urlencoded, which leaves `*-._` alone and encodes
// the `!'()~` that `encodeURIComponent` keeps.
function encodeForm(text) {
  return encodeURIComponent(text)
    .replace(/%20/g, '+')
    .replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase());
}

function parseQuery(query) {
  query = String(query);
  if (query.startsWith('?')) query = query.slice(1);
  return query
    .split('&')
    .filter(Boolean)
    .map((part) => {
      const eq = part.indexOf('=');
      return eq < 0 ? [decodeForm(part), ''] : [decodeForm(part.slice(0, eq)), decodeForm(part.slice(eq + 1))];
    });
}

class URLSearchParams {
  constructor(init = '') {
    let list = [];
    if (init instanceof URLSearchParams) list = paramLists.get(init).map((p) => p.slice());
    else if (init !== null && typeof init === 'object') {
      if (typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) {
          const [k, v] = Array.from(pair);
          list.push([String(k), String(v)]);
        }
      } else {
        for (const key of Object.keys(init)) list.push([key, String(init[key])]);
      }
    } else list = parseQuery(init);
    paramLists.set(this, list);
  }
  get size() {
    return paramLists.get(this).length;
  }
  append(name, value) {
    paramLists.get(this).push([String(name), String(value)]);
    this.updateOwner();
  }
  delete(name, value) {
    const list = paramLists.get(this);
    paramLists.set(this, list.filter(([k, v]) => !(k === String(name) && (value === undefined || v === String(value)))));
    this.updateOwner();
  }
  get(name) {
    const hit = paramLists.get(this).find(([k]) => k === String(name));
    return hit ? hit[1] : null;
  }
  getAll(name) {
    return paramLists.get(this).filter(([k]) => k === String(name)).map(([, v]) => v);
  }
  has(name, value) {
    return paramLists.get(this).some(([k, v]) => k === String(name) && (value === undefined || v === String(value)));
  }
  set(name, value) {
    const list = paramLists.get(this);
    const index = list.findIndex(([k]) => k === String(name));
    if (index < 0) list.push([String(name), String(value)]);
    else {
      list[index] = [String(name), String(value)];
      paramLists.set(this, list.filter(([k], i) => k !== String(name) || i === index));
    }
    this.updateOwner();
  }
  sort() {
    paramLists.get(this).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    this.updateOwner();
  }
  forEach(callback, thisArg) {
    for (const [k, v] of paramLists.get(this)) callback.call(thisArg, v, k, this);
  }
  keys() {
    return paramLists.get(this).map(([k]) => k)[Symbol.iterator]();
  }
  values() {
    return paramLists.get(this).map(([, v]) => v)[Symbol.iterator]();
  }
  entries() {
    return paramLists.get(this).map((p) => p.slice())[Symbol.iterator]();
  }
  [Symbol.iterator]() {
    return this.entries();
  }
  toString() {
    return paramLists.get(this).map(([k, v]) => `${encodeForm(k)}=${encodeForm(v)}`).join('&');
  }
  updateOwner() {
    const url = paramsOwner.get(this);
    if (!url) return;
    const query = this.toString();
    const p = urlParts.get(url);
    const base = p[0].split('#')[0].split('?')[0];
    const rebuilt = parseUrl(base + (query ? '?' + query : '') + p[9]);
    if (rebuilt) urlParts.set(url, rebuilt);
  }
}
Object.defineProperty(URLSearchParams.prototype, 'updateOwner', { enumerable: false });
tagClass(URLSearchParams, 'URLSearchParams');

// ---- UTF-8 ------------------------------------------------------------------

function utf8Encode(text) {
  text = String(text);
  const out = [];
  for (let i = 0; i < text.length; i++) {
    let c = text.charCodeAt(i);
    if (c >= 0xd800 && c < 0xdc00 && i + 1 < text.length) {
      const d = text.charCodeAt(i + 1);
      if (d >= 0xdc00 && d < 0xe000) {
        c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00);
        i++;
      } else c = 0xfffd;
    } else if (c >= 0xd800 && c < 0xe000) c = 0xfffd;
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return new Uint8Array(out);
}

// Malformed sequences become U+FFFD, as TextDecoder's default does.
function utf8Decode(bytes) {
  const codes = [];
  let i = 0;
  while (i < bytes.length) {
    const b = bytes[i++];
    if (b < 0x80) {
      codes.push(b);
      continue;
    }
    let need = 0;
    let cp = 0;
    if (b >= 0xc2 && b < 0xe0) [need, cp] = [1, b & 0x1f];
    else if (b >= 0xe0 && b < 0xf0) [need, cp] = [2, b & 0x0f];
    else if (b >= 0xf0 && b < 0xf5) [need, cp] = [3, b & 0x07];
    else {
      codes.push(0xfffd);
      continue;
    }
    let ok = true;
    for (let k = 0; k < need; k++) {
      const c = bytes[i];
      if (c === undefined || (c & 0xc0) !== 0x80) {
        ok = false;
        break;
      }
      cp = (cp << 6) | (c & 0x3f);
      i++;
    }
    const overlong = (need === 2 && cp < 0x800) || (need === 3 && cp < 0x10000);
    if (!ok || overlong || cp > 0x10ffff || (cp >= 0xd800 && cp < 0xe000)) codes.push(0xfffd);
    else codes.push(cp);
  }
  let out = '';
  for (let j = 0; j < codes.length; j += 8192) out += String.fromCodePoint.apply(null, codes.slice(j, j + 8192));
  return out;
}

function bytesOf(value) {
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  return null;
}

class TextEncoder {
  get encoding() {
    return 'utf-8';
  }
  encode(text = '') {
    return utf8Encode(text);
  }
  encodeInto(text, destination) {
    const bytes = utf8Encode(text);
    const written = Math.min(bytes.length, destination.length);
    destination.set(bytes.subarray(0, written));
    return { read: written === bytes.length ? String(text).length : utf8Decode(bytes.subarray(0, written)).length, written };
  }
}
tagClass(TextEncoder, 'TextEncoder');

// Every encoding the Encoding standard names, by any of its labels. UTF-8
// is decoded here; the rest by the engine (encoding.rs). Each `decode` call
// stands alone: with `{ stream: true }`, a character split across two calls
// is not put back together.
const decoderState = new WeakMap();
class TextDecoder {
  constructor(label = 'utf-8', options = {}) {
    const encoding = api.decoderName(String(label));
    if (encoding === undefined) {
      throw new RangeError(`Failed to construct 'TextDecoder': The encoding label provided ('${label}') is not supported.`);
    }
    decoderState.set(this, { encoding, fatal: !!options.fatal, ignoreBOM: !!options.ignoreBOM });
  }
  get encoding() {
    return decoderState.get(this).encoding;
  }
  get fatal() {
    return decoderState.get(this).fatal;
  }
  get ignoreBOM() {
    return decoderState.get(this).ignoreBOM;
  }
  decode(input) {
    if (input === undefined) return '';
    const bytes = bytesOf(input);
    if (!bytes) throw new TypeError("Failed to execute 'decode' on 'TextDecoder': parameter 1 is not of type 'ArrayBuffer'.");
    const { encoding, fatal, ignoreBOM } = decoderState.get(this);
    if (encoding === 'utf-8') {
      const text = utf8Decode(bytes);
      if (fatal && text.includes('\ufffd')) throw new TypeError('The encoded data was not valid.');
      return !ignoreBOM && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
    }
    const text = api.decodeText(Array.from(bytes), encoding, fatal, ignoreBOM);
    if (text === undefined) throw new TypeError('The encoded data was not valid.');
    return text;
  }
}
tagClass(TextDecoder, 'TextDecoder');

// ---- Blob, File, FormData ---------------------------------------------------

const blobBytes = new WeakMap();
const blobUrls = new Map();

class Blob {
  constructor(parts = [], options = {}) {
    const chunks = [];
    for (const part of parts) {
      if (part instanceof Blob) chunks.push(blobBytes.get(part));
      else chunks.push(bytesOf(part) ?? utf8Encode(String(part)));
    }
    const total = chunks.reduce((n, c) => n + c.length, 0);
    const bytes = new Uint8Array(total);
    let offset = 0;
    for (const c of chunks) {
      bytes.set(c, offset);
      offset += c.length;
    }
    blobBytes.set(this, bytes);
    this.type = String(options.type ?? '').toLowerCase();
  }
  get size() {
    return blobBytes.get(this).length;
  }
  slice(start = 0, end = this.size, type = '') {
    return new Blob([blobBytes.get(this).slice(start, end)], { type });
  }
  text() {
    return Promise.resolve(utf8Decode(blobBytes.get(this)));
  }
  arrayBuffer() {
    return Promise.resolve(blobBytes.get(this).slice().buffer);
  }
  bytes() {
    return Promise.resolve(blobBytes.get(this).slice());
  }
}
tagClass(Blob, 'Blob');

class File extends Blob {
  constructor(bits, name, options = {}) {
    super(bits, options);
    this.name = String(name);
    this.lastModified = options.lastModified ?? epochNow();
  }
}
tagClass(File, 'File');

const formEntries = new WeakMap();
function entryValue(value, filename) {
  if (value instanceof Blob) {
    if (value instanceof File && filename === undefined) return value;
    return new File([value], filename ?? (value instanceof File ? value.name : 'blob'), { type: value.type });
  }
  return String(value);
}

class FormData {
  constructor(form, submitter) {
    const list = form ? entryList(form, submitter ?? null) : [];
    formEntries.set(this, list);
  }
  append(name, value, filename) {
    formEntries.get(this).push([String(name), entryValue(value, filename)]);
  }
  delete(name) {
    formEntries.set(this, formEntries.get(this).filter(([k]) => k !== String(name)));
  }
  get(name) {
    const hit = formEntries.get(this).find(([k]) => k === String(name));
    return hit ? hit[1] : null;
  }
  getAll(name) {
    return formEntries.get(this).filter(([k]) => k === String(name)).map(([, v]) => v);
  }
  has(name) {
    return formEntries.get(this).some(([k]) => k === String(name));
  }
  set(name, value, filename) {
    const key = String(name);
    const entry = [key, entryValue(value, filename)];
    const list = formEntries.get(this);
    const index = list.findIndex(([k]) => k === key);
    if (index < 0) list.push(entry);
    else formEntries.set(this, list.filter(([k], i) => k !== key || i === index).map((e) => (e[0] === key ? entry : e)));
  }
  forEach(callback, thisArg) {
    for (const [k, v] of formEntries.get(this)) callback.call(thisArg, v, k, this);
  }
  keys() {
    return formEntries.get(this).map(([k]) => k)[Symbol.iterator]();
  }
  values() {
    return formEntries.get(this).map(([, v]) => v)[Symbol.iterator]();
  }
  entries() {
    return formEntries.get(this).map((e) => e.slice())[Symbol.iterator]();
  }
  [Symbol.iterator]() {
    return this.entries();
  }
}
tagClass(FormData, 'FormData');

function randomHex(bytes) {
  return Array.from(api.randomBytes(bytes), (b) => b.toString(16).padStart(2, '0')).join('');
}

// multipart/form-data. Files are sent as UTF-8 text, because request bodies
// cross into Rust as strings; binary uploads are a known gap.
function encodeMultipart(entries) {
  const boundary = '----WebKitFormBoundary' + randomHex(8);
  const quote = (s) => String(s).replace(/\r/g, '%0D').replace(/\n/g, '%0A').replace(/"/g, '%22');
  let text = '';
  for (const [name, value] of entries) {
    text += `--${boundary}\r\nContent-Disposition: form-data; name="${quote(name)}"`;
    if (value instanceof Blob) {
      text += `; filename="${quote(value.name ?? 'blob')}"\r\nContent-Type: ${value.type || 'application/octet-stream'}\r\n\r\n`;
      text += utf8Decode(blobBytes.get(value)) + '\r\n';
    } else {
      text += `\r\n\r\n${value}\r\n`;
    }
  }
  text += `--${boundary}--\r\n`;
  return { text, contentType: `multipart/form-data; boundary=${boundary}` };
}

// ---- Headers ----------------------------------------------------------------

const headerLists = new WeakMap();
class Headers {
  constructor(init) {
    headerLists.set(this, []);
    if (init instanceof Headers) for (const [k, v] of headerLists.get(init)) this.append(k, v);
    else if (Array.isArray(init) || (init && typeof init[Symbol.iterator] === 'function')) {
      for (const pair of init) this.append(pair[0], pair[1]);
    } else if (init && typeof init === 'object') {
      for (const key of Object.keys(init)) this.append(key, init[key]);
    }
  }
  append(name, value) {
    headerLists.get(this).push([String(name).toLowerCase(), String(value).trim()]);
  }
  delete(name) {
    headerLists.set(this, headerLists.get(this).filter(([k]) => k !== String(name).toLowerCase()));
  }
  get(name) {
    const values = headerLists.get(this).filter(([k]) => k === String(name).toLowerCase()).map(([, v]) => v);
    return values.length ? values.join(', ') : null;
  }
  getSetCookie() {
    return headerLists.get(this).filter(([k]) => k === 'set-cookie').map(([, v]) => v);
  }
  has(name) {
    return headerLists.get(this).some(([k]) => k === String(name).toLowerCase());
  }
  set(name, value) {
    this.delete(name);
    this.append(name, value);
  }
  // Sorted and combined, as iteration is specified.
  entries() {
    const names = Array.from(new Set(headerLists.get(this).map(([k]) => k))).sort();
    return names.map((k) => [k, this.get(k)])[Symbol.iterator]();
  }
  keys() {
    return Array.from(this.entries(), ([k]) => k)[Symbol.iterator]();
  }
  values() {
    return Array.from(this.entries(), ([, v]) => v)[Symbol.iterator]();
  }
  forEach(callback, thisArg) {
    for (const [k, v] of this.entries()) callback.call(thisArg, v, k, this);
  }
  [Symbol.iterator]() {
    return this.entries();
  }
}
tagClass(Headers, 'Headers');

// [name, value, name, value, ...], the shape the `http` primitive takes.
function flatHeaders(headers) {
  const out = [];
  for (const [k, v] of headerLists.get(headers)) out.push(k, v);
  return out;
}
