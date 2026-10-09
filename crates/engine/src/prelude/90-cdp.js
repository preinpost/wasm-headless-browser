// Part 9: what the engine's CDP methods call into. Remote objects for
// `Runtime.evaluate` and `Runtime.callFunctionOn`, and input: inserted text
// and key events. Results go back to Rust as CDP-shaped JSON.

// ---- remote objects ---------------------------------------------------------

const remoteObjects = new Map();
const remoteIds = new Map();
let nextRemote = 1;

function remoteId(value) {
  let id = remoteIds.get(value);
  if (!id) {
    id = `whb-${nextRemote++}`;
    remoteIds.set(value, id);
    remoteObjects.set(id, value);
  }
  return id;
}

function releaseRemote(id) {
  const value = remoteObjects.get(id);
  remoteObjects.delete(id);
  remoteIds.delete(value);
}

// Chrome's short description of a node: `input#q.big`.
function nodeDescription(node) {
  const id = own(node);
  if (!isElementId(id)) return node.nodeName;
  let text = api.tagName(id);
  const elementId = api.attr(id, 'id');
  if (elementId) text += '#' + elementId;
  const classes = (api.attr(id, 'class') || '').trim();
  if (classes) text += '.' + classes.split(/\s+/).join('.');
  return text;
}

function subtypeOf(value) {
  if (nodeIds.has(value)) return 'node';
  if (Array.isArray(value)) return 'array';
  if (value instanceof Error || value instanceof DOMException) return 'error';
  if (value instanceof RegExp) return 'regexp';
  if (value instanceof RealDate) return 'date';
  if (value instanceof Map) return 'map';
  if (value instanceof Set) return 'set';
  if (value instanceof WeakMap) return 'weakmap';
  if (value instanceof WeakSet) return 'weakset';
  if (value instanceof Promise) return 'promise';
  if (ArrayBuffer.isView(value)) return 'typedarray';
  if (value instanceof ArrayBuffer) return 'arraybuffer';
  return undefined;
}

function classNameOf(value) {
  if (nodeIds.has(value)) return value[Symbol.toStringTag];
  const tag = value[Symbol.toStringTag];
  if (typeof tag === 'string') return tag;
  return value.constructor && typeof value.constructor.name === 'string' && value.constructor.name ? value.constructor.name : 'Object';
}

function descriptionOf(value, subtype, className) {
  switch (subtype) {
    case 'node':
      return nodeDescription(value);
    case 'array':
      return `Array(${value.length})`;
    case 'error':
      return value.stack ? `${value.name}: ${value.message}\n${value.stack}` : `${value.name}: ${value.message}`;
    case 'regexp':
      return String(value);
    case 'date':
      return value.toString();
    case 'map':
    case 'set':
      return `${className}(${value.size})`;
    case 'typedarray':
      return `${className}(${value.length})`;
  }
  return className;
}

// A value as a CDP RemoteObject. With `byValue`, objects travel as JSON, as
// `returnByValue` asks; otherwise as a handle the client can call into.
function remoteObject(value, byValue) {
  const type = typeof value;
  if (value === undefined) return { type: 'undefined' };
  if (value === null) return { type: 'object', subtype: 'null', value: null };
  if (type === 'number') {
    if (Number.isNaN(value) || !Number.isFinite(value) || Object.is(value, -0)) {
      const text = Object.is(value, -0) ? '-0' : String(value);
      return { type, unserializableValue: text, description: text };
    }
    return { type, value, description: String(value) };
  }
  if (type === 'string' || type === 'boolean') return { type, value };
  if (type === 'bigint') return { type, unserializableValue: `${value}n`, description: `${value}n` };
  if (type === 'symbol') return { type, description: value.toString(), objectId: remoteId(value) };
  if (byValue && type !== 'function') {
    try {
      const json = JSON.stringify(value);
      return { type: 'object', value: json === undefined ? undefined : JSON.parse(json) };
    } catch (e) {
      // Cyclic or otherwise unserializable: fall through to a handle.
    }
  }
  if (type === 'function') {
    return { type, className: 'Function', description: Function.prototype.toString.call(value), objectId: remoteId(value) };
  }
  const subtype = subtypeOf(value);
  const className = classNameOf(value);
  const out = { type: 'object', className, description: descriptionOf(value, subtype, className), objectId: remoteId(value) };
  if (subtype) out.subtype = subtype;
  return out;
}

function exceptionDetails(error) {
  const exception = remoteObject(error, false);
  const message =
    error !== null && typeof error === 'object' && 'message' in error ? `${error.name ?? 'Error'}: ${error.message}` : String(error);
  return { exceptionId: 1, text: message, lineNumber: 0, columnNumber: 0, exception };
}

