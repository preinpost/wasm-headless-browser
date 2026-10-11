// Part 8: timers, script execution and the page lifecycle, then the window
// itself: everything above is exported onto the global object here.

// ---- timers -----------------------------------------------------------------

function timerCallback(handler, args) {
  if (typeof handler === 'function') return () => handler(...args);
  // A string handler is compiled as a global script, as browsers still do.
  const source = String(handler);
  return () => {
    runClassic(source, 'timer');
  };
}

function setTimeout(handler, delay, ...args) {
  return schedule(timerCallback(handler, args), delay, false);
}
function setInterval(handler, delay, ...args) {
  return schedule(timerCallback(handler, args), delay, true);
}
function clearTimeout(handle) {
  cancel(handle);
}

// Frames come every 16 ms of virtual time.
function requestAnimationFrame(callback) {
  const now = nowMs();
  const nextFrame = Math.ceil((now + 1) / 16) * 16;
  return schedule(() => callback(nowMs()), nextFrame - now, false);
}

function requestIdleCallback(callback, options = {}) {
  return schedule(() => callback({ didTimeout: false, timeRemaining: () => 50 }), Math.min(Number(options.timeout) || 1, 50), false);
}

function queueMicrotask(callback) {
  if (typeof callback !== 'function') {
    throw new TypeError("Failed to execute 'queueMicrotask': parameter 1 is not of type 'Function'.");
  }
  Promise.resolve().then(() => {
    try {
      callback();
    } catch (e) {
      reportError(e);
    }
  });
}

// ---- scripts ----------------------------------------------------------------

const CLASSIC_TYPES = new Set([
  '', 'text/javascript', 'application/javascript', 'text/ecmascript', 'application/ecmascript',
  'application/x-javascript', 'application/x-ecmascript', 'text/x-javascript', 'text/x-ecmascript',
  'text/jscript', 'text/livescript', 'text/javascript1.0', 'text/javascript1.5',
]);

// 'classic', 'module', 'importmap', or null for a data block such as JSON
// or a template. A classic script marked `nomodule` is the fallback for
// browsers without modules, so it does not run here.
function scriptKind(id) {
  const type = api.attr(id, 'type');
  let kind;
  if (type === undefined) {
    const language = api.attr(id, 'language');
    kind = language === undefined || language === '' || CLASSIC_TYPES.has('text/' + language.toLowerCase()) ? 'classic' : null;
  } else {
    const t = type.trim().toLowerCase();
    kind = t === 'module' ? 'module' : t === 'importmap' ? 'importmap' : CLASSIC_TYPES.has(t) ? 'classic' : null;
  }
  if (kind === 'classic' && api.hasAttr(id, 'nomodule')) return null;
  return kind;
}

// A module script. Its imports are fetched as it is linked, then it runs,
// and `load` or `error` fires when its evaluation settles, which top-level
// `await` can put off. A module has no `document.currentScript`. Returns an
// error message for a failure known at once, or ''.
function runModuleElement(id, el) {
  const src = api.attr(id, 'src');
  let promise;
  try {
    if (src !== undefined) {
      const url = resolveUrl(src);
      if (!url) {
        schedule(() => fire(el, 'error'), 0);
        return `invalid module URL ${src}`;
      }
      promise = api.runModule(undefined, url, url);
    } else {
      const source = api.textContent(id);
      if (!source.trim()) return '';
      // Unique per script, so two inline modules are two modules; it still
      // resolves imports against the document, the fragment aside.
      const page = api.url();
      promise = api.runModule(source, `${page.split('#')[0]}#inline-module-${id}`, page);
    }
  } catch (e) {
    reportError(e);
    schedule(() => fire(el, 'error'), 0);
    return String(e && e.message !== undefined ? e.message : e);
  }
  promise.then(
    () => {
      if (src !== undefined) fire(el, 'load');
    },
    (e) => {
      reportError(e);
      fire(el, 'error');
    },
  );
  return '';
}

