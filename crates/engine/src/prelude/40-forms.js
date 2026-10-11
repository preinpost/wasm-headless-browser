// Part 4: forms. The element interface registry, form controls with their
// own state (what was typed, what is checked), form submission, and what a
// click does by default.

const interfaces = Object.create(null);
interfaces.HTMLElement = HTMLElement;
interfaces.SVGElement = SVGElement;
interfaces.SVGAnimatedString = SVGAnimatedString;

// An element interface. `bodies` hold its own accessors and methods; they
// become own properties of the prototype, which React relies on when it
// looks up `value` on `node.constructor.prototype`. They are copied as
// descriptors, because spreading an object with getters would call them.
function defineElement(name, Parent, ...bodies) {
  const C = tagClass(class extends Parent {}, name);
  for (const body of bodies) Object.defineProperties(C.prototype, Object.getOwnPropertyDescriptors(body));
  interfaces[name] = C;
  return C;
}

function elementInterface(name) {
  return interfaces[name] || defineElement(name, name.startsWith('SVG') ? SVGElement : HTMLElement);
}

// The form a control belongs to: its `form` attribute, else its nearest form
// ancestor.
function formOwner(control) {
  const id = own(control);
  const named = api.attr(id, 'form');
  if (named !== undefined) {
    const form = api.getElementById(named);
    return form !== undefined && api.tagName(form) === 'form' ? wrap(form) : null;
  }
  for (let p = api.parent(id); p !== undefined; p = api.parent(p)) {
    if (isElementId(p) && api.tagName(p) === 'form') return wrap(p);
  }
  return null;
}

const LISTED = new Set(['button', 'fieldset', 'input', 'object', 'output', 'select', 'textarea']);
function formControls(form) {
  const formId = own(form);
  const inside = api.byTag(formId, '*').filter((id) => LISTED.has(api.tagName(id)));
  const named = form.id ? api.byTag(documentId, '*').filter((id) => LISTED.has(api.tagName(id)) && api.attr(id, 'form') === form.id) : [];
  const ids = Array.from(new Set(inside.concat(named)));
  const controls = wrapAll(ids).filter((c) => formOwner(c) === form);
  if (named.length) controls.sort((a, b) => (a.compareDocumentPosition(b) & 4 ? -1 : 1));
  return controls;
}

function labelsOf(control) {
  return nodeList(wrapAll(api.byTag(documentId, 'label')).filter((label) => label.control === control));
}

const validity = Object.freeze({
  valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false,
  rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false, valid: true,
});
const customValidity = new WeakMap();
const ControlCommon = {
  get form() {
    return formOwner(this);
  },
  get labels() {
    return labelsOf(this);
  },
  get validity() {
    const message = customValidity.get(this);
    return message ? { ...validity, customError: true, valid: false } : validity;
  },
  get validationMessage() {
    return customValidity.get(this) || '';
  },
  get willValidate() {
    return !isDisabled(this);
  },
  checkValidity() {
    return !customValidity.get(this);
  },
  reportValidity() {
    return this.checkValidity();
  },
  setCustomValidity(message) {
    customValidity.set(this, String(message));
  },
};

// ---- input ------------------------------------------------------------------

const INPUT_TYPES = new Set([
  'hidden', 'text', 'search', 'tel', 'url', 'email', 'password', 'date', 'month', 'week', 'time',
  'datetime-local', 'number', 'range', 'color', 'checkbox', 'radio', 'file', 'submit', 'image', 'reset', 'button',
]);
const BUTTON_TYPES = new Set(['submit', 'image', 'reset', 'button']);

// How `value` behaves for each type, from the specification's "value mode".
function valueMode(type) {
  if (type === 'checkbox' || type === 'radio') return 'default/on';
  if (type === 'hidden' || BUTTON_TYPES.has(type)) return 'default';
  if (type === 'file') return 'filename';
  return 'value';
}

const selections = new WeakMap();
const indeterminate = new WeakMap();

function radioGroup(input) {
  const name = input.getAttribute('name');
  if (!name) return [input];
  const form = input.form;
  return wrapAll(api.byTag(form ? own(form) : documentId, 'input')).filter(
    (other) => other.type === 'radio' && other.getAttribute('name') === name && other.form === form,
  );
}

function setChecked(input, checked) {
  api.setControlChecked(own(input), !!checked);
  if (checked && input.type === 'radio') {
    for (const other of radioGroup(input)) if (other !== input) api.setControlChecked(own(other), false);
  }
}

