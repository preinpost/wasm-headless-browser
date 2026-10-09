// Part 5: the document. `Document`, ranges and selection, tree walkers, and
// the choice of prototype for each node.

let readyState = api.url() === '' || api.url() === 'about:blank' ? 'complete' : 'loading';
let currentScript = null;

class Document extends Node {
  constructor() {
    illegalConstructor();
  }
  get documentElement() {
    return wrap(api.documentElement());
  }
  get head() {
    return wrap(api.head());
  }
  get body() {
    return wrap(api.body());
  }
  get title() {
    return api.title();
  }
  set title(value) {
    api.setTitle(String(value));
  }
  get URL() {
    return api.url();
  }
  get documentURI() {
    return api.url();
  }
  get location() {
    return location;
  }
  set location(value) {
    location.href = value;
  }
  get domain() {
    return location.hostname;
  }
  set domain(value) {}
  get referrer() {
    return '';
  }
  get cookie() {
    return api.cookie();
  }
  set cookie(value) {
    api.setCookie(String(value));
  }
  get readyState() {
    return readyState;
  }
  get currentScript() {
    return currentScript;
  }
  get activeElement() {
    return focusedElement() ?? this.body;
  }
  get defaultView() {
    return globalThis;
  }
  get compatMode() {
    return 'CSS1Compat';
  }
  get characterSet() {
    return 'UTF-8';
  }
  get charset() {
    return 'UTF-8';
  }
  get inputEncoding() {
    return 'UTF-8';
  }
  get contentType() {
    return 'text/html';
  }
  get visibilityState() {
    return 'visible';
  }
  get hidden() {
    return false;
  }
  get doctype() {
    return wrap(api.children(documentId).find((id) => api.nodeType(id) === 10));
  }
  get forms() {
    return htmlCollection(wrapAll(api.byTag(documentId, 'form')));
  }
  get images() {
    return htmlCollection(wrapAll(api.byTag(documentId, 'img')));
  }
  get scripts() {
    return htmlCollection(wrapAll(api.byTag(documentId, 'script')));
  }
  get links() {
    return htmlCollection(wrapAll(api.byTag(documentId, '*').filter((id) => ['a', 'area'].includes(api.tagName(id)) && api.hasAttr(id, 'href'))));
  }
  get anchors() {
    return htmlCollection(wrapAll(api.byTag(documentId, 'a').filter((id) => api.hasAttr(id, 'name'))));
  }
  get embeds() {
    return htmlCollection(wrapAll(api.byTag(documentId, 'embed')));
  }
  get plugins() {
    return this.embeds;
  }
  get styleSheets() {
    return documentStyleSheets();
  }
  get scrollingElement() {
    return this.documentElement;
  }
  get implementation() {
    return implementation;
  }
  get fonts() {
    return fonts;
  }
  get fullscreenElement() {
    return null;
  }
  get fullscreenEnabled() {
    return false;
  }
  get pointerLockElement() {
    return null;
  }
  hasFocus() {
    return true;
  }
  getElementById(value) {
    return wrap(api.getElementById(String(value)));
  }
  getElementsByName(name) {
    return nodeList(wrapAll(api.byTag(documentId, '*').filter((id) => api.attr(id, 'name') === String(name))));
  }
  createElement(tag) {
    tag = String(tag);
    if (!/^[A-Za-z][^\s/>\0]*$/.test(tag)) {
      throw domError('InvalidCharacterError', `Failed to execute 'createElement' on 'Document': The tag name provided ('${tag}') is not a valid name.`);
    }
    return wrap(api.createElement(tag));
  }
  createElementNS(namespace, qualifiedName) {
    return wrap(api.createElementNS(namespace === null || namespace === undefined ? '' : String(namespace), String(qualifiedName)));
  }
  createTextNode(text) {
    return wrap(api.createTextNode(String(text)));
  }
  createComment(text) {
    return wrap(api.createComment(String(text)));
  }
  createDocumentFragment() {
    return wrap(api.createFragment());
  }
  createEvent(kind) {
    const C = {
      event: Event, events: Event, htmlevents: Event, customevent: CustomEvent, uievent: UIEvent, uievents: UIEvent,
      mouseevent: MouseEvent, mouseevents: MouseEvent, keyboardevent: KeyboardEvent, focusevent: FocusEvent,
      messageevent: MessageEvent, errorevent: ErrorEvent, hashchangeevent: HashChangeEvent, popstateevent: PopStateEvent,
    }[String(kind).toLowerCase()];
    if (!C) throw domError('NotSupportedError', `The provided event type ('${kind}') is invalid.`);
    return new C('');
  }
  createRange() {
    return new Range();
  }
  createTreeWalker(root, whatToShow = 0xffffffff, filter = null) {
    return new TreeWalker(root, whatToShow, filter);
  }
  createNodeIterator(root, whatToShow = 0xffffffff, filter = null) {
    return new TreeWalker(root, whatToShow, filter);
  }
  importNode(node, deep) {
    return node.cloneNode(deep);
  }
  adoptNode(node) {
    if (node.parentNode) removeNode(node);
    return node;
  }
  // The engine's hit test, which is what a click at the point would land
  // on. Points outside the viewport have nothing, as the specification says.
  elementFromPoint(x, y) {
    return this.elementsFromPoint(x, y)[0] ?? null;
  }
  elementsFromPoint(x, y) {
    x = Number(x);
    y = Number(y);
    if (!(x >= 0 && y >= 0 && x < api.viewportWidth() && y < api.viewportHeight())) return [];
    return wrapAll(api.hitTest(x, y));
  }
  caretRangeFromPoint() {
    return null;
  }
  getSelection() {
    return selection;
  }
  write(...parts) {
    documentWrite(parts.join(''));
  }
  writeln(...parts) {
    documentWrite(parts.join('') + '\n');
  }
  open() {
    return this;
  }
  close() {}
  execCommand() {
    return false;
  }
  queryCommandSupported() {
    return false;
  }
  queryCommandEnabled() {
    return false;
  }
  exitFullscreen() {
    return Promise.resolve();
  }
}
tagClass(Document, 'Document');
mixin(Document, ParentNode);
class HTMLDocument extends Document {}
tagClass(HTMLDocument, 'HTMLDocument');

