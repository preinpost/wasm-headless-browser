// Part 2: the node tree. `Node`, character data, fragments, insertion and
// removal, the ParentNode and ChildNode mixins, and `MutationObserver`.

const illegalConstructor = () => {
  throw new TypeError('Illegal constructor');
};

class Node extends EventTarget {
  constructor() {
    illegalConstructor();
  }
  get nodeType() {
    return api.nodeType(own(this));
  }
  get nodeName() {
    return api.nodeName(own(this));
  }
  get nodeValue() {
    const type = this.nodeType;
    return type === 3 || type === 8 ? api.data(own(this)) : null;
  }
  set nodeValue(value) {
    const type = this.nodeType;
    if (type === 3 || type === 8) setData(this, value);
  }
  get ownerDocument() {
    return own(this) === documentId ? null : document;
  }
  get parentNode() {
    return wrap(api.parent(own(this)));
  }
  get parentElement() {
    const parent = api.parent(own(this));
    return parent !== undefined && isElementId(parent) ? wrap(parent) : null;
  }
  get childNodes() {
    return nodeList(wrapAll(api.children(own(this))));
  }
  get firstChild() {
    return wrap(api.children(own(this))[0]);
  }
  get lastChild() {
    const kids = api.children(own(this));
    return wrap(kids[kids.length - 1]);
  }
  get previousSibling() {
    return wrap(api.sibling(own(this), -1));
  }
  get nextSibling() {
    return wrap(api.sibling(own(this), 1));
  }
  get isConnected() {
    return api.isConnected(own(this));
  }
  get baseURI() {
    return api.url();
  }
  get textContent() {
    const type = this.nodeType;
    return type === 9 || type === 10 ? null : api.textContent(own(this));
  }
  set textContent(value) {
    const type = this.nodeType;
    if (type === 3 || type === 8) setData(this, value);
    else if (type !== 9 && type !== 10) replaceText(this, value);
  }
  hasChildNodes() {
    return api.children(own(this)).length > 0;
  }
  getRootNode() {
    let node = this;
    while (node.parentNode) node = node.parentNode;
    return node;
  }
  contains(other) {
    if (other === null || other === undefined) return false;
    const id = nodeIds.get(other);
    return id !== undefined && api.contains(own(this), id);
  }
  isSameNode(other) {
    return this === other;
  }
  isEqualNode(other) {
    return (
      other !== null &&
      nodeIds.has(other) &&
      this.nodeType === other.nodeType &&
      api.outerHTML(own(this)) === api.outerHTML(own(other))
    );
  }
  compareDocumentPosition(other) {
    const a = own(this);
    const b = requireNode(other, 'compareDocumentPosition');
    if (a === b) return 0;
    if (api.contains(a, b)) return 20; // CONTAINED_BY | FOLLOWING
    if (api.contains(b, a)) return 10; // CONTAINS | PRECEDING
    const pa = ancestry(a);
    const pb = ancestry(b);
    if (pa[0] !== pb[0]) return 1 | 32 | (a < b ? 4 : 2); // DISCONNECTED | IMPLEMENTATION_SPECIFIC
    let i = 0;
    while (pa[i] === pb[i]) i++;
    const siblings = api.children(pa[i - 1]);
    return siblings.indexOf(pa[i]) < siblings.indexOf(pb[i]) ? 4 : 2;
  }
  cloneNode(deep) {
    return wrap(api.cloneNode(own(this), !!deep));
  }
  normalize() {}
  lookupNamespaceURI() {
    return null;
  }
  lookupPrefix() {
    return null;
  }
  isDefaultNamespace(namespace) {
    return namespace === null || namespace === '';
  }
  appendChild(child) {
    return insertNode(this, child, null, 'appendChild');
  }
  insertBefore(child, reference) {
    return insertNode(this, child, reference, 'insertBefore');
  }
  removeChild(child) {
    const id = requireNode(child, 'removeChild');
    if (api.parent(id) !== own(this)) {
      throw domError('NotFoundError', "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.");
    }
    removeNode(child);
    return child;
  }
  replaceChild(newChild, oldChild) {
    const id = requireNode(oldChild, 'replaceChild');
    if (api.parent(id) !== own(this)) {
      throw domError('NotFoundError', "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.");
    }
    if (newChild !== oldChild) {
      insertNode(this, newChild, oldChild, 'replaceChild');
      removeNode(oldChild);
    }
    return oldChild;
  }
}
tagClass(Node, 'Node');
const NODE_CONSTANTS = {
  ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, CDATA_SECTION_NODE: 4, PROCESSING_INSTRUCTION_NODE: 7,
  COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_TYPE_NODE: 10, DOCUMENT_FRAGMENT_NODE: 11,
  DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4,
  DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32,
};
for (const [name, value] of Object.entries(NODE_CONSTANTS)) {
  Object.defineProperty(Node, name, { value });
  Object.defineProperty(Node.prototype, name, { value });
}