const HTMLInputElement = defineElement('HTMLInputElement', HTMLElement, ControlCommon, {
  get type() {
    const type = (this.getAttribute('type') || '').toLowerCase();
    return INPUT_TYPES.has(type) ? type : 'text';
  },
  set type(value) {
    this.setAttribute('type', String(value));
  },
  get value() {
    const id = own(this);
    if (valueMode(this.type) === 'value') {
      const typed = api.controlValue(id);
      if (typed !== undefined) return typed;
    }
    const attribute = api.attr(id, 'value');
    if (attribute !== undefined) return attribute;
    return valueMode(this.type) === 'default/on' ? 'on' : '';
  },
  set value(value) {
    value = value === null || value === undefined ? '' : String(value);
    const mode = valueMode(this.type);
    if (mode === 'value') {
      api.setControlValue(own(this), value);
      selections.set(this, { start: value.length, end: value.length, direction: 'none' });
    } else if (mode === 'filename') {
      if (value !== '') throw domError('InvalidStateError', 'This input element accepts a filename, which may only be programmatically set to the empty string.');
    } else {
      this.setAttribute('value', value);
    }
  },
  get defaultValue() {
    return this.getAttribute('value') ?? '';
  },
  set defaultValue(value) {
    this.setAttribute('value', String(value));
  },
  get checked() {
    const id = own(this);
    return api.controlChecked(id) ?? api.hasAttr(id, 'checked');
  },
  set checked(value) {
    setChecked(this, value);
  },
  get indeterminate() {
    return indeterminate.get(this) ?? false;
  },
  set indeterminate(value) {
    indeterminate.set(this, !!value);
  },
  get valueAsNumber() {
    const n = parseFloat(this.value);
    return Number.isFinite(n) ? n : NaN;
  },
  set valueAsNumber(value) {
    this.value = Number.isFinite(value) ? String(value) : '';
  },
  get valueAsDate() {
    return null;
  },
  get files() {
    return this.type === 'file' ? [] : null;
  },
  get selectionStart() {
    return selections.get(this)?.start ?? this.value.length;
  },
  set selectionStart(v) {
    this.setSelectionRange(v, Math.max(v, this.selectionEnd));
  },
  get selectionEnd() {
    return selections.get(this)?.end ?? this.value.length;
  },
  set selectionEnd(v) {
    this.setSelectionRange(Math.min(this.selectionStart, v), v);
  },
  get selectionDirection() {
    return selections.get(this)?.direction ?? 'none';
  },
  setSelectionRange(start, end, direction = 'none') {
    const length = this.value.length;
    const s = Math.min(Math.max(0, Number(start) || 0), length);
    const e = Math.min(Math.max(s, Number(end) || 0), length);
    selections.set(this, { start: s, end: e, direction });
  },
  setRangeText(text, start = this.selectionStart, end = this.selectionEnd) {
    const v = this.value;
    this.value = v.slice(0, start) + String(text) + v.slice(end);
  },
  select() {
    this.setSelectionRange(0, this.value.length);
  },
  get maxLength() {
    const n = parseInt(this.getAttribute('maxlength'), 10);
    return Number.isFinite(n) ? n : -1;
  },
  set maxLength(v) {
    this.setAttribute('maxlength', String(v));
  },
  stepUp() {},
  stepDown() {},
  showPicker() {},
});
reflect(HTMLInputElement, 'string', {
  name: 'name', placeholder: 'placeholder', autocomplete: 'autocomplete', min: 'min', max: 'max', step: 'step',
  pattern: 'pattern', accept: 'accept', alt: 'alt', dirName: 'dirname', formMethod: 'formmethod',
  formEnctype: 'formenctype', formTarget: 'formtarget', list: 'list',
});
reflect(HTMLInputElement, 'boolean', {
  disabled: 'disabled', required: 'required', readOnly: 'readonly', multiple: 'multiple',
  defaultChecked: 'checked', formNoValidate: 'formnovalidate',
});
reflect(HTMLInputElement, 'url', { src: 'src', formAction: 'formaction' });
reflect(HTMLInputElement, 'number', { size: 'size', minLength: 'minlength' });

const HTMLTextAreaElement = defineElement('HTMLTextAreaElement', HTMLElement, ControlCommon, {
  get type() {
    return 'textarea';
  },
  get value() {
    const id = own(this);
    return api.controlValue(id) ?? api.textContent(id);
  },
  set value(value) {
    value = value === null || value === undefined ? '' : String(value);
    api.setControlValue(own(this), value);
    selections.set(this, { start: value.length, end: value.length, direction: 'none' });
  },
  get defaultValue() {
    return api.textContent(own(this));
  },
  set defaultValue(value) {
    replaceText(this, value);
  },
  get textLength() {
    return this.value.length;
  },
  get selectionStart() {
    return selections.get(this)?.start ?? this.value.length;
  },
  get selectionEnd() {
    return selections.get(this)?.end ?? this.value.length;
  },
  get selectionDirection() {
    return selections.get(this)?.direction ?? 'none';
  },
  setSelectionRange: HTMLInputElement.prototype.setSelectionRange,
  setRangeText: HTMLInputElement.prototype.setRangeText,
  select: HTMLInputElement.prototype.select,
});
reflect(HTMLTextAreaElement, 'string', { name: 'name', placeholder: 'placeholder', autocomplete: 'autocomplete', wrap: 'wrap' });
reflect(HTMLTextAreaElement, 'boolean', { disabled: 'disabled', required: 'required', readOnly: 'readonly' });
reflect(HTMLTextAreaElement, 'number', { rows: 'rows', cols: 'cols', minLength: 'minlength' });