// `document.write` during loading inserts after the running script, which is
// roughly where the parser would have put it; afterwards it appends to the
// body rather than replacing the document. Written scripts run, unlike
// `innerHTML`'s.
function documentWrite(html) {
  const anchor = currentScript && currentScript.parentNode ? currentScript : null;
  const parent = anchor ? anchor.parentNode : document.body ?? document.documentElement;
  if (!parent) return;
  const context = parent.nodeType === 1 ? own(parent) : documentId;
  const fragment = parseMarkup(context, html, true);
  parent.insertBefore(fragment, anchor ? anchor.nextSibling : null);
}

// A document that is not the page: what `createHTMLDocument` and `DOMParser`
// return. It lives in the same arena, under a fragment that stands in for
// its document node and is never connected to the page, so its scripts do
// not run and its links do not load, as in a browser's inert document. Its
// nodes can be moved into the page.
function inertDocument(markup, title) {
  const rootId = api.createFragment();
  const doc = Object.create(HTMLDocument.prototype);
  nodeIds.set(doc, rootId);
  wrappers.set(rootId, doc);
  const html = document.createElement('html');
  doc.appendChild(html);
  if (markup !== undefined) html.appendChild(parseMarkup(own(html), markup, false));
  if (!html.querySelector('head')) html.insertBefore(document.createElement('head'), html.firstChild);
  if (!html.querySelector('body')) html.appendChild(document.createElement('body'));
  const find = (tag) => wrap(api.byTag(rootId, tag)[0]);
  if (title !== undefined) {
    const t = document.createElement('title');
    t.textContent = String(title);
    find('head').appendChild(t);
  }
  Object.defineProperties(doc, {
    nodeType: { value: 9 },
    nodeName: { value: '#document' },
    ownerDocument: { value: null },
    documentElement: { get: () => find('html') },
    head: { get: () => find('head') },
    body: { get: () => find('body') },
    title: {
      get: () => (find('title')?.textContent ?? '').trim(),
      set: (v) => {
        let t = find('title');
        if (!t) find('head').appendChild((t = document.createElement('title')));
        t.textContent = String(v);
      },
    },
    getElementById: { value: (id) => wrap(api.byTag(rootId, '*').find((n) => api.attr(n, 'id') === String(id))) },
    URL: { value: 'about:blank' },
    documentURI: { value: 'about:blank' },
    readyState: { value: 'complete' },
    defaultView: { value: null },
    location: { value: null },
    activeElement: { get: () => null },
    cookie: { get: () => '', set() {} },
  });
  return doc;
}

