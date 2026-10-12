// Part 3: elements. Attributes, `classList`, `dataset`, inline `style`, markup
// in and out, geometry, focus and clicking.

// HTML attribute names fold case; SVG and other foreign elements do not.
function attributeName(el, name) {
  name = String(name);
  return api.namespace(own(el)) === 'http://www.w3.org/1999/xhtml' ? name.toLowerCase() : name;
}

function setAttr(el, name, value) {
  const id = own(el);
  name = attributeName(el, name);
  const oldValue = api.attr(id, name) ?? null;
  api.setAttr(id, name, String(value));
  queueRecord('attributes', id, { attributeName: name, oldValue });
  if (name === 'src' && api.tagName(id) === 'script') prepareScript(id);
  if ((name === 'src' || name === 'srcset') && api.tagName(id) === 'img') startImage(id);
}

function removeAttr(el, name) {
  const id = own(el);
  name = attributeName(el, name);
  if (!api.hasAttr(id, name)) return;
  const oldValue = api.attr(id, name) ?? null;
  api.removeAttr(id, name);
  queueRecord('attributes', id, { attributeName: name, oldValue });
  if ((name === 'src' || name === 'srcset') && api.tagName(id) === 'img') startImage(id);
}

class Attr {
  constructor() {
    illegalConstructor();
  }
}
tagClass(Attr, 'Attr');
function attrNode(el, name) {
  return {
    __proto__: Attr.prototype,
    name,
    localName: name,
    nodeName: name,
    namespaceURI: null,
    prefix: null,
    specified: true,
    ownerElement: el,
    get value() {
      return el.getAttribute(name) ?? '';
    },
    set value(v) {
      el.setAttribute(name, v);
    },
    get nodeValue() {
      return this.value;
    },
  };
}

// Markup parsed in the context of `contextId`, as `innerHTML` parses it.
// Scripts in it are marked started, because `innerHTML` never runs them.
function parseMarkup(contextId, html, runScripts) {
  const fragment = api.parseFragment(contextId, html === null || html === undefined ? '' : String(html));
  if (!runScripts) for (const s of api.byTag(fragment, 'script')) startedScripts.add(s);
  return wrap(fragment);
}

const tokenLists = new WeakMap();
function tokenList(el, attribute) {
  let lists = tokenLists.get(el);
  if (!lists) tokenLists.set(el, (lists = new Map()));
  if (lists.has(attribute)) return lists.get(attribute);
  const read = () => (el.getAttribute(attribute) || '').split(/[\t\n\f\r ]+/).filter(Boolean);
  const write = (tokens) => {
    if (tokens.length === 0 && !el.hasAttribute(attribute)) return;
    el.setAttribute(attribute, Array.from(new Set(tokens)).join(' '));
  };
  const list = {
    get length() {
      return read().length;
    },
    get value() {
      return el.getAttribute(attribute) || '';
    },
    set value(v) {
      el.setAttribute(attribute, v);
    },
    item: (i) => read()[i] ?? null,
    contains: (token) => read().includes(String(token)),
    add(...tokens) {
      write(read().concat(tokens.map(String)));
    },
    remove(...tokens) {
      const gone = tokens.map(String);
      write(read().filter((t) => !gone.includes(t)));
    },
    toggle(token, force) {
      token = String(token);
      const has = read().includes(token);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.add(token);
      if (!want && has) this.remove(token);
      return want;
    },
    replace(oldToken, newToken) {
      const tokens = read();
      const i = tokens.indexOf(String(oldToken));
      if (i < 0) return false;
      tokens[i] = String(newToken);
      write(tokens);
      return true;
    },
    supports: () => true,
    forEach(callback, thisArg) {
      read().forEach((t, i) => callback.call(thisArg, t, i, proxy));
    },
    entries: () => read().entries(),
    keys: () => read().keys(),
    values: () => read().values(),
    [Symbol.iterator]: () => read()[Symbol.iterator](),
    toString: () => el.getAttribute(attribute) || '',
    [Symbol.toStringTag]: 'DOMTokenList',
  };
  const proxy = new Proxy(list, {
    get(target, key, receiver) {
      if (typeof key === 'string' && /^\d+$/.test(key)) return read()[Number(key)];
      return Reflect.get(target, key, receiver);
    },
  });
  lists.set(attribute, proxy);
  return proxy;
}

