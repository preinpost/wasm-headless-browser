// Part 1: events. `Event` and its subclasses, `EventTarget`, dispatch through
// the capture, target and bubble phases, and `on*` handler properties,
// including inline attributes like `onclick="..."`.

const NONE = 0;
const CAPTURING_PHASE = 1;
const AT_TARGET = 2;
const BUBBLING_PHASE = 3;

// Dispatch flags that page code must not see as properties.
const eventFlags = new WeakMap();
function flags(event) {
  let f = eventFlags.get(event);
  if (!f) {
    f = { stop: false, stopImmediate: false, passive: false, dispatching: false, path: [] };
    eventFlags.set(event, f);
  }
  return f;
}

function tagClass(C, name) {
  Object.defineProperty(C, 'name', { value: name });
  Object.defineProperty(C.prototype, Symbol.toStringTag, { value: name, configurable: true });
  return C;
}

class Event {
  constructor(type, init) {
    if (arguments.length === 0) {
      throw new TypeError("Failed to construct 'Event': 1 argument required, but only 0 present.");
    }
    init = init || {};
    this.type = String(type);
    this.bubbles = !!init.bubbles;
    this.cancelable = !!init.cancelable;
    this.composed = !!init.composed;
    this.defaultPrevented = false;
    this.isTrusted = false;
    this.target = null;
    this.currentTarget = null;
    this.eventPhase = NONE;
    this.timeStamp = nowMs();
    flags(this);
  }
  get srcElement() {
    return this.target;
  }
  get returnValue() {
    return !this.defaultPrevented;
  }
  set returnValue(value) {
    if (!value) this.preventDefault();
  }
  get cancelBubble() {
    return flags(this).stop;
  }
  set cancelBubble(value) {
    if (value) flags(this).stop = true;
  }
  preventDefault() {
    if (this.cancelable && !flags(this).passive) this.defaultPrevented = true;
  }
  stopPropagation() {
    flags(this).stop = true;
  }
  stopImmediatePropagation() {
    const f = flags(this);
    f.stop = true;
    f.stopImmediate = true;
  }
  composedPath() {
    return flags(this).path.slice();
  }
  initEvent(type, bubbles, cancelable) {
    this.type = String(type);
    this.bubbles = !!bubbles;
    this.cancelable = !!cancelable;
  }
}
tagClass(Event, 'Event');
for (const [name, value] of Object.entries({ NONE, CAPTURING_PHASE, AT_TARGET, BUBBLING_PHASE })) {
  Object.defineProperty(Event, name, { value });
  Object.defineProperty(Event.prototype, name, { value });
}

// An event interface whose extra members are plain fields taken from the
// init dictionary, with these defaults.
function eventInterface(name, Parent, defaults) {
  const C = class extends Parent {
    constructor(type, init) {
      super(type, init);
      init = init || {};
      for (const key of Object.keys(defaults)) {
        this[key] = init[key] !== undefined ? init[key] : defaults[key];
      }
    }
  };
  return tagClass(C, name);
}