// The result of an evaluation or a call. A promise the client asked to await
// is parked under a token; the engine runs the page until `settled` reports
// it.
const pendingPromises = new Map();
let nextPending = 1;
function cdpResult(value, threw, byValue, awaitPromise) {
  if (threw) return JSON.stringify({ result: remoteObject(value, false), exceptionDetails: exceptionDetails(value) });
  if (awaitPromise && value !== null && (typeof value === 'object' || typeof value === 'function') && typeof value.then === 'function') {
    const token = nextPending++;
    const slot = { done: null };
    pendingPromises.set(token, slot);
    Promise.resolve(value).then(
      (v) => (slot.done = cdpResult(v, false, byValue, false)),
      (e) => (slot.done = cdpResult(e, true, byValue, false)),
    );
    return JSON.stringify({ pending: token });
  }
  return JSON.stringify({ result: remoteObject(value, byValue) });
}

function cdpSettled(token) {
  const slot = pendingPromises.get(token);
  if (!slot || slot.done === null) return '';
  pendingPromises.delete(token);
  return slot.done;
}

// One CallArgument: a JSON value, an unserializable number, or a handle.
function callArgument(arg) {
  if (!arg || typeof arg !== 'object') return undefined;
  if ('objectId' in arg) {
    if (!remoteObjects.has(arg.objectId)) throw new Error('Could not find object with given id');
    return remoteObjects.get(arg.objectId);
  }
  if ('unserializableValue' in arg) {
    const text = arg.unserializableValue;
    if (text.endsWith('n')) return BigInt(text.slice(0, -1));
    return text === '-0' ? -0 : Number(text);
  }
  return arg.value;
}

function cdpCall(objectId, declaration, argsJson, byValue, awaitPromise) {
  let target;
  if (objectId) {
    if (!remoteObjects.has(objectId)) {
      return JSON.stringify({ error: 'Could not find object with given id' });
    }
    target = remoteObjects.get(objectId);
  }
  try {
    const fn = (0, eval)(`(${declaration})`);
    const args = JSON.parse(argsJson || '[]').map(callArgument);
    return cdpResult(fn.apply(target, args), false, byValue, awaitPromise);
  } catch (e) {
    return cdpResult(e, true, byValue, false);
  }
}

// `DOM.resolveNode`: a handle on node `id`.
function cdpNode(id) {
  return JSON.stringify(remoteObject(wrap(id), false));
}

// The node behind a handle, for `DOM.describeNode` and `DOM.requestNode`.
function cdpNodeId(objectId) {
  const id = nodeIds.get(remoteObjects.get(objectId));
  return JSON.stringify(id === undefined ? -1 : id);
}

// ---- text input -------------------------------------------------------------

function isTextControl(el) {
  if (!el || !nodeIds.has(el)) return false;
  if (el.localName === 'textarea') return true;
  return el.localName === 'input' && valueMode(el.type) === 'value';
}

// Replaces the selection of a text control with `text`, the way typing
// does: `beforeinput`, then the value changes underneath any property a
// framework installed on the element, then `input`. Going under the
// property is what lets React's value tracker notice the change.
function editControl(el, text, inputType) {
  if (el.disabled || el.readOnly) return;
  const proceed = dispatch(el, new InputEvent('beforeinput', { bubbles: true, cancelable: true, composed: true, inputType, data: text || null }));
  if (!proceed) return;
  const value = currentValue(el);
  let start = Math.min(el.selectionStart, value.length);
  let end = Math.min(el.selectionEnd, value.length);
  if (inputType === 'deleteContentBackward' && start === end) start = Math.max(0, start - 1);
  if (inputType === 'deleteContentForward' && start === end) end = Math.min(value.length, end + 1);
  let inserted = text;
  const limit = el.maxLength;
  if (limit >= 0) inserted = inserted.slice(0, Math.max(0, limit - (value.length - (end - start))));
  const next = value.slice(0, start) + inserted + value.slice(end);
  if (next === value) return;
  api.setControlValue(own(el), next);
  const caret = start + inserted.length;
  selections.set(el, { start: caret, end: caret, direction: 'none' });
  dispatch(el, new InputEvent('input', { bubbles: true, composed: true, inputType, data: text || null }));
}

// The value as the prototype reports it, ignoring any accessor a framework
// put on the element itself.
function currentValue(el) {
  const proto = el.localName === 'textarea' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  return Object.getOwnPropertyDescriptor(proto, 'value').get.call(el);
}

function editableHost(el) {
  return el && nodeIds.has(el) && el.isContentEditable ? el : null;
}

// `Input.insertText`: text into the focused element.
function insertText(text) {
  const el = focusedElement();
  if (isTextControl(el)) {
    editControl(el, String(text), 'insertText');
    return;
  }
  const host = editableHost(el);
  if (host) {
    if (!dispatch(host, new InputEvent('beforeinput', { bubbles: true, cancelable: true, composed: true, inputType: 'insertText', data: text }))) return;
    host.appendChild(document.createTextNode(String(text)));
    dispatch(host, new InputEvent('input', { bubbles: true, composed: true, inputType: 'insertText', data: text }));
  }
}