const datasets = new WeakMap();
function datasetFor(el) {
  let data = datasets.get(el);
  if (data) return data;
  const attr = (key) => 'data-' + String(key).replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());
  const keys = () =>
    el.getAttributeNames()
      .filter((n) => n.startsWith('data-'))
      .map((n) => n.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase()));
  data = new Proxy(Object.create(DOMStringMap.prototype), {
    get: (t, key) => (typeof key === 'string' ? el.getAttribute(attr(key)) ?? undefined : t[key]),
    set: (t, key, value) => (el.setAttribute(attr(key), String(value)), true),
    has: (t, key) => typeof key === 'string' && el.hasAttribute(attr(key)),
    deleteProperty: (t, key) => (el.removeAttribute(attr(key)), true),
    ownKeys: () => keys(),
    getOwnPropertyDescriptor: (t, key) =>
      el.hasAttribute(attr(key))
        ? { value: el.getAttribute(attr(key)), writable: true, enumerable: true, configurable: true }
        : undefined,
  });
  datasets.set(el, data);
  return data;
}
class DOMStringMap {}
tagClass(DOMStringMap, 'DOMStringMap');

// ---- inline style -----------------------------------------------------------

// `a: b; c: d !important` into [name, value, important] triples. A value
// containing `;` inside `url(...)` is split wrongly; rare enough for now.
function parseDeclarations(text) {
  const out = [];
  for (const part of String(text || '').split(';')) {
    const colon = part.indexOf(':');
    if (colon < 0) continue;
    const name = part.slice(0, colon).trim();
    let value = part.slice(colon + 1).trim();
    if (!name) continue;
    const key = name.startsWith('--') ? name : name.toLowerCase();
    const important = /!\s*important$/i.test(value);
    if (important) value = value.replace(/\s*!\s*important$/i, '');
    const existing = out.findIndex(([n]) => n === key);
    if (existing >= 0) out.splice(existing, 1);
    out.push([key, value, important]);
  }
  return out;
}

// `backgroundColor` to `background-color`, `webkitTransform` to
// `-webkit-transform`. Custom properties pass through.
function kebab(property) {
  if (property.startsWith('--')) return property;
  if (property === 'cssFloat') return 'float';
  return property.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase()).replace(/^(webkit|moz|ms)-/, '-$1-');
}

class CSSStyleDeclaration {
  constructor() {
    illegalConstructor();
  }
}
tagClass(CSSStyleDeclaration, 'CSSStyleDeclaration');

