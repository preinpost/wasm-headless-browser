// Part 7: the window's own objects. `location`, `history`, `navigator`,
// storage, media queries, computed style, observers, `performance`,
// `crypto`, and a `Date` that follows the virtual clock.

// ---- navigation -------------------------------------------------------------

function runJavascriptUrl(url) {
  let source = String(url).replace(/^javascript:/i, '');
  try {
    source = decodeURIComponent(source);
  } catch (e) {
    // Leave it as written.
  }
  runClassic(source, 'javascript-url');
}

const withoutFragment = (url) => String(url).split('#')[0];

// Where every script-initiated navigation goes. A change of fragment alone
// stays in this document and fires `hashchange`; anything else asks the
// engine to navigate once the current script has finished.
function navigateTo(url) {
  const target = resolveUrl(url);
  if (!target) throw domError('SyntaxError', `'${url}' is not a valid URL.`);
  if (/^javascript:/i.test(target)) {
    runJavascriptUrl(target);
    return;
  }
  const current = api.url();
  if (target.includes('#') && withoutFragment(target) === withoutFragment(current)) {
    if (target === current) return;
    api.setUrl(target, false);
    schedule(() => dispatch(globalThis, new HashChangeEvent('hashchange', { oldURL: current, newURL: target })), 0);
    return;
  }
  api.navigate(target);
}

class Location {
  constructor() {
    illegalConstructor();
  }
  get href() {
    return api.url();
  }
  set href(value) {
    navigateTo(value);
  }
  assign(url) {
    navigateTo(url);
  }
  replace(url) {
    navigateTo(url);
  }
  reload() {
    api.reload();
  }
  toString() {
    return api.url();
  }
  get ancestorOrigins() {
    return [];
  }
}
tagClass(Location, 'Location');
for (const part of ['origin', 'protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash']) {
  Object.defineProperty(Location.prototype, part, {
    get() {
      const u = URL.parse(api.url());
      return u ? u[part] : '';
    },
    set(value) {
      if (part === 'origin') return;
      const u = URL.parse(api.url());
      if (!u) return;
      u[part] = value;
      navigateTo(u.href);
    },
    enumerable: true,
    configurable: true,
  });
}
const location = Object.create(Location.prototype);

// `pushState` and `replaceState` change the URL without loading anything. The
// engine records the new URL in its history. Going back through those
// entries reloads the page at that URL rather than firing `popstate`, which
// is a known difference from a browser.
let historyState = null;
let historyLength = 1;
class History {
  constructor() {
    illegalConstructor();
  }
  get length() {
    return historyLength;
  }
  get state() {
    return historyState;
  }
  get scrollRestoration() {
    return 'auto';
  }
  set scrollRestoration(v) {}
  back() {
    api.historyGo(-1);
  }
  forward() {
    api.historyGo(1);
  }
  go(delta = 0) {
    if (Number(delta) === 0) api.reload();
    else api.historyGo(Number(delta) | 0);
  }
  pushState(state, title, url) {
    changeHistory(state, url, false);
    historyLength++;
  }
  replaceState(state, title, url) {
    changeHistory(state, url, true);
  }
}
tagClass(History, 'History');
const history = Object.create(History.prototype);

function changeHistory(state, url, replace) {
  if (url !== undefined && url !== null) {
    const target = resolveUrl(url);
    if (!target) throw domError('SyntaxError', `Failed to execute 'pushState' on 'History': '${url}' is not a valid URL.`);
    if (URL.parse(target)?.origin !== URL.parse(api.url())?.origin) {
      throw domError('SecurityError', `Failed to execute 'pushState' on 'History': A history state object with URL '${target}' cannot be created in a document with origin '${location.origin}'.`);
    }
    api.setUrl(target, replace);
  }
  historyState = structuredClone(state);
}

// ---- navigator and screen ---------------------------------------------------