// ---- keys -------------------------------------------------------------------

const ACTIVATED_BY_SPACE = (el) =>
  el.localName === 'button' || (el.localName === 'input' && (BUTTON_TYPES.has(el.type) || el.type === 'checkbox' || el.type === 'radio'));

// Enter in a form field submits the form, by clicking its default button
// when it has one. That is "implicit submission".
function implicitSubmit(field) {
  const form = field.form;
  if (!form) return;
  const controls = formControls(form);
  const defaultButton = controls.find(isSubmitButton);
  if (defaultButton) {
    if (!isDisabled(defaultButton)) defaultButton.click();
    return;
  }
  const blocking = controls.filter((c) => c.localName === 'input' && valueMode(c.type) === 'value');
  if (blocking.length <= 1) submitForm(form, null, false);
}

function tabbable() {
  return wrapAll(api.byTag(documentId, '*')).filter((el) => focusable(el) && el.tabIndex >= 0);
}

function moveFocusByTab(backwards) {
  const order = tabbable();
  if (!order.length) return;
  const current = order.indexOf(focusedElement());
  const next = current < 0 ? (backwards ? order.length - 1 : 0) : (current + (backwards ? -1 : 1) + order.length) % order.length;
  order[next].focus();
}

// What a key does when no listener cancelled its keydown.
function keyDefault(target, key, init) {
  const text = isTextControl(target);
  const shortcut = init.ctrlKey || init.metaKey;
  if (shortcut && key.toLowerCase() === 'a' && text) {
    target.select();
    return;
  }
  switch (key) {
    case 'Enter':
      if (target.localName === 'textarea') editControl(target, '\n', 'insertLineBreak');
      else if (text) implicitSubmit(target);
      else if (target.localName === 'button' || (target.localName === 'a' && target.hasAttribute('href'))) target.click();
      return;
    case 'Backspace':
      if (text) editControl(target, '', 'deleteContentBackward');
      return;
    case 'Delete':
      if (text) editControl(target, '', 'deleteContentForward');
      return;
    case 'Tab':
      moveFocusByTab(init.shiftKey);
      return;
    case 'ArrowLeft':
    case 'ArrowRight':
    case 'Home':
    case 'End': {
      if (!text) return;
      const length = currentValue(target).length;
      const at = target.selectionStart;
      const caret = key === 'Home' ? 0 : key === 'End' ? length : Math.min(length, Math.max(0, at + (key === 'ArrowLeft' ? -1 : 1)));
      target.setSelectionRange(caret, caret);
      return;
    }
  }
}

// `Input.dispatchKeyEvent`. A keyDown that carries text also types it, as
// Chrome does: keydown, keypress, then the text goes in.
function dispatchKey(json) {
  const p = JSON.parse(json);
  const target = focusedElement() ?? document.body ?? document.documentElement;
  if (!target) return;
  const modifiers = p.modifiers | 0;
  const code = p.windowsVirtualKeyCode | 0;
  const init = {
    bubbles: true,
    cancelable: true,
    composed: true,
    view: globalThis,
    key: p.key ?? '',
    code: p.code ?? '',
    location: p.location | 0,
    repeat: !!p.autoRepeat,
    altKey: !!(modifiers & 1),
    ctrlKey: !!(modifiers & 2),
    metaKey: !!(modifiers & 4),
    shiftKey: !!(modifiers & 8),
    keyCode: code,
    which: code,
  };
  if (p.type === 'keyUp') {
    const proceed = dispatch(target, new KeyboardEvent('keyup', init));
    if (proceed && init.key === ' ' && ACTIVATED_BY_SPACE(target)) target.click();
    return;
  }
  if (p.type === 'char') {
    if (p.text) insertText(p.text);
    return;
  }
  if (!dispatch(target, new KeyboardEvent('keydown', init))) return;
  const text = p.type === 'keyDown' ? p.text ?? '' : '';
  if (text && !init.ctrlKey && !init.metaKey) {
    const charCode = text.charCodeAt(0);
    if (!dispatch(target, new KeyboardEvent('keypress', { ...init, charCode, keyCode: charCode, which: charCode }))) return;
    if (text === '\r' || text === '\n') keyDefault(target, 'Enter', init);
    else if (isTextControl(target) || editableHost(target)) insertText(text);
    return;
  }
  keyDefault(target, init.key, init);
}

// ---- the mouse --------------------------------------------------------------

const mouse = { hover: null, pressed: null, pressedButton: -1 };
const BUTTONS = { left: 0, middle: 1, right: 2, back: 3, forward: 4 };