// Fetches and runs one script element. Returns an error message, or ''.
// `finish` collects a request already started for its `src`.
function runScriptElement(id, finish) {
  const kind = scriptKind(id);
  if (kind === null) return '';
  const el = wrap(id);
  if (kind === 'module') return runModuleElement(id, el);
  if (kind === 'importmap') {
    if (api.hasAttr(id, 'src')) {
      schedule(() => fire(el, 'error'), 0);
      return 'an import map cannot have a src';
    }
    const error = api.addImportMap(api.textContent(id));
    if (error) console.error(error);
    return error ?? '';
  }
  const src = api.attr(id, 'src');
  let source;
  let name;
  if (src !== undefined) {
    const url = resolveUrl(src);
    const response = finish ? finish() : url ? performRequest('GET', url, ['Accept', '*/*'], '', 'Script') : { status: 0, statusText: 'invalid URL' };
    if (response.status === 0 || response.status >= 400) {
      fire(el, 'error');
      return `could not load ${url ?? src}: ${response.status === 0 ? response.statusText : 'HTTP ' + response.status}`;
    }
    source = response.body;
    name = url;
  } else {
    source = api.textContent(id);
    name = `inline-script-${id}`;
    if (!source.trim()) return '';
  }
  const previous = currentScript;
  currentScript = el;
  const error = runClassic(source, name);
  currentScript = previous;
  if (src !== undefined) fire(el, 'load');
  return error;
}

// A script that became connected, or got its content or `src`, after the
// parser was done. Inline scripts run at once; external ones in a task,
// because a script inserted by script is async. The download starts now, so
// a bundler that inserts a dozen chunks gets them in parallel.
function prepareScript(id) {
  if (startedScripts.has(id) || parserScripts.has(id) || !api.isConnected(id)) return;
  const hasSrc = api.hasAttr(id, 'src');
  // An empty inline script waits for content or a src, as in a browser.
  if (!hasSrc && !api.textContent(id).trim()) return;
  startedScripts.add(id);
  if (scriptKind(id) === 'module') {
    // Inserted modules run when they are ready, inline ones included.
    schedule(() => runScriptElement(id), 0);
    return;
  }
  if (hasSrc) {
    const url = scriptKind(id) === 'classic' ? resolveUrl(api.attr(id, 'src')) : null;
    const finish = url ? startRequest('GET', url, ['Accept', '*/*'], '', 'Script') : undefined;
    schedule(() => runScriptElement(id, finish), 0);
  }
  else runScriptElement(id);
}

// The engine's entry points, called by name from Rust.

function executeParserScript(id) {
  if (startedScripts.has(id)) return '';
  startedScripts.add(id);
  return runScriptElement(id);
}

function setReadyState(value) {
  readyState = value;
  api.markReady(value);
  fire(document, 'readystatechange');
}

function documentParsed() {
  for (const image of api.byTag(documentId, 'img')) startImage(image);
  setReadyState('interactive');
}

function fireDOMContentLoaded() {
  fire(document, 'DOMContentLoaded', { bubbles: true });
}

function fireLoad() {
  setReadyState('complete');
  fire(globalThis, 'load');
  dispatch(globalThis, new PageTransitionEvent('pageshow', { persisted: false }));
}

// ---- the rest of the window -------------------------------------------------

const customElementRegistry = new Map();
const customElementWaiters = new Map();
// Definitions are recorded, but existing and parsed elements are not
// upgraded: custom element reactions are not implemented.
const customElements = {
  define(name, constructor) {
    if (customElementRegistry.has(name)) {
      throw domError('NotSupportedError', `Failed to execute 'define' on 'CustomElementRegistry': the name "${name}" has already been used with this registry`);
    }
    customElementRegistry.set(name, constructor);
    customElementWaiters.get(name)?.(constructor);
  },
  get: (name) => customElementRegistry.get(name),
  getName: (constructor) => Array.from(customElementRegistry).find(([, c]) => c === constructor)?.[0] ?? null,
  whenDefined(name) {
    if (customElementRegistry.has(name)) return Promise.resolve(customElementRegistry.get(name));
    return new Promise((resolve) => customElementWaiters.set(name, resolve));
  },
  upgrade() {},
};

// HTML only: an XML parser does not exist, and an HTML parse of XML would
// look like it worked.
class DOMParser {
  parseFromString(text, type) {
    if (String(type).toLowerCase() !== 'text/html') {
      throw domError('NotSupportedError', `DOMParser cannot parse '${type}': only text/html is supported.`);
    }
    return inertDocument(String(text).replace(/^\s*<!doctype[^>]*>/i, ''));
  }
}
tagClass(DOMParser, 'DOMParser');

