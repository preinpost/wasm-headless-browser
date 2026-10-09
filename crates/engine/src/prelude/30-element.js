// Part 3: elements. Attributes, `classList`, `dataset`, inline `style`, markup
// in and out, geometry, focus and clicking.

function setAttr(el, name, value) {
  const id = own(el);
  name = String(name).toLowerCase();
  const oldValue = api.attr(id, name) ?? null;
  api.setAttr(id, name, String(value));
  queueRecord('attributes', id, { attributeName: name, oldValue });
  if (name === 'src' && api.tagName(id) === 'script') prepareScript(id);
}

function removeAttr(el, name) {
  const id = own(el);
  name = String(name).toLowerCase();
  if (!api.hasAttr(id, name)) return;
  const oldValue = api.attr(id, name) ?? null;
  api.removeAttr(id, name);
  queueRecord('attributes', id, { attributeName: name, oldValue });
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
const scrollPositions = new WeakMap();

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
    list.getNamedItem = (n) => (this.hasAttribute(n) ? attrNode(this, String(n).toLowerCase()) : null);
    list.item = (i) => list[i] ?? null;
    return list;
  }
  getAttribute(name) {
    return api.attr(own(this), String(name)) ?? null;
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
    return api.hasAttr(own(this), String(name));
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
    return this.hasAttribute(name) ? attrNode(this, String(name).toLowerCase()) : null;
  }
  toggleAttribute(name, force) {
    const has = this.hasAttribute(name);
    const want = force === undefined ? !has : !!force;
    if (want && !has) this.setAttribute(name, '');
    if (!want && has) this.removeAttribute(name);
    return want;
  }
  matches(selector) {
    return api.matches(own(this), String(selector));
  }
  webkitMatchesSelector(selector) {
    return this.matches(selector);
  }
  closest(selector) {
    for (let id = own(this); id !== undefined; id = api.parent(id)) {
      if (isElementId(id) && api.matches(id, String(selector))) return wrap(id);
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
    return boxOf(this).width;
  }
  get clientHeight() {
    return boxOf(this).height;
  }
  get clientTop() {
    return 0;
  }
  get clientLeft() {
    return 0;
  }
  get scrollWidth() {
    return boxOf(this).width;
  }
  get scrollHeight() {
    return boxOf(this).height;
  }
  get scrollTop() {
    return scrollPositions.get(this)?.top ?? 0;
  }
  set scrollTop(value) {
    scrollPositions.set(this, { left: this.scrollLeft, top: Number(value) || 0 });
  }
  get scrollLeft() {
    return scrollPositions.get(this)?.left ?? 0;
  }
  set scrollLeft(value) {
    scrollPositions.set(this, { left: Number(value) || 0, top: this.scrollTop });
  }
  scroll() {}
  scrollTo() {}
  scrollBy() {}
  scrollIntoView() {}
  scrollIntoViewIfNeeded() {}
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
    return api.textContent(own(this));
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
    return boxOf(this).top;
  }
  get offsetLeft() {
    return boxOf(this).left;
  }
  // No layout, so no element is rendered, and an unrendered element has no
  // offset parent. jQuery's `:visible` reads this.
  get offsetParent() {
    return null;
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

class SVGElement extends Element {
  get style() {
    return inlineStyle(this);
  }
  get dataset() {
    return datasetFor(this);
  }
  // SVG's className is an SVGAnimatedString.
  get className() {
    const el = this;
    return {
      get baseVal() {
        return el.getAttribute('class') ?? '';
      },
      set baseVal(v) {
        el.setAttribute('class', String(v));
      },
      get animVal() {
        return el.getAttribute('class') ?? '';
      },
    };
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