class Navigator {
  constructor() {
    illegalConstructor();
  }
}
tagClass(Navigator, 'Navigator');
const navigator = Object.assign(Object.create(Navigator.prototype), {
  userAgent: api.userAgent(),
  appName: 'Netscape',
  appVersion: api.userAgent().replace(/^Mozilla\//, ''),
  appCodeName: 'Mozilla',
  product: 'Gecko',
  productSub: '20030107',
  vendor: 'Google Inc.',
  vendorSub: '',
  platform: 'Linux x86_64',
  language: 'en-US',
  languages: Object.freeze(['en-US', 'en']),
  onLine: true,
  cookieEnabled: true,
  doNotTrack: null,
  hardwareConcurrency: 1,
  maxTouchPoints: 0,
  pdfViewerEnabled: false,
  webdriver: false,
  plugins: [],
  mimeTypes: [],
  javaEnabled: () => false,
  sendBeacon(url, data) {
    const target = resolveUrl(url);
    if (!target) return false;
    const { text, contentType } = encodeBody(data);
    schedule(() => performRequest('POST', target, contentType ? ['Content-Type', contentType] : [], text), 0);
    return true;
  },
  // Clipboard and permissions exist so feature checks pass; neither grants
  // anything.
  clipboard: {
    writeText: () => Promise.resolve(),
    readText: () => Promise.reject(domError('NotAllowedError', 'Clipboard access is not available.')),
  },
  permissions: { query: () => Promise.resolve({ state: 'prompt', onchange: null }) },
  getGamepads: () => [],
  vibrate: () => false,
  registerProtocolHandler() {},
});

const screen = {
  get width() {
    return api.viewportWidth();
  },
  get height() {
    return api.viewportHeight();
  },
  get availWidth() {
    return api.viewportWidth();
  },
  get availHeight() {
    return api.viewportHeight();
  },
  availLeft: 0,
  availTop: 0,
  colorDepth: 24,
  pixelDepth: 24,
  orientation: { type: 'landscape-primary', angle: 0, addEventListener() {}, removeEventListener() {} },
};

// ---- storage ----------------------------------------------------------------
//
// Per origin and in memory only: login tokens end up here, and nothing about
// a session should touch a disk.

const storageKinds = new WeakMap();
class Storage {
  constructor() {
    illegalConstructor();
  }
  getItem(key) {
    const hit = api.storage(storageKinds.get(this), 'get', String(key), undefined);
    return hit ? hit[0] : null;
  }
  setItem(key, value) {
    api.storage(storageKinds.get(this), 'set', String(key), String(value));
  }
  removeItem(key) {
    api.storage(storageKinds.get(this), 'remove', String(key), undefined);
  }
  clear() {
    api.storage(storageKinds.get(this), 'clear', undefined, undefined);
  }
  key(index) {
    return (api.storage(storageKinds.get(this), 'keys', undefined, undefined) || [])[index] ?? null;
  }
  get length() {
    return (api.storage(storageKinds.get(this), 'keys', undefined, undefined) || []).length;
  }
}
tagClass(Storage, 'Storage');

// `localStorage.token = 'x'` is the same as `setItem`, hence the proxy.
function makeStorage(kind) {
  const target = Object.create(Storage.prototype);
  const read = (key) => api.storage(kind, 'get', key, undefined);
  const proxy = new Proxy(target, {
    get(t, key, receiver) {
      if (typeof key !== 'string' || key in t) return Reflect.get(t, key, receiver);
      const hit = read(key);
      return hit ? hit[0] : undefined;
    },
    set(t, key, value, receiver) {
      if (typeof key !== 'string' || key in t) return Reflect.set(t, key, value, receiver);
      api.storage(kind, 'set', key, String(value));
      return true;
    },
    has: (t, key) => key in t || (typeof key === 'string' && !!read(key)),
    deleteProperty(t, key) {
      api.storage(kind, 'remove', String(key), undefined);
      return true;
    },
    ownKeys: () => api.storage(kind, 'keys', undefined, undefined) || [],
    getOwnPropertyDescriptor(t, key) {
      const hit = typeof key === 'string' ? read(key) : undefined;
      return hit ? { value: hit[0], writable: true, enumerable: true, configurable: true } : undefined;
    },
  });
  storageKinds.set(target, kind);
  storageKinds.set(proxy, kind);
  return proxy;
}
const localStorage = makeStorage('local');
const sessionStorage = makeStorage('session');

// ---- media queries and computed style ---------------------------------------

function toPx(value) {
  const n = parseFloat(value);
  return /em$/.test(value) ? n * 16 : n;
}

// Width, height, orientation and the common preferences. Anything else does
// not match, which is what a browser says about a feature it lacks.
function mediaMatches(query) {
  const width = api.viewportWidth();
  const height = api.viewportHeight();
  return String(query)
    .toLowerCase()
    .split(',')
    .some((raw) => {
      let part = raw.trim().replace(/^only\s+/, '');
      const negate = part.startsWith('not ');
      if (negate) part = part.slice(4);
      let ok = true;
      for (const piece of part.split(/\s+and\s+/)) {
        const term = piece.trim();
        if (term === 'all' || term === 'screen' || term === '') continue;
        const m = /^\(\s*([a-z-]+)\s*(?::\s*([^)]+?))?\s*\)$/.exec(term);
        if (!m) {
          ok = false;
          continue;
        }
        const [, feature, value] = m;
        switch (feature) {
          case 'min-width': ok = ok && width >= toPx(value); break;
          case 'max-width': ok = ok && width <= toPx(value); break;
          case 'width': ok = ok && width === toPx(value); break;
          case 'min-height': ok = ok && height >= toPx(value); break;
          case 'max-height': ok = ok && height <= toPx(value); break;
          case 'orientation': ok = ok && value === (width >= height ? 'landscape' : 'portrait'); break;
          case 'prefers-color-scheme': ok = ok && value === 'light'; break;
          case 'prefers-reduced-motion': ok = ok && value === 'no-preference'; break;
          case 'prefers-contrast': ok = ok && value === 'no-preference'; break;
          case 'hover': case 'any-hover': ok = ok && (value === undefined || value === 'hover'); break;
          case 'pointer': case 'any-pointer': ok = ok && (value === undefined || value === 'fine'); break;
          case 'min-resolution': ok = ok && 96 >= parseFloat(value); break;
          case '-webkit-min-device-pixel-ratio': case 'min-device-pixel-ratio': ok = ok && 1 >= parseFloat(value); break;
          default: ok = false;
        }
      }
      return negate ? !ok : ok;
    });
}