class XMLSerializer {
  serializeToString(node) {
    return api.outerHTML(requireNode(node, 'serializeToString'));
  }
}
tagClass(XMLSerializer, 'XMLSerializer');

function Image(width, height) {
  const img = document.createElement('img');
  if (width !== undefined) img.width = width;
  if (height !== undefined) img.height = height;
  return img;
}
Image.prototype = interfaces.HTMLImageElement.prototype;

function Option(text = '', value, defaultSelected = false, selected = false) {
  const option = document.createElement('option');
  option.text = String(text);
  if (value !== undefined) option.value = value;
  if (defaultSelected) option.setAttribute('selected', '');
  if (selected) option.selected = true;
  return option;
}
Option.prototype = interfaces.HTMLOptionElement.prototype;

function Audio(src) {
  const audio = document.createElement('audio');
  if (src !== undefined) audio.src = src;
  return audio;
}
Audio.prototype = interfaces.HTMLAudioElement.prototype;

const CSS = {
  escape: cssEscape,
  supports: () => false,
};

function atob(data) {
  const out = api.atob(String(data));
  if (out === undefined) throw domError('InvalidCharacterError', "Failed to execute 'atob' on 'Window': The string to be decoded is not correctly encoded.");
  return out;
}
function btoa(data) {
  const out = api.btoa(String(data));
  if (out === undefined) throw domError('InvalidCharacterError', "Failed to execute 'btoa' on 'Window': The string to be encoded contains characters outside of the Latin1 range.");
  return out;
}

// Dialogs are logged and answered as if dismissed. A client watching for
// `Page.javascriptDialogOpening` will not see them yet.
function alert(message) {
  console.warn(`[alert] ${message ?? ''}`);
}
function confirm(message) {
  console.warn(`[confirm] ${message ?? ''}`);
  return false;
}
function prompt(message) {
  console.warn(`[prompt] ${message ?? ''}`);
  return null;
}

function postMessage(message, targetOrigin) {
  const data = structuredClone(message);
  schedule(() => dispatch(globalThis, new MessageEvent('message', { data, origin: location.origin, source: globalThis })), 0);
}

class Window extends EventTarget {
  constructor() {
    illegalConstructor();
  }
}
tagClass(Window, 'Window');

// ---- export -----------------------------------------------------------------

const exported = {
  // events
  Event, UIEvent, MouseEvent, PointerEvent, WheelEvent, KeyboardEvent, FocusEvent, InputEvent,
  CompositionEvent, CustomEvent, ErrorEvent, ProgressEvent, MessageEvent, PopStateEvent, HashChangeEvent,
  PageTransitionEvent, SubmitEvent, CloseEvent, StorageEvent, PromiseRejectionEvent, EventTarget,
  // nodes
  Node, CharacterData, Text, Comment, DocumentType, DocumentFragment, ShadowRoot, Document, HTMLDocument,
  Element, HTMLElement, SVGElement, Attr, NodeList, HTMLCollection, DOMStringMap,
  CSSStyleDeclaration, CSSStyleSheet, DOMRect, DOMRectReadOnly, Range, TreeWalker, NodeIterator: TreeWalker,
  NodeFilter, MutationObserver, MutationRecord, Image, Option, Audio,
  // platform
  URL, URLSearchParams, TextEncoder, TextDecoder, Blob, File, FormData, Headers, Request, Response, fetch,
  XMLHttpRequest, XMLHttpRequestEventTarget, XMLHttpRequestUpload, AbortController, AbortSignal,
  WebSocket, Worker, Location, History, Navigator, Storage, MediaQueryList, IntersectionObserver,
  ResizeObserver, PerformanceObserver, DOMParser, XMLSerializer, Window, CanvasRenderingContext2D,
  // the window's objects and functions
  document, history, navigator, screen, localStorage, sessionStorage, performance, crypto, customElements,
  CSS, Date: VirtualDate, structuredClone, matchMedia, getComputedStyle, setTimeout, setInterval,
  clearTimeout, clearInterval: clearTimeout, requestAnimationFrame, cancelAnimationFrame: clearTimeout,
  requestIdleCallback, cancelIdleCallback: clearTimeout, queueMicrotask, reportError, atob, btoa, alert,
  confirm, prompt, postMessage,
  getSelection: () => selection,
  open: () => null,
  close() {},
  stop() {},
  print() {},
  focus() {},
  blur() {},
  scroll: (...args) => scrollNode(documentId, args, false),
  scrollTo: (...args) => scrollNode(documentId, args, false),
  scrollBy: (...args) => scrollNode(documentId, args, true),
  moveTo() {},
  resizeTo() {},
  devicePixelRatio: 1,
  screenX: 0,
  screenY: 0,
  screenLeft: 0,
  screenTop: 0,
  name: '',
  status: '',
  closed: false,
  opener: null,
  frameElement: null,
  isSecureContext: true,
  crossOriginIsolated: false,
  origin: location.origin,
  frames: globalThis,
  length: 0,
};
for (const interfaceName of Object.keys(interfaces)) exported[interfaceName] = interfaces[interfaceName];