function commonAncestor(a, b) {
  for (let n = a; n; n = n.parentNode) if (n.contains(b)) return n;
  return null;
}

// `Input.dispatchMouseEvent`. The target is whatever the layout puts at the
// point, the events come in a browser's order, and a press then release
// on the same element (or inside the same ancestor) is a click, with the
// click's default action: follow a link, submit a form, toggle a checkbox.
function dispatchMouse(json) {
  const p = JSON.parse(json);
  const x = Number(p.x) || 0;
  const y = Number(p.y) || 0;
  const target = wrap(api.hitTest(x, y)[0]) ?? document.documentElement;
  if (!target) return;
  const modifiers = p.modifiers | 0;
  const button = BUTTONS[p.button] ?? 0;
  const init = {
    bubbles: true,
    cancelable: true,
    composed: true,
    view: globalThis,
    clientX: x,
    clientY: y,
    screenX: x,
    screenY: y,
    button: p.button === 'none' ? 0 : button,
    buttons: p.buttons | 0,
    altKey: !!(modifiers & 1),
    ctrlKey: !!(modifiers & 2),
    metaKey: !!(modifiers & 4),
    shiftKey: !!(modifiers & 8),
    detail: p.clickCount | 0,
    pointerId: 1,
    pointerType: 'mouse',
    isPrimary: true,
  };

  // Every event at a point first moves the pointer there.
  if (mouse.hover !== target) {
    const previous = mouse.hover;
    if (previous && previous.isConnected) {
      dispatch(previous, new PointerEvent('pointerout', { ...init, relatedTarget: target }));
      dispatch(previous, new MouseEvent('mouseout', { ...init, relatedTarget: target }));
      dispatch(previous, new PointerEvent('pointerleave', { ...init, bubbles: false, cancelable: false, relatedTarget: target }));
      dispatch(previous, new MouseEvent('mouseleave', { ...init, bubbles: false, cancelable: false, relatedTarget: target }));
    }
    mouse.hover = target;
    dispatch(target, new PointerEvent('pointerover', { ...init, relatedTarget: previous }));
    dispatch(target, new MouseEvent('mouseover', { ...init, relatedTarget: previous }));
    dispatch(target, new PointerEvent('pointerenter', { ...init, bubbles: false, cancelable: false, relatedTarget: previous }));
    dispatch(target, new MouseEvent('mouseenter', { ...init, bubbles: false, cancelable: false, relatedTarget: previous }));
  }

  switch (p.type) {
    case 'mouseMoved':
      dispatch(target, new PointerEvent('pointermove', init));
      dispatch(target, new MouseEvent('mousemove', init));
      return;
    case 'mousePressed': {
      dispatch(target, new PointerEvent('pointerdown', init));
      const proceed = dispatch(target, new MouseEvent('mousedown', init));
      mouse.pressed = target;
      mouse.pressedButton = button;
      // A press moves focus to what was pressed, or away from whatever had it.
      if (proceed && button === 0) {
        let focusTarget = target;
        while (focusTarget && focusTarget.nodeType === 1 && !focusable(focusTarget)) focusTarget = focusTarget.parentElement;
        if (focusTarget && focusTarget.nodeType === 1) focusTarget.focus();
        else if (focusedElement()) moveFocus(null);
      }
      if (button === 2) dispatch(target, new MouseEvent('contextmenu', init));
      return;
    }
    case 'mouseReleased': {
      dispatch(target, new PointerEvent('pointerup', init));
      dispatch(target, new MouseEvent('mouseup', init));
      const pressed = mouse.pressed;
      mouse.pressed = null;
      if (!pressed || mouse.pressedButton !== button) return;
      const clicked = pressed === target ? target : commonAncestor(pressed, target);
      if (!clicked) return;
      if (button === 0) {
        if (!isDisabled(clicked)) activate(clicked, new PointerEvent('click', { ...init, detail: Math.max(1, init.detail) }));
        if (init.detail === 2) dispatch(clicked, new MouseEvent('dblclick', { ...init, detail: 2 }));
      } else if (button === 1) {
        dispatch(clicked, new PointerEvent('auxclick', init));
      }
      return;
    }
    case 'mouseWheel':
      dispatch(target, new WheelEvent('wheel', { ...init, deltaX: Number(p.deltaX) || 0, deltaY: Number(p.deltaY) || 0 }));
  }
}

for (const [name, fn] of Object.entries({
  __dispatchMouse: dispatchMouse,
  __cdpResult: cdpResult,
  __cdpSettled: cdpSettled,
  __cdpCall: cdpCall,
  __cdpNode: cdpNode,
  __cdpNodeId: cdpNodeId,
  __cdpRelease: releaseRemote,
  __insertText: insertText,
  __dispatchKey: dispatchKey,
})) {
  Object.defineProperty(globalThis, name, { value: fn, writable: false, configurable: false, enumerable: false });
}