// Root first, `id` last.
function ancestry(id) {
  const out = [];
  for (let n = id; n !== undefined; n = api.parent(n)) out.unshift(n);
  return out;
}

class CharacterData extends Node {
  get data() {
    return api.data(own(this));
  }
  set data(value) {
    setData(this, value);
  }
  get length() {
    return this.data.length;
  }
  appendData(text) {
    this.data += String(text);
  }
  insertData(offset, text) {
    const d = this.data;
    this.data = d.slice(0, offset) + String(text) + d.slice(offset);
  }
  deleteData(offset, count) {
    const d = this.data;
    this.data = d.slice(0, offset) + d.slice(offset + count);
  }
  replaceData(offset, count, text) {
    const d = this.data;
    this.data = d.slice(0, offset) + String(text) + d.slice(offset + count);
  }
  substringData(offset, count) {
    return this.data.substr(offset, count);
  }
}
tagClass(CharacterData, 'CharacterData');

class Text extends CharacterData {
  constructor(data = '') {
    return wrap(api.createTextNode(String(data)));
  }
  get wholeText() {
    return this.data;
  }
  splitText(offset) {
    const d = this.data;
    const rest = document.createTextNode(d.slice(offset));
    this.data = d.slice(0, offset);
    if (this.parentNode) this.parentNode.insertBefore(rest, this.nextSibling);
    return rest;
  }
}
tagClass(Text, 'Text');

class Comment extends CharacterData {
  constructor(data = '') {
    return wrap(api.createComment(String(data)));
  }
}
tagClass(Comment, 'Comment');

class DocumentType extends Node {
  get name() {
    return this.nodeName;
  }
  get publicId() {
    return '';
  }
  get systemId() {
    return '';
  }
}
tagClass(DocumentType, 'DocumentType');

class DocumentFragment extends Node {
  constructor() {
    return wrap(api.createFragment());
  }
  getElementById(value) {
    const id = api.query(own(this), `[id="${cssEscape(String(value))}"]`);
    return wrap(id);
  }
}
tagClass(DocumentFragment, 'DocumentFragment');

class ShadowRoot extends DocumentFragment {
  get host() {
    return shadowHosts.get(this) ?? null;
  }
  get mode() {
    return 'open';
  }
}
tagClass(ShadowRoot, 'ShadowRoot');

// ---- mutation ----------------------------------------------------------------

function setData(node, value) {
  const id = own(node);
  const oldValue = api.data(id);
  api.setTextContent(id, value === null || value === undefined ? '' : String(value));
  queueRecord('characterData', id, { oldValue });
  const parent = api.parent(id);
  if (parent !== undefined && api.tagName(parent) === 'script') prepareScript(parent);
}

function replaceText(node, value) {
  const id = own(node);
  const removed = wrapAll(api.children(id));
  api.setTextContent(id, value === null || value === undefined ? '' : String(value));
  queueRecord('childList', id, { addedNodes: wrapAll(api.children(id)), removedNodes: removed });
  if (api.tagName(id) === 'script') prepareScript(id);
}