for (const [name, value] of Object.entries(exported)) {
  Object.defineProperty(globalThis, name, { value, writable: true, configurable: true, enumerable: false });
}
for (const name of ['window', 'self', 'top', 'parent', 'globalThis']) {
  Object.defineProperty(globalThis, name, { value: globalThis, writable: true, configurable: true, enumerable: false });
}
Object.defineProperties(globalThis, {
  // `location = '/next'` navigates, as `location.href = ...` does.
  location: { get: () => location, set: (v) => navigateTo(v), configurable: true },
  innerWidth: { get: () => api.viewportWidth(), configurable: true },
  innerHeight: { get: () => api.viewportHeight(), configurable: true },
  outerWidth: { get: () => api.viewportWidth(), configurable: true },
  outerHeight: { get: () => api.viewportHeight(), configurable: true },
  scrollX: { get: () => api.scrollOffset(documentId)[0], configurable: true },
  scrollY: { get: () => api.scrollOffset(documentId)[1], configurable: true },
  pageXOffset: { get: () => api.scrollOffset(documentId)[0], configurable: true },
  pageYOffset: { get: () => api.scrollOffset(documentId)[1], configurable: true },
});

// The engine's entry points, by the names browser.rs and js.rs call.
for (const [name, fn] of Object.entries({
  __executeScript: executeParserScript,
  __documentParsed: documentParsed,
  __fireDOMContentLoaded: fireDOMContentLoaded,
  __fireLoad: fireLoad,
  __beginTurn: beginTurn,
  __runNextTask: runNextTask,
})) {
  Object.defineProperty(globalThis, name, { value: fn, writable: false, configurable: false, enumerable: false });
}

// The window is an EventTarget with `on*` handlers, so `window.onload = ...`
// and a bare `addEventListener(...)` work.
try {
  Object.setPrototypeOf(globalThis, Window.prototype);
} catch (e) {
  for (const key of ['addEventListener', 'removeEventListener', 'dispatchEvent']) {
    Object.defineProperty(globalThis, key, { value: EventTarget.prototype[key], writable: true, configurable: true });
  }
}

// Named access: `<div id=menu>` is `window.menu`, and a bare `menu` in
// script, as long as nothing else has that name. In a browser these come
// from an object between the window and Window.prototype; here it is a
// proxy in that place, which QuickJS consults for bare names too. Real
// globals and anything script assigns come first, as they do there.
try {
  const windowPrototype = Object.getPrototypeOf(globalThis);
  const namedProperties = new Proxy(windowPrototype, {
    get(target, key, receiver) {
      if (typeof key === 'string' && !(key in target)) {
        const id = api.namedItem(key);
        if (id !== undefined) return wrap(id);
      }
      return Reflect.get(target, key, receiver);
    },
    has(target, key) {
      return Reflect.has(target, key) || (typeof key === 'string' && api.namedItem(key) !== undefined);
    },
    // So that `window instanceof Window` still holds.
    getPrototypeOf: () => windowPrototype,
  });
  Object.setPrototypeOf(globalThis, namedProperties);
} catch (e) {
  // Without it, pages that use the names fail as they would anywhere else.
}