const modifiers = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
const UIEvent = eventInterface('UIEvent', Event, { view: null, detail: 0, which: 0 });
const MouseEvent = eventInterface('MouseEvent', UIEvent, {
  ...modifiers,
  screenX: 0,
  screenY: 0,
  clientX: 0,
  clientY: 0,
  movementX: 0,
  movementY: 0,
  button: 0,
  buttons: 0,
  relatedTarget: null,
});
for (const [alias, field] of [['x', 'clientX'], ['y', 'clientY'], ['pageX', 'clientX'], ['pageY', 'clientY'], ['offsetX', 'clientX'], ['offsetY', 'clientY']]) {
  Object.defineProperty(MouseEvent.prototype, alias, {
    get() {
      return this[field];
    },
    configurable: true,
  });
}
MouseEvent.prototype.getModifierState = function (key) {
  return !!{ Control: this.ctrlKey, Shift: this.shiftKey, Alt: this.altKey, Meta: this.metaKey }[key];
};
const PointerEvent = eventInterface('PointerEvent', MouseEvent, {
  pointerId: 1,
  pointerType: 'mouse',
  isPrimary: true,
  width: 1,
  height: 1,
  pressure: 0,
});
const WheelEvent = eventInterface('WheelEvent', MouseEvent, { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 });
const KeyboardEvent = eventInterface('KeyboardEvent', UIEvent, {
  ...modifiers,
  key: '',
  code: '',
  location: 0,
  repeat: false,
  isComposing: false,
  charCode: 0,
  keyCode: 0,
});
KeyboardEvent.prototype.getModifierState = MouseEvent.prototype.getModifierState;
const FocusEvent = eventInterface('FocusEvent', UIEvent, { relatedTarget: null });
const InputEvent = eventInterface('InputEvent', UIEvent, { data: null, inputType: '', isComposing: false, dataTransfer: null });
const CompositionEvent = eventInterface('CompositionEvent', UIEvent, { data: '' });
const CustomEvent = eventInterface('CustomEvent', Event, { detail: null });
CustomEvent.prototype.initCustomEvent = function (type, bubbles, cancelable, detail) {
  this.initEvent(type, bubbles, cancelable);
  this.detail = detail;
};
const ErrorEvent = eventInterface('ErrorEvent', Event, { message: '', filename: '', lineno: 0, colno: 0, error: null });
const ProgressEvent = eventInterface('ProgressEvent', Event, { lengthComputable: false, loaded: 0, total: 0 });
const MessageEvent = eventInterface('MessageEvent', Event, { data: null, origin: '', lastEventId: '', source: null, ports: [] });
const PopStateEvent = eventInterface('PopStateEvent', Event, { state: null });
const HashChangeEvent = eventInterface('HashChangeEvent', Event, { oldURL: '', newURL: '' });
const PageTransitionEvent = eventInterface('PageTransitionEvent', Event, { persisted: false });
const SubmitEvent = eventInterface('SubmitEvent', Event, { submitter: null });
const CloseEvent = eventInterface('CloseEvent', Event, { code: 0, reason: '', wasClean: false });
const StorageEvent = eventInterface('StorageEvent', Event, { key: null, oldValue: null, newValue: null, url: '', storageArea: null });
const PromiseRejectionEvent = eventInterface('PromiseRejectionEvent', Event, { promise: null, reason: undefined });

// ---- EventTarget ------------------------------------------------------------

const listenerMaps = new WeakMap();
const handlerMaps = new WeakMap();
const compiledHandlers = new WeakMap();

// `this` is undefined when a page calls a bare `addEventListener(...)`, which
// in a browser means the window.
const targetOf = (value) => (value === undefined || value === null ? globalThis : value);

class EventTarget {
  addEventListener(type, callback, options) {
    const target = targetOf(this);
    if (callback === null || callback === undefined) return;
    const opts = options !== null && typeof options === 'object' ? options : { capture: !!options };
    if (opts.signal && opts.signal.aborted) return;
    const capture = !!opts.capture;
    type = String(type);
    let byType = listenerMaps.get(target);
    if (!byType) listenerMaps.set(target, (byType = new Map()));
    let list = byType.get(type);
    if (!list) byType.set(type, (list = []));
    if (list.some((l) => l.callback === callback && l.capture === capture)) return;
    list.push({ callback, capture, once: !!opts.once, passive: !!opts.passive, removed: false });
    if (opts.signal) {
      opts.signal.addEventListener('abort', () => target.removeEventListener(type, callback, { capture }));
    }
  }

  removeEventListener(type, callback, options) {
    const target = targetOf(this);
    const capture = options !== null && typeof options === 'object' ? !!options.capture : !!options;
    const list = listenerMaps.get(target)?.get(String(type));
    if (!list) return;
    const index = list.findIndex((l) => l.callback === callback && l.capture === capture);
    if (index >= 0) {
      list[index].removed = true;
      list.splice(index, 1);
    }
  }

  dispatchEvent(event) {
    if (!(event instanceof Event)) {
      throw new TypeError("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'.");
    }
    return dispatch(targetOf(this), event);
  }
}
tagClass(EventTarget, 'EventTarget');

// The next target up the propagation path. Nodes bubble to their parent, the
// document to the window, and the window nowhere. `load` stops at the
// document, because a resource's load event is not the window's.
function eventParent(target, event) {
  const id = nodeIds.get(target);
  if (id === undefined) return shadowHosts.get(target) ?? null;
  if (id === documentId) return event.type === 'load' ? null : globalThis;
  const parent = api.parent(id);
  if (parent === undefined) return shadowHosts.get(target) ?? null;
  return wrap(parent);
}

