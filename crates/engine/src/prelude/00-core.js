// The JavaScript-side DOM, part 0: primitives, node identity, errors and the
// task queue.
//
// The prelude is the files in this directory concatenated in name order inside
// one function (see `PRELUDE` in bindings.rs). They share that function's
// scope, so a name declared in one file is visible in the others, and nothing
// leaks onto the page's global object unless 70-window.js exports it.
//
// Rust exposes primitives keyed by numeric node id under `globalThis.__dom`.
// Everything object-shaped is built here, because identity
// (`document.body === document.body`) and shape (prototypes, getters,
// `instanceof HTMLElement`) are cheap in JavaScript and expensive in Rust.
//
// Primitives that return a node return `undefined` for "none". Node 0 is the
// document, so 0 cannot mean "none".

'use strict';

const api = globalThis.__dom;
if (!api) throw new Error('the DOM primitives were not installed');
// Page script has no business calling the primitives directly.
delete globalThis.__dom;

const documentId = api.documentId();

// ---- node identity ----------------------------------------------------------
//
// Wrappers are cached by id, so one node is always one object. A wrapper's id
// lives in a WeakMap rather than on the object, where page code would see it.

const wrappers = new Map();
const nodeIds = new WeakMap();

function wrap(id) {
  if (id === undefined || id === null) return null;
  let node = wrappers.get(id);
  if (!node) {
    node = decorate(Object.create(protoFor(id)), id);
    nodeIds.set(node, id);
    wrappers.set(id, node);
  }
  return node;
}

function wrapAll(ids) {
  const out = [];
  for (let i = 0; i < ids.length; i++) out.push(wrap(ids[i]));
  return out;
}

// The id behind `this` in a DOM method. Calling a method on something that is
// not a node is what a browser calls an illegal invocation.
function own(node) {
  const id = nodeIds.get(node);
  if (id === undefined) throw new TypeError('Illegal invocation');
  return id;
}

function requireNode(value, method) {
  const id = value !== null && typeof value === 'object' ? nodeIds.get(value) : undefined;
  if (id === undefined) {
    throw new TypeError(`Failed to execute '${method}': parameter is not of type 'Node'.`);
  }
  return id;
}

const isElementId = (id) => api.nodeType(id) === 1;

// Static lists. A browser's are live; these are snapshots, which is what page
// code that iterates them immediately sees anyway.
class NodeList extends Array {
  item(index) {
    return this[index] ?? null;
  }
}
class HTMLCollection extends Array {
  item(index) {
    return this[index] ?? null;
  }
  namedItem(name) {
    return this.find((el) => el.id === name || el.getAttribute('name') === name) ?? null;
  }
}
Object.defineProperty(NodeList.prototype, Symbol.toStringTag, { value: 'NodeList' });
Object.defineProperty(HTMLCollection.prototype, Symbol.toStringTag, { value: 'HTMLCollection' });

function nodeList(nodes) {
  return Object.setPrototypeOf(nodes, NodeList.prototype);
}

// Named access (`document.forms.login`, `form.elements.password`) works on
// the snapshot through own properties, added only where they do not shadow an
// array member.
function htmlCollection(nodes) {
  const list = Object.setPrototypeOf(nodes, HTMLCollection.prototype);
  for (const el of nodes) {
    for (const key of [el.getAttribute('id'), el.getAttribute('name')]) {
      if (key && !(key in list)) {
        Object.defineProperty(list, key, { value: el, enumerable: false, configurable: true });
      }
    }
  }
  return list;
}

// ---- errors -----------------------------------------------------------------

if (typeof DOMException !== 'function') {
  globalThis.DOMException = class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(message);
      this.name = name;
    }
  };
}

// In a browser a DOMException is an Error. QuickJS's is not, which makes a
// thrown one lose its message on the way out; give it Error's prototype.
if (!(DOMException.prototype instanceof Error)) {
  Object.setPrototypeOf(DOMException.prototype, Error.prototype);
}

function domError(name, message) {
  return new DOMException(message, name);
}

// The engine's own console, a line of text per call, kept for diagnostics
// (`probe_a_url`). The page's `console` (prelude/90-cdp.js) writes here too.
const hostConsole = globalThis.console;