class MediaQueryList extends EventTarget {
  constructor() {
    illegalConstructor();
  }
  addListener(callback) {
    this.addEventListener('change', callback);
  }
  removeListener(callback) {
    this.removeEventListener('change', callback);
  }
}
tagClass(MediaQueryList, 'MediaQueryList');

function matchMedia(query) {
  const list = Object.create(MediaQueryList.prototype);
  Object.defineProperties(list, {
    media: { value: String(query), enumerable: true },
    matches: { get: () => mediaMatches(query), enumerable: true },
  });
  return list;
}

// The engine's cascade, one property at a time, so a page reading only
// `display` does not pay for the rest. Pseudo-elements have no computed
// style here and read as empty.
function getComputedStyle(element, pseudoElement) {
  const id = element !== null && typeof element === 'object' ? nodeIds.get(element) : undefined;
  const live = id !== undefined && isElementId(id) && !pseudoElement;
  const lookup = (name) => (live ? api.computedValue(id, String(name)) : '');
  const names = () => (live ? api.computedNames(id) : []);
  const readOnly = () => {
    throw domError('NoModificationAllowedError', 'These styles are computed, and therefore read-only.');
  };
  const decl = Object.assign(Object.create(CSSStyleDeclaration.prototype), {
    getPropertyValue: lookup,
    getPropertyPriority: () => '',
    item: (i) => names()[i] ?? '',
    setProperty: readOnly,
    removeProperty: readOnly,
  });
  Object.defineProperties(decl, {
    length: { get: () => names().length },
    cssText: { get: () => names().map((n) => `${n}: ${lookup(n)};`).join(' ') },
  });
  return new Proxy(decl, {
    get(target, key, receiver) {
      if (typeof key !== 'string' || key in target) return Reflect.get(target, key, receiver);
      if (/^\d+$/.test(key)) return target.item(Number(key));
      return lookup(kebab(key));
    },
  });
}

// ---- observers --------------------------------------------------------------

// With no layout there is nothing to intersect, so each target is reported
// once as fully visible. Content a page loads "when scrolled into view" then
// loads, which is what a reader of the page needs.
class IntersectionObserver {
  constructor(callback, options = {}) {
    this.root = options.root ?? null;
    this.rootMargin = options.rootMargin ?? '0px';
    this.thresholds = [].concat(options.threshold ?? 0);
    this.callback = callback;
    this.targets = new Set();
  }
  observe(target) {
    if (this.targets.has(target)) return;
    this.targets.add(target);
    schedule(() => {
      if (!this.targets.has(target)) return;
      const box = new DOMRect();
      this.callback([{ target, isIntersecting: true, intersectionRatio: 1, boundingClientRect: box, intersectionRect: box, rootBounds: null, time: nowMs() }], this);
    }, 0);
  }
  unobserve(target) {
    this.targets.delete(target);
  }
  disconnect() {
    this.targets.clear();
  }
  takeRecords() {
    return [];
  }
}
tagClass(IntersectionObserver, 'IntersectionObserver');

// Sizes never change without layout, so a resize observer never fires.
class ResizeObserver {
  constructor(callback) {}
  observe() {}
  unobserve() {}
  disconnect() {}
}
tagClass(ResizeObserver, 'ResizeObserver');

