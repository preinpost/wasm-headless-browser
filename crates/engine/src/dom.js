// The JavaScript-side DOM.
//
// Rust exposes primitives keyed by numeric node id under `globalThis.__dom`.
// Everything object-shaped is built here, in JavaScript, because that is where
// the two things a binding needs are cheap:
//
//   * **identity.** `document.body === document.body` has to hold, and clients
//     compare elements with `===`. A cache in JS gives that for free. Doing it
//     in Rust would mean holding persistent object handles.
//   * **shape.** Getters, methods and coercions are ordinary JavaScript once
//     the primitives exist.
//
// This file is compiled in with `include_str!`, so it is real JavaScript that
// an editor can check.

(function () {
  const api = globalThis.__dom;
  if (!api) throw new Error('the DOM primitives were not installed');

  // Node ids are indices into the engine's arena. Wrappers are cached by id so
  // that the same node is always the same object.
  const wrappers = new Map();

  function wrap(id) {
    if (!id || id <= 0) return null;
    let node = wrappers.get(id);
    if (node) return node;
    node = makeNode(id);
    wrappers.set(id, node);
    return node;
  }

  function wrapAll(ids) {
    const out = [];
    for (let i = 0; i < ids.length; i++) {
      const node = wrap(ids[i]);
      if (node) out.push(node);
    }
    return out;
  }

  // A collection that also behaves a little like an array, because page code
  // indexes into it and calls forEach.
  function styleFromPairs(pairs) {
    const style = {};
    for (let i = 0; i + 1 < pairs.length; i += 2) {
      // camelCase, the way CSSStyleDeclaration exposes it.
      const name = String(pairs[i]).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
      style[name] = pairs[i + 1];
    }
    return style;
  }

  function attributeList(pairs) {
    const out = [];
    for (let i = 0; i + 1 < pairs.length; i += 2) {
      out.push({ name: pairs[i], value: pairs[i + 1], nodeName: pairs[i], nodeValue: pairs[i + 1] });
    }
    return out;
  }

  function collection(nodes) {
    const list = nodes.slice();
    Object.defineProperty(list, 'item', {
      value: (i) => list[i] || null,
      enumerable: false,
    });
    return list;
  }

  function makeNode(id) {
    const node = {};

    const define = (name, getter) => {
      Object.defineProperty(node, name, { get: getter, configurable: true, enumerable: true });
    };
    const method = (name, fn) => {
      Object.defineProperty(node, name, { value: fn, configurable: true, enumerable: false });
    };

    // ---- identity and tree position ----
    define('nodeType', () => api.nodeType(id));
    define('tagName', () => api.tagName(id).toUpperCase());
    define('nodeName', () => api.nodeName(id));
    define('localName', () => api.tagName(id));
    define('id', () => api.attr(id, 'id') || '');
    define('className', () => api.attr(id, 'class') || '');
    define('parentNode', () => wrap(api.parent(id)));
    define('parentElement', () => {
      const parent = api.parent(id);
      return parent && api.nodeType(parent) === 1 ? wrap(parent) : null;
    });
    define('childNodes', () => collection(wrapAll(api.children(id))));
    define('children', () =>
      collection(wrapAll(api.children(id)).filter((n) => n.nodeType === 1)),
    );
    define('firstChild', () => wrap(api.children(id)[0] || 0));
    define('firstElementChild', () => {
      const kids = api.children(id);
      for (let i = 0; i < kids.length; i++) {
        if (api.nodeType(kids[i]) === 1) return wrap(kids[i]);
      }
      return null;
    });
    define('lastChild', () => {
      const kids = api.children(id);
      return wrap(kids[kids.length - 1] || 0);
    });
    define('nextSibling', () => wrap(api.sibling(id, 1)));
    define('previousSibling', () => wrap(api.sibling(id, -1)));
    define('nextElementSibling', () => {
      let next = api.sibling(id, 1);
      while (next && api.nodeType(next) !== 1) next = api.sibling(next, 1);
      return wrap(next);
    });

    // ---- attributes and content ----
    define('textContent', () => api.textContent(id));
    define('innerText', () => api.textContent(id));
    define('innerHTML', () => api.innerHTML(id));
    define('outerHTML', () => api.outerHTML(id));
    define('hidden', () => api.hasAttr(id, 'hidden'));
    define('title', () => api.attr(id, 'title') || '');
    define('href', () => api.attr(id, 'href') || '');
    define('src', () => api.attr(id, 'src') || '');
    define('alt', () => api.attr(id, 'alt') || '');
    define('name', () => api.attr(id, 'name') || '');
    define('type', () => api.attr(id, 'type') || (api.tagName(id) === 'input' ? 'text' : ''));
    define('placeholder', () => api.attr(id, 'placeholder') || '');
    define('value', () => api.value(id));
    define('checked', () => api.checked(id));
    define('disabled', () => api.hasAttr(id, 'disabled'));
    // Present so that page code testing for it does not throw. A handler that
    // is never invoked is worse than no handler at all, but throwing here would
    // break scripts that feature-detect.
    // Inline handlers are not compiled yet, so this is always null. A client
    // that checks `el.onclick !== null` therefore finds nothing.
    define('onclick', () => null);
    define('style', () => styleFromPairs(api.stylePairs(id)));
    define('attributes', () => attributeList(api.attributeList(id)));

    method('getAttribute', (name) => {
      const value = api.attr(id, String(name));
      return value === undefined ? null : value;
    });
    method('setAttribute', (name, value) => {
      api.setAttr(id, String(name), String(value));
    });
    method('removeAttribute', (name) => {
      api.removeAttr(id, String(name));
    });
    method('hasAttribute', (name) => api.hasAttr(id, String(name)));
    method('getAttributeNames', () => api.attributeNames(id));

    method('querySelector', (selector) => wrap(api.query(id, String(selector))));
    method('querySelectorAll', (selector) =>
      collection(wrapAll(api.queryAll(id, String(selector)))),
    );
    method('getElementsByTagName', (tag) =>
      collection(wrapAll(api.byTag(id, String(tag).toLowerCase()))),
    );
    method('getElementsByClassName', (name) =>
      collection(wrapAll(api.queryAll(id, '.' + String(name).trim().replace(/\s+/g, '.')))),
    );
    method('matches', (selector) => api.matches(id, String(selector)));
    method('closest', (selector) => {
      let current = id;
      while (current) {
        if (api.nodeType(current) === 1 && api.matches(current, String(selector))) {
          return wrap(current);
        }
        current = api.parent(current);
      }
      return null;
    });
    method('contains', (other) => {
      if (!other) return false;
      let current = other.__nodeId;
      while (current) {
        if (current === id) return true;
        current = api.parent(current);
      }
      return false;
    });

    // ---- geometry ----
    //
    // The engine has no layout yet, so these are honest zeroes rather than
    // invented numbers. A client asking whether a box is visible gets "no",
    // which is the safe answer.
    method('getBoundingClientRect', () => {
      const box = api.rect(id);
      const x = box[0] || 0;
      const y = box[1] || 0;
      const width = box[2] || 0;
      const height = box[3] || 0;
      const rect = {
        x: x,
        y: y,
        width: width,
        height: height,
        top: y,
        right: x + width,
        bottom: y + height,
        left: x,
      };
      rect.toJSON = () => rect;
      return rect;
    });
    method('getClientRects', () => collection([]));
    method('scrollIntoView', () => {});
    method('focus', () => api.focus(id));
    method('blur', () => {});
    method('click', () => api.dispatch(id, 'click'));
    method('scrollBy', () => {});
    method('scrollTo', () => {});

    // ---- events ----
    method('addEventListener', (type, handler) => api.addListener(id, String(type), handler));
    method('removeEventListener', (type, handler) => api.removeListener(id, String(type), handler));
    method('dispatchEvent', (event) => api.dispatchEvent(id, event));

    // ---- mutation ----
    method('appendChild', (child) => {
      api.appendChild(id, child ? child.__nodeId : 0);
      return child;
    });
    method('removeChild', (child) => {
      api.removeNode(child ? child.__nodeId : 0);
      return child;
    });
    method('remove', () => api.removeNode(id));
    method('insertBefore', (child) => {
      api.appendChild(id, child ? child.__nodeId : 0);
      return child;
    });
    method('cloneNode', () => wrap(api.cloneNode(id)));

    Object.defineProperty(node, '__nodeId', { value: id, enumerable: false });
    return node;
  }

  const documentId = api.documentId();

  // Hit testing needs boxes, and there are none yet. Returning null means "no
  // blocker", which lets a client's click path proceed rather than abort; the
  // input methods themselves refuse until layout exists.
  function elementFromPoint() {
    return null;
  }

  const document = (function () {
    const doc = makeNode(documentId);
    Object.defineProperty(doc, 'nodeType', { value: 9, enumerable: true });
    Object.defineProperty(doc, 'nodeName', { value: '#document', enumerable: true });
    Object.defineProperty(doc, 'documentElement', { get: () => wrap(api.documentElement()), enumerable: true });
    Object.defineProperty(doc, 'body', { get: () => wrap(api.body()), enumerable: true });
    Object.defineProperty(doc, 'head', { get: () => wrap(api.head()), enumerable: true });
    Object.defineProperty(doc, 'title', {
      get: () => api.title(),
      set: (value) => api.setTitle(String(value)),
      enumerable: true,
    });
    Object.defineProperty(doc, 'URL', { get: () => api.url(), enumerable: true });
    Object.defineProperty(doc, 'documentURI', { get: () => api.url(), enumerable: true });
    Object.defineProperty(doc, 'readyState', { get: () => api.readyState(), enumerable: true });
    Object.defineProperty(doc, 'activeElement', { get: () => wrap(api.activeElement()), enumerable: true });
    Object.defineProperty(doc, 'location', { get: () => location, enumerable: true });
    Object.defineProperty(doc, 'defaultView', { get: () => globalThis, enumerable: true });

    doc.getElementById = (value) => wrap(api.getElementById(String(value)));
    doc.createElement = (tag) => wrap(api.createElement(String(tag)));
    doc.createTextNode = (text) => wrap(api.createTextNode(String(text)));
    doc.createDocumentFragment = () => wrap(api.createElement('fragment'));
    doc.elementFromPoint = elementFromPoint;
    doc.elementsFromPoint = () => [];
    return doc;
  })();

  const location = {
    get href() {
      return api.url();
    },
    set href(value) {
      api.navigate(String(value));
    },
    get origin() {
      return api.origin();
    },
    get pathname() {
      try {
        return new URL(api.url()).pathname;
      } catch (e) {
        return '/';
      }
    },
    get search() {
      try {
        return new URL(api.url()).search;
      } catch (e) {
        return '';
      }
    },
    get hash() {
      try {
        return new URL(api.url()).hash;
      } catch (e) {
        return '';
      }
    },
    assign(value) {
      api.navigate(String(value));
    },
    replace(value) {
      api.navigate(String(value));
    },
    reload() {},
    toString() {
      return api.url();
    },
  };

  const history = {
    get length() {
      return api.historyLength();
    },
    back() {
      api.historyBack();
    },
    forward() {
      api.historyForward();
    },
    go(delta) {
      if (delta < 0) api.historyBack();
      else if (delta > 0) api.historyForward();
    },
    pushState(_state, _title, url) {
      if (url) api.navigate(String(url));
    },
    replaceState() {},
  };

  // `getComputedStyle` is a stub that reports what the attribute-level styles
  // say. A client reading `cursor`, `display` or `visibility` gets a plausible
  // answer; a client reading anything else gets an empty string rather than an
  // exception. The real cascade arrives with layout.
  // The five properties a client reads are filled from the engine. Anything
  // else reads as an empty string rather than throwing, so a page script that
  // probes an unimplemented property keeps running.
  const STYLE_KEYS = ['display', 'visibility', 'cursor', 'opacity', 'position'];
  function computedStyleFrom(values) {
    const style = {};
    for (let i = 0; i < STYLE_KEYS.length; i++) {
      style[STYLE_KEYS[i]] = values[i] || '';
    }
    return new Proxy(style, {
      get(target, key) {
        if (key in target) return target[key];
        if (typeof key === 'symbol') return undefined;
        return '';
      },
    });
  }
  function emptyStyle() {
    return computedStyleFrom(STYLE_KEYS.map(() => ''));
  }
  function getComputedStyle(element) {
    if (!element || !element.__nodeId) return emptyStyle();
    return computedStyleFrom(api.computedStyle(element.__nodeId));
  }

  const navigator = {
    userAgent: api.userAgent(),
    language: 'en-US',
    languages: ['en-US', 'en'],
    platform: 'WASIX',
    onLine: true,
    cookieEnabled: false,
    maxTouchPoints: 0,
  };

  const localStorage = (function () {
    const store = new Map();
    return {
      getItem: (key) => (store.has(String(key)) ? store.get(String(key)) : null),
      setItem: (key, value) => store.set(String(key), String(value)),
      removeItem: (key) => store.delete(String(key)),
      clear: () => store.clear(),
      key: (i) => Array.from(store.keys())[i] ?? null,
      get length() {
        return store.size;
      },
    };
  })();

  // Timers are queued and drained by the engine after a script finishes.
  // Nothing runs asynchronously: there is no real event loop yet.
  const timers = new Map();
  let nextTimer = 1;
  function setTimeout(fn, _delay, ...args) {
    if (typeof fn !== 'function') return 0;
    const handle = nextTimer++;
    timers.set(handle, () => fn(...args));
    return handle;
  }
  function clearTimeout(handle) {
    timers.delete(handle);
  }
  const setInterval = setTimeout;
  const clearInterval = clearTimeout;

  Object.assign(globalThis, {
    window: globalThis,
    self: globalThis,
    document,
    location,
    history,
    navigator,
    localStorage,
    sessionStorage: localStorage,
    innerWidth: api.viewportWidth(),
    innerHeight: api.viewportHeight(),
    outerWidth: api.viewportWidth(),
    outerHeight: api.viewportHeight(),
    devicePixelRatio: 1,
    screenX: 0,
    screenY: 0,
    pageXOffset: 0,
    pageYOffset: 0,
    scrollX: 0,
    scrollY: 0,
    getComputedStyle,
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    requestAnimationFrame: (fn) => setTimeout(fn, 0),
    cancelAnimationFrame: clearTimeout,
    matchMedia: (query) => ({
      matches: false,
      media: String(query),
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
    }),
    scrollBy() {},
    scrollTo() {},
    scroll() {},
    focus() {},
    blur() {},
    open: () => null,
    close() {},
    alert() {},
    confirm: () => false,
    prompt: () => null,
    // A page that looks for jQuery or a framework will not find it, which is
    // correct: nothing has been loaded.
    __wrap: wrap,
    __drainTimers: () => {
      const pending = Array.from(timers.values());
      timers.clear();
      for (const fn of pending) fn();
      return pending.length;
    },
  });

  // `document.write` during parsing is common enough to be worth accepting.
  // It appends text to the body rather than re-parsing, which is wrong for the
  // full specification and right often enough to keep a page alive.
  document.write = (...parts) => api.write(parts.join(''));
  document.writeln = (...parts) => api.write(parts.join('') + '\n');

  // Ready state transitions, in the order a page observes them.
  api.markReady('loading');
  api.markReady('interactive');
  api.markReady('complete');
  api.fireReadyEvents();
})();