// An exception nobody caught: a script, a listener, a timer, a promise
// nobody handled (`inPromise`). A browser reports it to DevTools as
// `Runtime.exceptionThrown` and, unless it came from a promise, as an `error`
// event, then carries on.
let reportingError = false;
function reportError(error, inPromise = false) {
  let text;
  try {
    text = error && error.stack ? `${error}\n${error.stack}` : String(error);
  } catch (e) {
    text = Object.prototype.toString.call(error);
  }
  hostConsole.error(`Uncaught ${inPromise ? '(in promise) ' : ''}${text}`);
  queueException(error, inPromise ? 'Uncaught (in promise)' : 'Uncaught');
  if (inPromise || reportingError) return;
  reportingError = true;
  try {
    dispatch(
      globalThis,
      new ErrorEvent('error', {
        cancelable: true,
        message: String(error && error.message !== undefined ? error.message : error),
        error,
      }),
    );
  } catch (e) {
    // A broken error handler must not take the reporter down with it.
  } finally {
    reportingError = false;
  }
}

// ---- time and the task queue ------------------------------------------------
//
// Timers run on a virtual clock. When the next task is due later than now the
// clock jumps to it instead of waiting, so a page's five-second timeout costs
// only the work it does. Real time still passes underneath, so a busy loop
// polling the clock terminates. `performance.now()` and `Date.now()` both read
// this clock, so debouncing code that compares the two sees time pass.

const RealDate = Date;
const realStart = RealDate.now();
let skipped = 0;

// Milliseconds since the page started, as `performance.now()` reports.
function nowMs() {
  return RealDate.now() - realStart + skipped;
}

// Milliseconds since the epoch, as `Date.now()` reports.
function epochNow() {
  return RealDate.now() + skipped;
}

const tasks = new Map();
let nextHandle = 1;
let nextSequence = 1;
let turnLimit = 0;

// Queues `callback` to run `delay` ms from now. A repeating task is clamped
// to 4 ms, as browsers clamp nested intervals, so `setInterval(f, 0)` cannot
// starve everything else within one turn.
function schedule(callback, delay, repeat) {
  const handle = nextHandle++;
  const wait = Math.max(0, Number(delay) || 0);
  tasks.set(handle, {
    callback,
    due: nowMs() + wait,
    sequence: nextSequence++,
    repeat: repeat ? Math.max(wait, 4) : 0,
  });
  return handle;
}

function cancel(handle) {
  tasks.delete(Number(handle));
}

// Called by the engine before a run of `__runNextTask`: tasks due later than
// `budget` ms of virtual time from now wait for a later turn.
function beginTurn(budget) {
  turnLimit = nowMs() + Number(budget);
}

// Runs the earliest due task, in (due time, queue order). Returns false when
// nothing is due within the turn.
function runNextTask() {
  // Rejections the last task left unhandled are reported between tasks.
  flushRejections();
  let handle = 0;
  let next = null;
  for (const [h, task] of tasks) {
    if (!next || task.due < next.due || (task.due === next.due && task.sequence < next.sequence)) {
      next = task;
      handle = h;
    }
  }
  if (!next || next.due > turnLimit) return false;
  const now = nowMs();
  if (next.due > now) skipped += next.due - now;
  if (next.repeat) {
    next.due = nowMs() + next.repeat;
    next.sequence = nextSequence++;
  } else {
    tasks.delete(handle);
  }
  try {
    next.callback();
  } catch (e) {
    reportError(e);
  }
  return true;
}

// ---- small shared helpers ---------------------------------------------------

// Classic script text, run as a global script. A throw is reported like any
// uncaught exception. Returns the error as the engine reports it, or ''.
function runClassic(source, name) {
  const failed = api.runScript(source, name);
  if (!failed) return '';
  reportError(failed[1]);
  return failed[0];
}

// `relative` against the document's URL, or null when it is not a URL.
function resolveUrl(relative) {
  return api.resolveUrl(api.url(), String(relative)) ?? null;
}

// Scripts already started, by node id. Parser-inserted scripts are started by
// the engine through `__executeScript`; scripts that arrive in `innerHTML` are
// marked here without running, as in a browser.
const startedScripts = new Set();

// The scripts present when this document's prelude ran were put there by the
// parser. The engine runs those, in its own order, so insertion hooks leave
// them alone.
const parserScripts = new Set(api.byTag(documentId, 'script'));