const implementation = {
  hasFeature: () => true,
  createHTMLDocument: (title) => inertDocument(undefined, title),
};

const fonts = {
  ready: Promise.resolve(),
  status: 'loaded',
  load: () => Promise.resolve([]),
  check: () => true,
  add() {},
  delete: () => false,
  forEach() {},
  addEventListener() {},
  removeEventListener() {},
};

// ---- ranges and selection ---------------------------------------------------
//
// Enough of a range for code that builds fragments or measures text. Without
// layout, measurements are zero.

class Range {
  constructor() {
    this.startContainer = document;
    this.startOffset = 0;
    this.endContainer = document;
    this.endOffset = 0;
  }
  get collapsed() {
    return this.startContainer === this.endContainer && this.startOffset === this.endOffset;
  }
  get commonAncestorContainer() {
    let node = this.startContainer;
    while (node && !node.contains(this.endContainer)) node = node.parentNode;
    return node ?? document;
  }
  setStart(node, offset) {
    this.startContainer = node;
    this.startOffset = offset;
  }
  setEnd(node, offset) {
    this.endContainer = node;
    this.endOffset = offset;
  }
  setStartBefore(node) {
    this.setStart(node.parentNode, Array.from(node.parentNode.childNodes).indexOf(node));
  }
  setStartAfter(node) {
    this.setStart(node.parentNode, Array.from(node.parentNode.childNodes).indexOf(node) + 1);
  }
  setEndBefore(node) {
    this.setEnd(node.parentNode, Array.from(node.parentNode.childNodes).indexOf(node));
  }
  setEndAfter(node) {
    this.setEnd(node.parentNode, Array.from(node.parentNode.childNodes).indexOf(node) + 1);
  }
  selectNode(node) {
    this.setStartBefore(node);
    this.setEndAfter(node);
  }
  selectNodeContents(node) {
    this.setStart(node, 0);
    this.setEnd(node, node.nodeType === 3 || node.nodeType === 8 ? node.data.length : node.childNodes.length);
  }
  collapse(toStart) {
    if (toStart) this.setEnd(this.startContainer, this.startOffset);
    else this.setStart(this.endContainer, this.endOffset);
  }
  cloneRange() {
    const r = new Range();
    r.setStart(this.startContainer, this.startOffset);
    r.setEnd(this.endContainer, this.endOffset);
    return r;
  }
  // Scripts in a contextual fragment run when it is inserted, unlike
  // `innerHTML`'s.
  createContextualFragment(html) {
    const node = this.startContainer;
    const context = node && node.nodeType === 1 ? own(node) : api.body() ?? documentId;
    return parseMarkup(context, html, true);
  }
  insertNode(node) {
    const container = this.startContainer;
    if (container.nodeType === 3) container.parentNode.insertBefore(node, container.nextSibling);
    else container.insertBefore(node, container.childNodes[this.startOffset] ?? null);
  }
  toString() {
    const c = this.commonAncestorContainer;
    return c && c.nodeType !== 9 ? c.textContent : '';
  }
  getBoundingClientRect() {
    return new DOMRect();
  }
  getClientRects() {
    return [];
  }
  detach() {}
}
tagClass(Range, 'Range');