class PerformanceObserver {
  constructor(callback) {}
  observe() {}
  disconnect() {}
  takeRecords() {
    return [];
  }
}
PerformanceObserver.supportedEntryTypes = [];
tagClass(PerformanceObserver, 'PerformanceObserver');

// ---- performance, crypto, time ----------------------------------------------

const performanceMarks = [];
const performance = {
  now: () => nowMs(),
  timeOrigin: realStart,
  timing: { navigationStart: realStart, fetchStart: realStart, domContentLoadedEventEnd: 0, loadEventEnd: 0 },
  navigation: { type: 0, redirectCount: 0 },
  mark(name) {
    const entry = { name: String(name), entryType: 'mark', startTime: nowMs(), duration: 0 };
    performanceMarks.push(entry);
    return entry;
  },
  measure(name, start, end) {
    const at = (m) => performanceMarks.find((e) => e.name === m)?.startTime ?? 0;
    const startTime = start ? at(start) : 0;
    const entry = { name: String(name), entryType: 'measure', startTime, duration: (end ? at(end) : nowMs()) - startTime };
    performanceMarks.push(entry);
    return entry;
  },
  getEntries: () => performanceMarks.slice(),
  getEntriesByName: (name) => performanceMarks.filter((e) => e.name === name),
  getEntriesByType: (type) => performanceMarks.filter((e) => e.entryType === type),
  clearMarks() {
    performanceMarks.length = 0;
  },
  clearMeasures() {},
  clearResourceTimings() {},
  setResourceTimingBufferSize() {},
  toJSON() {
    return { timeOrigin: realStart };
  },
};

// `getRandomValues` draws from the engine's CSPRNG. `subtle` is absent: a
// half-working WebCrypto would be worse than none.
const crypto = {
  getRandomValues(array) {
    if (!ArrayBuffer.isView(array) || array instanceof Float32Array || array instanceof Float64Array) {
      throw domError('TypeMismatchError', "Failed to execute 'getRandomValues' on 'Crypto': The provided ArrayBufferView is of type 'Float', which is not an integer array type.");
    }
    if (array.byteLength > 65536) {
      throw domError('QuotaExceededError', `Failed to execute 'getRandomValues' on 'Crypto': The ArrayBufferView's byte length (${array.byteLength}) exceeds the number of bytes of entropy available via this API (65536).`);
    }
    new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(api.randomBytes(array.byteLength));
    return array;
  },
  randomUUID() {
    const b = crypto.getRandomValues(new Uint8Array(16));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
    return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
  },
};

// `Date` on the virtual clock: `new Date()` and `Date.now()` include the time
// skipped over by timers, so code that compares them with its own timeouts
// sees the time it waited for.
function VirtualDate(...args) {
  if (!new.target) return new RealDate(epochNow()).toString();
  return args.length ? new RealDate(...args) : new RealDate(epochNow());
}
VirtualDate.prototype = RealDate.prototype;
VirtualDate.now = epochNow;
VirtualDate.parse = RealDate.parse;
VirtualDate.UTC = RealDate.UTC;
Object.defineProperty(VirtualDate, 'name', { value: 'Date' });

function cloneValue(value, seen) {
  if (typeof value === 'function' || typeof value === 'symbol') {
    throw domError('DataCloneError', `${String(value)} could not be cloned.`);
  }
  if (value === null || typeof value !== 'object') return value;
  if (seen.has(value)) return seen.get(value);
  if (nodeIds.has(value)) throw domError('DataCloneError', 'A DOM node could not be cloned.');
  if (value instanceof RealDate) return new RealDate(value.getTime());
  if (value instanceof RegExp) return new RegExp(value.source, value.flags);
  if (value instanceof Blob) return value;
  if (value instanceof ArrayBuffer) return value.slice(0);
  if (ArrayBuffer.isView(value)) return new value.constructor(value);
  if (value instanceof Error) return new value.constructor(value.message);
  let out;
  if (value instanceof Map) {
    out = new Map();
    seen.set(value, out);
    for (const [k, v] of value) out.set(cloneValue(k, seen), cloneValue(v, seen));
    return out;
  }
  if (value instanceof Set) {
    out = new Set();
    seen.set(value, out);
    for (const v of value) out.add(cloneValue(v, seen));
    return out;
  }
  out = Array.isArray(value) ? [] : {};
  seen.set(value, out);
  for (const key of Object.keys(value)) out[key] = cloneValue(value[key], seen);
  return out;
}
function structuredClone(value) {
  return cloneValue(value, new Map());
}