const inlineStyles = new WeakMap();
function inlineStyle(el) {
  let style = inlineStyles.get(el);
  if (style) return style;
  const read = () => parseDeclarations(el.getAttribute('style'));
  const write = (decls) => {
    const text = decls.map(([n, v, imp]) => `${n}: ${v}${imp ? ' !important' : ''};`).join(' ');
    if (text || el.hasAttribute('style')) el.setAttribute('style', text);
  };
  const normal = (name) => (String(name).startsWith('--') ? String(name) : String(name).toLowerCase());
  const decl = Object.assign(Object.create(CSSStyleDeclaration.prototype), {
    getPropertyValue(name) {
      const d = read().find(([n]) => n === normal(name));
      return d ? d[1] : '';
    },
    getPropertyPriority(name) {
      const d = read().find(([n]) => n === normal(name));
      return d && d[2] ? 'important' : '';
    },
    setProperty(name, value, priority) {
      const key = normal(name);
      const decls = read().filter(([n]) => n !== key);
      if (value !== null && value !== undefined && String(value) !== '') {
        decls.push([key, String(value), String(priority || '').toLowerCase() === 'important']);
      }
      write(decls);
    },
    removeProperty(name) {
      const old = this.getPropertyValue(name);
      write(read().filter(([n]) => n !== normal(name)));
      return old;
    },
    item(index) {
      const d = read()[index];
      return d ? d[0] : '';
    },
  });
  Object.defineProperties(decl, {
    length: { get: () => read().length },
    cssText: {
      get: () => el.getAttribute('style') || '',
      set: (v) => el.setAttribute('style', String(v ?? '')),
    },
    parentRule: { value: null },
  });
  style = new Proxy(decl, {
    get(target, key, receiver) {
      if (typeof key !== 'string' || key in target) return Reflect.get(target, key, receiver);
      if (/^\d+$/.test(key)) return target.item(Number(key));
      return target.getPropertyValue(kebab(key));
    },
    set(target, key, value, receiver) {
      if (typeof key !== 'string' || key in target) return Reflect.set(target, key, value, receiver);
      target.setProperty(kebab(key), value);
      return true;
    },
  });
  inlineStyles.set(el, style);
  return style;
}

// ---- geometry ---------------------------------------------------------------
//
// There is no layout yet, so every box is zero-sized. That makes every
// element "not visible" to a client, which is the safe direction to be wrong
// in.

class DOMRectReadOnly {
  constructor(x = 0, y = 0, width = 0, height = 0) {
    this.x = x;
    this.y = y;
    this.width = width;
    this.height = height;
    this.top = Math.min(y, y + height);
    this.right = Math.max(x, x + width);
    this.bottom = Math.max(y, y + height);
    this.left = Math.min(x, x + width);
  }
  static fromRect(r = {}) {
    return new this(r.x, r.y, r.width, r.height);
  }
  toJSON() {
    const { x, y, width, height, top, right, bottom, left } = this;
    return { x, y, width, height, top, right, bottom, left };
  }
}
tagClass(DOMRectReadOnly, 'DOMRectReadOnly');
class DOMRect extends DOMRectReadOnly {}
tagClass(DOMRect, 'DOMRect');

const boxOf = (el) => {
  const [x, y, width, height] = api.rect(own(el));
  return new DOMRect(x || 0, y || 0, width || 0, height || 0);
};

// ---- scrolling ----------------------------------------------------------------
// Offsets live in the engine (scroll.rs), so boxes, hit testing and painting
// see them. The root element's are the viewport's.

const scrollId = (el) => (el === document.documentElement ? documentId : own(el));

// `scroll` fires once per task for each container that moved: at the
// document, bubbling to the window, for the viewport, at the element, not
// bubbling, for the rest.
const scrolledIds = new Set();
function noteScrolled(ids) {
  const first = scrolledIds.size === 0;
  for (const id of ids) scrolledIds.add(id);
  if (!first || scrolledIds.size === 0) return;
  schedule(() => {
    const ids = [...scrolledIds];
    scrolledIds.clear();
    for (const id of ids) {
      if (id === documentId) fire(document, 'scroll', { bubbles: true });
      else fire(wrap(id), 'scroll');
    }
  }, 0);
}

// `scrollTo(x, y)`, `scrollTo({ left, top })`, and `scrollBy` likewise.
function scrollNode(id, args, relative) {
  let [x, y] = api.scrollOffset(id);
  const number = (v, base) => {
    const n = Number(v);
    return Number.isFinite(n) ? (relative ? base + n : n) : base;
  };
  if (args.length === 1 && args[0] !== null && typeof args[0] === 'object') {
    if (args[0].left !== undefined) x = number(args[0].left, x);
    if (args[0].top !== undefined) y = number(args[0].top, y);
  } else if (args.length >= 2) {
    x = number(args[0], x);
    y = number(args[1], y);
  }
  if (api.scrollTo(id, x, y)) noteScrolled([id]);
}