function insertNode(parent, child, reference, method) {
  const parentId = own(parent);
  const childId = requireNode(child, method);
  const referenceId = reference === null || reference === undefined ? undefined : requireNode(reference, method);
  if (referenceId !== undefined && api.parent(referenceId) !== parentId) {
    throw domError('NotFoundError', `Failed to execute '${method}' on 'Node': The node before which the new node is to be inserted is not a child of this node.`);
  }
  const moved = api.nodeType(childId) === 11 ? api.children(childId) : [childId];
  const oldParent = api.parent(childId);
  if (oldParent !== undefined && api.nodeType(childId) !== 11) {
    queueRecord('childList', oldParent, { removedNodes: [child] });
  }
  if (!api.insertChild(parentId, childId, referenceId)) {
    throw domError('HierarchyRequestError', `Failed to execute '${method}' on 'Node': The new child element contains the parent.`);
  }
  queueRecord('childList', parentId, { addedNodes: wrapAll(moved), nextSibling: reference ?? null });
  for (const id of moved) nodeInserted(id);
  if (api.tagName(parentId) === 'script') prepareScript(parentId);
  return child;
}

function removeNode(node) {
  const id = own(node);
  const parent = api.parent(id);
  if (parent === undefined) return;
  const previousSibling = wrap(api.sibling(id, -1));
  const nextSibling = wrap(api.sibling(id, 1));
  api.removeNode(id);
  queueRecord('childList', parent, { removedNodes: [node], previousSibling, nextSibling });
  const focused = api.activeElement();
  if (focused !== undefined && api.contains(id, focused)) api.setFocus(undefined);
}

// What becoming connected sets in motion: scripts run, stylesheets load.
function nodeInserted(id) {
  if (!api.isConnected(id) || !isElementId(id)) return;
  const scripts = api.tagName(id) === 'script' ? [id] : api.byTag(id, 'script');
  for (const script of scripts) prepareScript(script);
  const links = api.tagName(id) === 'link' ? [id] : api.byTag(id, 'link');
  for (const link of links) linkInserted(link);
  const images = api.tagName(id) === 'img' ? [id] : api.byTag(id, 'img');
  for (const image of images) startImage(image);
}

// A selector a method was given, checked as a browser checks it.
function checkedSelector(selector, method) {
  const text = String(selector);
  if (!api.validSelector(text)) {
    throw domError('SyntaxError', `Failed to execute '${method}': '${text}' is not a valid selector.`);
  }
  return text;
}

// Strings become text nodes; several nodes become one fragment.
function toNode(nodes) {
  const converted = nodes.map((n) => (nodeIds.has(n) ? n : document.createTextNode(String(n))));
  if (converted.length === 1) return converted[0];
  const fragment = document.createDocumentFragment();
  for (const n of converted) fragment.appendChild(n);
  return fragment;
}

const ParentNode = {
  get children() {
    return htmlCollection(wrapAll(api.children(own(this)).filter(isElementId)));
  },
  get childElementCount() {
    return api.children(own(this)).filter(isElementId).length;
  },
  get firstElementChild() {
    return wrap(api.children(own(this)).find(isElementId));
  },
  get lastElementChild() {
    const kids = api.children(own(this)).filter(isElementId);
    return wrap(kids[kids.length - 1]);
  },
  append(...nodes) {
    if (nodes.length) this.appendChild(toNode(nodes));
  },
  prepend(...nodes) {
    if (nodes.length) this.insertBefore(toNode(nodes), this.firstChild);
  },
  replaceChildren(...nodes) {
    for (const child of wrapAll(api.children(own(this)))) removeNode(child);
    if (nodes.length) this.appendChild(toNode(nodes));
  },
  querySelector(selector) {
    return wrap(api.query(own(this), checkedSelector(selector, 'querySelector')));
  },
  querySelectorAll(selector) {
    return nodeList(wrapAll(api.queryAll(own(this), checkedSelector(selector, 'querySelectorAll'))));
  },
  getElementsByTagName(tag) {
    return htmlCollection(wrapAll(api.byTag(own(this), String(tag))));
  },
  getElementsByTagNameNS(namespace, tag) {
    return this.getElementsByTagName(tag);
  },
  getElementsByClassName(names) {
    const selector = String(names).trim().split(/\s+/).filter(Boolean).map((n) => '.' + cssEscape(n)).join('');
    return htmlCollection(selector ? wrapAll(api.queryAll(own(this), selector)) : []);
  },
};