function dispatch(target, event) {
  const f = flags(event);
  if (f.dispatching) throw domError('InvalidStateError', 'The event is already being dispatched.');
  f.dispatching = true;
  f.stop = false;
  f.stopImmediate = false;
  const path = [];
  for (let t = target; t; t = eventParent(t, event)) path.push(t);
  f.path = path;
  event.target = target;
  try {
    for (let i = path.length - 1; i > 0 && !f.stop; i--) invoke(path[i], event, CAPTURING_PHASE);
    if (!f.stop) invoke(target, event, AT_TARGET);
    if (event.bubbles) {
      for (let i = 1; i < path.length && !f.stop; i++) invoke(path[i], event, BUBBLING_PHASE);
    }
  } finally {
    event.currentTarget = null;
    event.eventPhase = NONE;
    f.dispatching = false;
    f.path = [];
  }
  return !event.defaultPrevented;
}

function invoke(target, event, phase) {
  const f = flags(event);
  const handler = phase === CAPTURING_PHASE ? null : handlerFor(target, event.type);
  const list = listenerMaps.get(target)?.get(event.type);
  if (!handler && !list) return;
  event.currentTarget = target;
  event.eventPhase = phase;
  if (handler) {
    try {
      // `return false` from a handler cancels, as it always has.
      if (handler.call(target, event) === false) event.preventDefault();
    } catch (e) {
      reportError(e);
    }
    if (f.stopImmediate) return;
  }
  if (!list) return;
  for (const entry of list.slice()) {
    if (entry.removed) continue;
    if (phase === CAPTURING_PHASE && !entry.capture) continue;
    if (phase === BUBBLING_PHASE && entry.capture) continue;
    if (entry.once) target.removeEventListener(event.type, entry.callback, { capture: entry.capture });
    f.passive = entry.passive;
    try {
      if (typeof entry.callback === 'function') entry.callback.call(target, event);
      else entry.callback.handleEvent(event);
    } catch (e) {
      reportError(e);
    }
    f.passive = false;
    if (f.stopImmediate) break;
  }
}

// Shortcut for the events the engine fires itself.
function fire(target, type, init) {
  return dispatch(target, new Event(type, init));
}

// ---- on* handler properties -------------------------------------------------

const HANDLER_EVENTS = (
  'abort beforeinput beforeunload blur cancel change click close contextmenu copy cut dblclick ' +
  'drag dragend dragenter dragleave dragover dragstart drop error focus focusin focusout ' +
  'hashchange input invalid keydown keypress keyup load loadend loadstart message ' +
  'mousedown mouseenter mouseleave mousemove mouseout mouseover mouseup offline online open ' +
  'pagehide pageshow paste pointercancel pointerdown pointerenter pointerleave pointermove ' +
  'pointerout pointerover pointerup popstate progress readystatechange reset resize scroll ' +
  'search select selectionchange storage submit timeout toggle touchcancel touchend touchmove ' +
  'touchstart transitionend animationend unhandledrejection unload visibilitychange wheel'
).split(' ');

// The handler for `type`: one assigned as a property, else the element's
// inline attribute, compiled on first use and again whenever its text
// changes. Inline handlers see the element, its form and the document in
// scope, as they do in a browser.
function handlerFor(target, type) {
  const assigned = handlerMaps.get(target);
  if (assigned && assigned.has(type)) return assigned.get(type);
  const id = nodeIds.get(target);
  if (id === undefined || !isElementId(id)) return null;
  const source = api.attr(id, 'on' + type);
  if (source === undefined) return null;
  let cache = compiledHandlers.get(target);
  if (!cache) compiledHandlers.set(target, (cache = new Map()));
  const hit = cache.get(type);
  if (hit && hit.source === source) return hit.handler;
  let handler = null;
  try {
    // The Function constructor compiles sloppy code, where `with` is legal.
    const body = new Function('event', '__document', '__form', `with (__document) with (__form || {}) with (this) {\n${source}\n}`);
    handler = function (event) {
      return body.call(this, event, document, typeof this.form === 'object' ? this.form : null);
    };
  } catch (e) {
    reportError(e);
  }
  cache.set(type, { source, handler });
  return handler;
}

for (const type of HANDLER_EVENTS) {
  Object.defineProperty(EventTarget.prototype, 'on' + type, {
    get() {
      return handlerFor(targetOf(this), type);
    },
    set(value) {
      const target = targetOf(this);
      let map = handlerMaps.get(target);
      if (!map) handlerMaps.set(target, (map = new Map()));
      map.set(type, typeof value === 'function' ? value : null);
    },
    configurable: true,
    enumerable: true,
  });
}

// Shadow roots and their hosts, for event propagation out of a shadow tree.
const shadowHosts = new WeakMap();