// `scrollIntoView(alignToTop)` or `scrollIntoView({ block, inline })`.
function scrollIntoViewOf(el, arg) {
  let block = 'start';
  let inline = 'nearest';
  if (arg === false) block = 'end';
  else if (arg !== null && typeof arg === 'object') {
    block = arg.block ?? 'start';
    inline = arg.inline ?? 'nearest';
  }
  noteScrolled(api.scrollIntoView(own(el), String(block), String(inline)));
}

// ---- Element ----------------------------------------------------------------

class Element extends Node {
  get tagName() {
    return api.nodeName(own(this));
  }
  get localName() {
    return api.tagName(own(this));
  }
  get namespaceURI() {
    return api.namespace(own(this)) || null;
  }
  get prefix() {
    return null;
  }
  get id() {
    return this.getAttribute('id') ?? '';
  }
  set id(value) {
    this.setAttribute('id', value);
  }
  get className() {
    return this.getAttribute('class') ?? '';
  }
  set className(value) {
    this.setAttribute('class', value);
  }
  get classList() {
    return tokenList(this, 'class');
  }
  get slot() {
    return this.getAttribute('slot') ?? '';
  }
  get attributes() {
    const names = api.attributeNames(own(this));
    const list = names.map((n) => attrNode(this, n));
    for (const attr of list) if (!(attr.name in list)) list[attr.name] = attr;
    list.getNamedItem = (n) => (this.hasAttribute(n) ? attrNode(this, attributeName(this, n)) : null);
    list.item = (i) => list[i] ?? null;
    return list;
  }
  getAttribute(name) {
    return api.attr(own(this), attributeName(this, name)) ?? null;
  }
  getAttributeNS(namespace, name) {
    return this.getAttribute(String(name).split(':').pop());
  }
  setAttribute(name, value) {
    setAttr(this, name, value);
  }
  setAttributeNS(namespace, name, value) {
    setAttr(this, String(name).split(':').pop(), value);
  }
  removeAttribute(name) {
    removeAttr(this, name);
  }
  removeAttributeNS(namespace, name) {
    removeAttr(this, String(name).split(':').pop());
  }
  hasAttribute(name) {
    return api.hasAttr(own(this), attributeName(this, name));
  }
  hasAttributeNS(namespace, name) {
    return this.hasAttribute(String(name).split(':').pop());
  }
  hasAttributes() {
    return api.attributeNames(own(this)).length > 0;
  }
  getAttributeNames() {
    return api.attributeNames(own(this));
  }
  getAttributeNode(name) {
    return this.hasAttribute(name) ? attrNode(this, attributeName(this, name)) : null;
  }
  toggleAttribute(name, force) {
    const has = this.hasAttribute(name);
    const want = force === undefined ? !has : !!force;
    if (want && !has) this.setAttribute(name, '');
    if (!want && has) this.removeAttribute(name);
    return want;
  }
  matches(selector) {
    return api.matches(own(this), checkedSelector(selector, 'matches'));
  }
  webkitMatchesSelector(selector) {
    return this.matches(selector);
  }
  closest(selector) {
    const text = checkedSelector(selector, 'closest');
    for (let id = own(this); id !== undefined; id = api.parent(id)) {
      if (isElementId(id) && api.matches(id, text)) return wrap(id);
    }
    return null;
  }
  get innerHTML() {
    return api.innerHTML(own(this));
  }
  set innerHTML(html) {
    const fragment = parseMarkup(own(this), html, false);
    this.replaceChildren(fragment);
  }
  get outerHTML() {
    return api.outerHTML(own(this));
  }
  set outerHTML(html) {
    const parent = this.parentNode;
    if (!parent) return;
    const context = parent.nodeType === 1 ? own(parent) : api.body() ?? documentId;
    parent.insertBefore(parseMarkup(context, html, false), this);
    removeNode(this);
  }
  insertAdjacentHTML(position, html) {
    const where = String(position).toLowerCase();
    const context = where === 'beforebegin' || where === 'afterend' ? api.parent(own(this)) ?? documentId : own(this);
    insertAdjacent(this, where, parseMarkup(context, html, false));
  }
  insertAdjacentElement(position, element) {
    return insertAdjacent(this, String(position).toLowerCase(), element);
  }
  insertAdjacentText(position, text) {
    insertAdjacent(this, String(position).toLowerCase(), document.createTextNode(String(text)));
  }
  getBoundingClientRect() {
    return boxOf(this);
  }
  getClientRects() {
    const box = boxOf(this);
    return box.width || box.height ? [box] : [];
  }
  get clientWidth() {
    return api.scrollSizes(scrollId(this))[2];
  }
  get clientHeight() {
    return api.scrollSizes(scrollId(this))[3];
  }
  get clientTop() {
    return 0;
  }
  get clientLeft() {
    return 0;
  }
  get scrollWidth() {
    return api.scrollSizes(scrollId(this))[0];
  }
  get scrollHeight() {
    return api.scrollSizes(scrollId(this))[1];
  }
  get scrollTop() {
    return api.scrollOffset(scrollId(this))[1];
  }
  set scrollTop(value) {
    scrollNode(scrollId(this), [{ top: value }], false);
  }
  get scrollLeft() {
    return api.scrollOffset(scrollId(this))[0];
  }
  set scrollLeft(value) {
    scrollNode(scrollId(this), [{ left: value }], false);
  }
  scroll(...args) {
    scrollNode(scrollId(this), args, false);
  }
  scrollTo(...args) {
    scrollNode(scrollId(this), args, false);
  }
  scrollBy(...args) {
    scrollNode(scrollId(this), args, true);
  }
  scrollIntoView(arg) {
    scrollIntoViewOf(this, arg);
  }
  scrollIntoViewIfNeeded(center = true) {
    const how = center ? 'if-needed' : 'nearest';
    noteScrolled(api.scrollIntoView(own(this), how, how));
  }
  attachShadow() {
    // Shadow trees are not rendered or projected; script gets a working
    // fragment it can build into, which is enough for most components.
    const root = Object.setPrototypeOf(new DocumentFragment(), ShadowRoot.prototype);
    shadowRoots.set(this, root);
    shadowHosts.set(root, this);
    return root;
  }
  get shadowRoot() {
    return shadowRoots.get(this) ?? null;
  }
  get assignedSlot() {
    return null;
  }
  animate() {
    return { finished: Promise.resolve(), cancel() {}, finish() {}, play() {}, pause() {}, onfinish: null };
  }
  getAnimations() {
    return [];
  }
  requestFullscreen() {
    return Promise.reject(domError('NotSupportedError', 'Fullscreen is not available in a headless engine.'));
  }
  setPointerCapture() {}
  releasePointerCapture() {}
  hasPointerCapture() {
    return false;
  }
}
tagClass(Element, 'Element');
mixin(Element, ParentNode);
mixin(Element, ChildNode);
const shadowRoots = new WeakMap();