const ChildNode = {
  before(...nodes) {
    const parent = this.parentNode;
    if (parent && nodes.length) parent.insertBefore(toNode(nodes), this);
  },
  after(...nodes) {
    const parent = this.parentNode;
    if (parent && nodes.length) parent.insertBefore(toNode(nodes), this.nextSibling);
  },
  replaceWith(...nodes) {
    const parent = this.parentNode;
    if (!parent) return;
    if (nodes.length) parent.insertBefore(toNode(nodes), this);
    removeNode(this);
  },
  remove() {
    removeNode(this);
  },
};

function mixin(C, source) {
  Object.defineProperties(C.prototype, Object.getOwnPropertyDescriptors(source));
}
mixin(DocumentFragment, ParentNode);
mixin(CharacterData, ChildNode);
mixin(DocumentType, ChildNode);

// `CSS.escape`, which selectors built from ids and class names need.
function cssEscape(value) {
  return String(value).replace(/([\0-\x1f\x7f]|^-?\d)|^-$|[^\w-\u0080-\uffff]/g, (ch, control) =>
    control ? '\\' + ch.codePointAt(ch.length - 1).toString(16) + ' ' : '\\' + ch,
  );
}

// ---- MutationObserver --------------------------------------------------------
//
// Records are queued as mutations happen and delivered in a microtask, as in
// a browser. Only mutations made from script are seen; the parser runs before
// any observer exists.

const registrations = [];
const pendingObservers = new Set();
const observerState = new WeakMap();

class MutationRecord {}
tagClass(MutationRecord, 'MutationRecord');

class MutationObserver {
  constructor(callback) {
    if (typeof callback !== 'function') {
      throw new TypeError("Failed to construct 'MutationObserver': parameter 1 is not of type 'MutationCallback'.");
    }
    observerState.set(this, { callback, records: [] });
  }
  observe(target, options = {}) {
    const id = requireNode(target, 'observe');
    const opts = { ...options };
    if (opts.attributeFilter || opts.attributeOldValue) opts.attributes = opts.attributes ?? true;
    if (opts.characterDataOldValue) opts.characterData = opts.characterData ?? true;
    const existing = registrations.find((r) => r.observer === this && r.target === id);
    if (existing) existing.options = opts;
    else registrations.push({ observer: this, target: id, options: opts });
  }
  disconnect() {
    for (let i = registrations.length - 1; i >= 0; i--) {
      if (registrations[i].observer === this) registrations.splice(i, 1);
    }
    observerState.get(this).records = [];
  }
  takeRecords() {
    const state = observerState.get(this);
    const records = state.records;
    state.records = [];
    return records;
  }
}
tagClass(MutationObserver, 'MutationObserver');

function queueRecord(type, targetId, details) {
  if (registrations.length === 0) return;
  for (const { observer, target, options } of registrations) {
    if (target !== targetId && !(options.subtree && api.contains(target, targetId))) continue;
    if (type === 'childList' && !options.childList) continue;
    if (type === 'characterData' && !options.characterData) continue;
    if (type === 'attributes') {
      if (!options.attributes) continue;
      if (options.attributeFilter && !options.attributeFilter.includes(details.attributeName)) continue;
    }
    const wantsOld = type === 'attributes' ? options.attributeOldValue : options.characterDataOldValue;
    const record = Object.assign(Object.create(MutationRecord.prototype), {
      type,
      target: wrap(targetId),
      addedNodes: nodeList(details.addedNodes || []),
      removedNodes: nodeList(details.removedNodes || []),
      previousSibling: details.previousSibling ?? null,
      nextSibling: details.nextSibling ?? null,
      attributeName: details.attributeName ?? null,
      attributeNamespace: null,
      oldValue: wantsOld ? details.oldValue ?? null : null,
    });
    observerState.get(observer).records.push(record);
    if (pendingObservers.size === 0) Promise.resolve().then(deliverMutations);
    pendingObservers.add(observer);
  }
}

function deliverMutations() {
  const observers = Array.from(pendingObservers);
  pendingObservers.clear();
  for (const observer of observers) {
    const records = observer.takeRecords();
    if (records.length === 0) continue;
    try {
      observerState.get(observer).callback.call(observer, records, observer);
    } catch (e) {
      reportError(e);
    }
  }
}