// ---- select and option ------------------------------------------------------

const optionsOf = (select) => wrapAll(api.byTag(own(select), 'option'));
const explicitSelected = (id) => api.controlSelected(id) ?? api.hasAttr(id, 'selected');

// Whether an option is selected, including the single-select default: with no
// option explicitly selected, the first enabled one is.
function selectedness(option) {
  const id = own(option);
  const explicit = api.controlSelected(id);
  if (explicit !== undefined) return explicit;
  if (api.hasAttr(id, 'selected')) return true;
  const select = option.closest('select');
  if (!select || select.multiple) return false;
  const options = optionsOf(select);
  if (options.some((o) => explicitSelected(own(o)))) return false;
  return options.find((o) => !o.disabled) === option;
}

function selectOption(select, chosen) {
  for (const option of optionsOf(select)) {
    if (option === chosen) api.setControlSelected(own(option), true);
    else if (!select.multiple) api.setControlSelected(own(option), false);
  }
}

const HTMLOptionElement = defineElement('HTMLOptionElement', HTMLElement, {
  get value() {
    return this.getAttribute('value') ?? this.text;
  },
  set value(v) {
    this.setAttribute('value', String(v));
  },
  get text() {
    return api.textContent(own(this)).replace(/\s+/g, ' ').trim();
  },
  set text(v) {
    replaceText(this, v);
  },
  get label() {
    return this.getAttribute('label') ?? this.text;
  },
  get selected() {
    return selectedness(this);
  },
  set selected(value) {
    const select = this.closest('select');
    if (value && select) selectOption(select, this);
    else api.setControlSelected(own(this), !!value);
  },
  get index() {
    const select = this.closest('select');
    return select ? optionsOf(select).indexOf(this) : 0;
  },
  get form() {
    const select = this.closest('select');
    return select ? select.form : null;
  },
});
reflect(HTMLOptionElement, 'boolean', { disabled: 'disabled', defaultSelected: 'selected' });

const HTMLSelectElement = defineElement('HTMLSelectElement', HTMLElement, ControlCommon, {
  get type() {
    return this.multiple ? 'select-multiple' : 'select-one';
  },
  get options() {
    const list = htmlCollection(optionsOf(this));
    Object.defineProperty(list, 'selectedIndex', { get: () => this.selectedIndex, set: (v) => (this.selectedIndex = v) });
    return list;
  },
  get length() {
    return optionsOf(this).length;
  },
  get selectedOptions() {
    return htmlCollection(optionsOf(this).filter(selectedness));
  },
  get selectedIndex() {
    return optionsOf(this).findIndex(selectedness);
  },
  set selectedIndex(index) {
    const options = optionsOf(this);
    options.forEach((o, i) => api.setControlSelected(own(o), i === Number(index)));
  },
  get value() {
    const chosen = optionsOf(this).find(selectedness);
    return chosen ? chosen.value : '';
  },
  set value(value) {
    const options = optionsOf(this);
    const chosen = options.find((o) => o.value === String(value));
    options.forEach((o) => api.setControlSelected(own(o), o === chosen));
  },
  item(index) {
    return optionsOf(this)[index] ?? null;
  },
  namedItem(name) {
    return optionsOf(this).find((o) => o.id === name || o.getAttribute('name') === name) ?? null;
  },
  add(option, before) {
    const reference = typeof before === 'number' ? this.item(before) : before ?? null;
    if (reference) reference.parentNode.insertBefore(option, reference);
    else this.appendChild(option);
  },
  remove(index) {
    if (index === undefined) return ChildNode.remove.call(this);
    this.item(index)?.remove();
  },
  showPicker() {},
});
reflect(HTMLSelectElement, 'string', { name: 'name', autocomplete: 'autocomplete' });
reflect(HTMLSelectElement, 'boolean', { disabled: 'disabled', required: 'required', multiple: 'multiple' });
reflect(HTMLSelectElement, 'number', { size: 'size' });

// ---- button -----------------------------------------------------------------

const HTMLButtonElement = defineElement('HTMLButtonElement', HTMLElement, ControlCommon, {
  get type() {
    const type = (this.getAttribute('type') || '').toLowerCase();
    return type === 'reset' || type === 'button' ? type : 'submit';
  },
  set type(value) {
    this.setAttribute('type', String(value));
  },
});
reflect(HTMLButtonElement, 'string', { name: 'name', value: 'value', formMethod: 'formmethod', formEnctype: 'formenctype', formTarget: 'formtarget' });
reflect(HTMLButtonElement, 'boolean', { disabled: 'disabled', formNoValidate: 'formnovalidate' });
reflect(HTMLButtonElement, 'url', { formAction: 'formaction' });