function insertAdjacent(el, where, node) {
  switch (where) {
    case 'beforebegin':
      if (!el.parentNode) return null;
      el.parentNode.insertBefore(node, el);
      return node;
    case 'afterbegin':
      el.insertBefore(node, el.firstChild);
      return node;
    case 'beforeend':
      el.appendChild(node);
      return node;
    case 'afterend':
      if (!el.parentNode) return null;
      el.parentNode.insertBefore(node, el.nextSibling);
      return node;
  }
  throw domError('SyntaxError', `The value provided ('${where}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'.`);
}

// ---- attribute reflection ---------------------------------------------------

function reflect(C, kind, map) {
  for (const [property, attribute] of Object.entries(map)) {
    const descriptors = {
      string: {
        get() {
          return this.getAttribute(attribute) ?? '';
        },
        set(v) {
          this.setAttribute(attribute, String(v));
        },
      },
      boolean: {
        get() {
          return this.hasAttribute(attribute);
        },
        set(v) {
          this.toggleAttribute(attribute, !!v);
        },
      },
      url: {
        get() {
          const v = this.getAttribute(attribute);
          return v === null ? '' : resolveUrl(v) ?? v;
        },
        set(v) {
          this.setAttribute(attribute, String(v));
        },
      },
      number: {
        get() {
          const n = parseInt(this.getAttribute(attribute), 10);
          return Number.isFinite(n) ? n : 0;
        },
        set(v) {
          this.setAttribute(attribute, String(Math.trunc(Number(v)) || 0));
        },
      },
    };
    Object.defineProperty(C.prototype, property, { ...descriptors[kind], configurable: true, enumerable: true });
  }
}