const selection = {
  anchorNode: null,
  anchorOffset: 0,
  focusNode: null,
  focusOffset: 0,
  isCollapsed: true,
  rangeCount: 0,
  type: 'None',
  addRange() {},
  removeAllRanges() {},
  removeRange() {},
  empty() {},
  collapse() {},
  collapseToEnd() {},
  collapseToStart() {},
  extend() {},
  selectAllChildren() {},
  containsNode: () => false,
  getRangeAt(index) {
    throw domError('IndexSizeError', `${index} is not a valid index.`);
  },
  toString: () => '',
};

// ---- tree walking -----------------------------------------------------------

const NodeFilter = {
  FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
  SHOW_ALL: 0xffffffff, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8,
  SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200,
  SHOW_DOCUMENT_FRAGMENT: 0x400,
};

// A TreeWalker over a pre-order snapshot of the subtree, which also serves as
// a NodeIterator. `FILTER_REJECT` skips the node's children, as a walker does.
class TreeWalker {
  constructor(root, whatToShow, filter) {
    this.root = root;
    this.whatToShow = whatToShow >>> 0;
    this.filter = filter;
    this.currentNode = root;
    this.referenceNode = root;
  }
  accept(node) {
    if (!(this.whatToShow & (1 << (node.nodeType - 1)))) return NodeFilter.FILTER_SKIP;
    if (!this.filter) return NodeFilter.FILTER_ACCEPT;
    const fn = typeof this.filter === 'function' ? this.filter : this.filter.acceptNode.bind(this.filter);
    return fn(node);
  }
  order() {
    const out = [];
    const visit = (id, isRoot) => {
      const node = wrap(id);
      const verdict = isRoot ? NodeFilter.FILTER_SKIP : this.accept(node);
      if (verdict === NodeFilter.FILTER_ACCEPT) out.push(node);
      if (verdict !== NodeFilter.FILTER_REJECT) for (const child of api.children(id)) visit(child, false);
    };
    visit(own(this.root), true);
    return out;
  }
  nextNode() {
    const order = this.order();
    const next = this.currentNode === this.root ? order[0] : order[order.indexOf(this.currentNode) + 1];
    if (next) this.currentNode = this.referenceNode = next;
    return next ?? null;
  }
  previousNode() {
    const order = this.order();
    const i = order.indexOf(this.currentNode);
    const prev = i > 0 ? order[i - 1] : null;
    if (prev) this.currentNode = this.referenceNode = prev;
    return prev;
  }
  parentNode() {
    for (let n = this.currentNode.parentNode; n && n !== this.root.parentNode; n = n.parentNode) {
      if (n === this.root) return null;
      if (this.accept(n) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n);
    }
    return null;
  }
  firstChild() {
    const child = Array.from(this.currentNode.childNodes).find((c) => this.accept(c) === NodeFilter.FILTER_ACCEPT);
    if (child) this.currentNode = child;
    return child ?? null;
  }
  lastChild() {
    const child = Array.from(this.currentNode.childNodes).reverse().find((c) => this.accept(c) === NodeFilter.FILTER_ACCEPT);
    if (child) this.currentNode = child;
    return child ?? null;
  }
  nextSibling() {
    for (let n = this.currentNode.nextSibling; n; n = n.nextSibling) {
      if (this.accept(n) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n);
    }
    return null;
  }
  previousSibling() {
    for (let n = this.currentNode.previousSibling; n; n = n.previousSibling) {
      if (this.accept(n) === NodeFilter.FILTER_ACCEPT) return (this.currentNode = n);
    }
    return null;
  }
  detach() {}
}
tagClass(TreeWalker, 'TreeWalker');

// ---- prototypes ---------------------------------------------------------------

function protoFor(id) {
  switch (api.nodeType(id)) {
    case 1:
      return elementInterface(api.interfaceName(id)).prototype;
    case 3:
      return Text.prototype;
    case 8:
      return Comment.prototype;
    case 9:
      return HTMLDocument.prototype;
    case 10:
      return DocumentType.prototype;
    case 11:
      return DocumentFragment.prototype;
  }
  return Node.prototype;
}

const document = wrap(documentId);