// ---- focus and activation -----------------------------------------------------

const FOCUSABLE_TAGS = new Set(['input', 'select', 'textarea', 'button', 'iframe', 'summary']);
function focusable(el) {
  const id = own(el);
  if (!api.isConnected(id) || !isElementId(id)) return false;
  if (isDisabled(el)) return false;
  const tag = api.tagName(id);
  if (FOCUSABLE_TAGS.has(tag)) return !(tag === 'input' && el.type === 'hidden');
  if ((tag === 'a' || tag === 'area') && api.hasAttr(id, 'href')) return true;
  if (api.hasAttr(id, 'tabindex')) return true;
  const editable = api.attr(id, 'contenteditable');
  return editable === '' || editable === 'true' || editable === 'plaintext-only';
}

function isDisabled(el) {
  const id = own(el);
  const tag = api.tagName(id);
  if (!['button', 'input', 'select', 'textarea', 'optgroup', 'option', 'fieldset'].includes(tag)) return false;
  if (api.hasAttr(id, 'disabled')) return true;
  const fieldset = el.parentElement?.closest('fieldset[disabled]');
  if (!fieldset) return false;
  // A control inside the fieldset's first legend stays enabled.
  const legend = fieldset.firstElementChild;
  return !(legend && legend.localName === 'legend' && legend.contains(el));
}

function focusedElement() {
  const id = api.activeElement();
  return id !== undefined && api.isConnected(id) ? wrap(id) : null;
}

// Moves focus, firing blur/focusout on the old element and focus/focusin on
// the new, in a browser's order. A text control whose value changed while it
// had focus fires `change` as it loses it, before `blur`.
const valueAtFocus = new WeakMap();
function moveFocus(el) {
  const previous = focusedElement();
  if (previous === el) return;
  api.setFocus(el ? own(el) : undefined);
  if (previous) {
    if (valueAtFocus.has(previous)) {
      const before = valueAtFocus.get(previous);
      valueAtFocus.delete(previous);
      if (before !== currentValue(previous)) fire(previous, 'change', { bubbles: true });
    }
    dispatch(previous, new FocusEvent('blur', { relatedTarget: el, view: globalThis }));
    dispatch(previous, new FocusEvent('focusout', { bubbles: true, relatedTarget: el, view: globalThis }));
  }
  if (el) {
    if (isTextControl(el)) valueAtFocus.set(el, currentValue(el));
    dispatch(el, new FocusEvent('focus', { relatedTarget: previous, view: globalThis }));
    dispatch(el, new FocusEvent('focusin', { bubbles: true, relatedTarget: previous, view: globalThis }));
  }
}

const clicking = new WeakSet();

class HTMLElement extends Element {
  get innerText() {
    return api.innerText(own(this));
  }
  set innerText(value) {
    replaceText(this, value);
  }
  get outerText() {
    return this.innerText;
  }
  get style() {
    return inlineStyle(this);
  }
  set style(value) {
    this.setAttribute('style', String(value));
  }
  get dataset() {
    return datasetFor(this);
  }
  get tabIndex() {
    const n = parseInt(this.getAttribute('tabindex'), 10);
    if (Number.isFinite(n)) return n;
    return focusable(this) ? 0 : -1;
  }
  set tabIndex(value) {
    this.setAttribute('tabindex', String(Math.trunc(Number(value)) || 0));
  }
  get contentEditable() {
    return this.getAttribute('contenteditable') ?? 'inherit';
  }
  set contentEditable(value) {
    this.setAttribute('contenteditable', String(value));
  }
  get isContentEditable() {
    const editable = this.closest('[contenteditable]');
    return !!editable && editable.getAttribute('contenteditable') !== 'false';
  }
  get offsetWidth() {
    return boxOf(this).width;
  }
  get offsetHeight() {
    return boxOf(this).height;
  }
  get offsetTop() {
    const parent = this.offsetParent;
    return boxOf(this).top - (parent ? boxOf(parent).top : 0);
  }
  get offsetLeft() {
    const parent = this.offsetParent;
    return boxOf(this).left - (parent ? boxOf(parent).left : 0);
  }
  // The nearest positioned ancestor, or the body. An element that is not
  // rendered, or is fixed, has none. jQuery's `:visible` reads this.
  get offsetParent() {
    const id = own(this);
    if (!api.isConnected(id) || this === document.body || this === document.documentElement) return null;
    for (let n = id; n !== undefined; n = api.parent(n)) {
      if (isElementId(n) && api.computedValue(n, 'display') === 'none') return null;
    }
    if (api.computedValue(id, 'position') === 'fixed') return null;
    for (let p = api.parent(id); p !== undefined && isElementId(p); p = api.parent(p)) {
      if (api.computedValue(p, 'position') !== 'static' || api.tagName(p) === 'body') return wrap(p);
    }
    return document.body;
  }
  click() {
    if (clicking.has(this) || isDisabled(this)) return;
    clicking.add(this);
    try {
      activate(this, new PointerEvent('click', { bubbles: true, cancelable: true, composed: true, view: globalThis, detail: 1 }));
    } finally {
      clicking.delete(this);
    }
  }
  focus() {
    if (focusable(this)) moveFocus(this);
  }
  blur() {
    if (focusedElement() === this) moveFocus(null);
  }
}
tagClass(HTMLElement, 'HTMLElement');
reflect(HTMLElement, 'string', { title: 'title', lang: 'lang', dir: 'dir', accessKey: 'accesskey', nonce: 'nonce', enterKeyHint: 'enterkeyhint', inputMode: 'inputmode' });
reflect(HTMLElement, 'boolean', { hidden: 'hidden', autofocus: 'autofocus', inert: 'inert', draggable: 'draggable', spellcheck: 'spellcheck' });

// What SVG reflects some attributes as: a base value, and an animated one,
// which is the same here, as nothing animates.
const animatedStrings = new WeakMap();
class SVGAnimatedString {
  get baseVal() {
    const { el, name } = animatedStrings.get(this);
    return el.getAttribute(name) ?? '';
  }
  set baseVal(value) {
    const { el, name } = animatedStrings.get(this);
    el.setAttribute(name, String(value));
  }
  get animVal() {
    return this.baseVal;
  }
}
tagClass(SVGAnimatedString, 'SVGAnimatedString');
function animatedString(el, name) {
  const value = Object.create(SVGAnimatedString.prototype);
  animatedStrings.set(value, { el, name });
  return value;
}

class SVGElement extends Element {
  get style() {
    return inlineStyle(this);
  }
  get dataset() {
    return datasetFor(this);
  }
  // SVG's className is an SVGAnimatedString.
  get className() {
    return animatedString(this, 'class');
  }
  get ownerSVGElement() {
    return this.parentElement?.closest('svg') ?? null;
  }
  getBBox() {
    return new DOMRect();
  }
  focus() {
    if (focusable(this)) moveFocus(this);
  }
  blur() {
    if (focusedElement() === this) moveFocus(null);
  }
}
tagClass(SVGElement, 'SVGElement');
