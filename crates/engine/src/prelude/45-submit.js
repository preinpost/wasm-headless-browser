// Part 4b: the form element, labels, submission, and what a click does by
// default: follow a link, submit a form, toggle a checkbox.

const HTMLFormElement = defineElement('HTMLFormElement', HTMLElement, {
  get elements() {
    return htmlCollection(formControls(this));
  },
  get length() {
    return formControls(this).length;
  },
  get action() {
    const v = this.getAttribute('action');
    return v ? resolveUrl(v) ?? v : api.url();
  },
  set action(v) {
    this.setAttribute('action', String(v));
  },
  get method() {
    const m = (this.getAttribute('method') || '').toLowerCase();
    return m === 'post' || m === 'dialog' ? m : 'get';
  },
  set method(v) {
    this.setAttribute('method', String(v));
  },
  get enctype() {
    const e = (this.getAttribute('enctype') || '').toLowerCase();
    return e === 'multipart/form-data' || e === 'text/plain' ? e : 'application/x-www-form-urlencoded';
  },
  set enctype(v) {
    this.setAttribute('enctype', String(v));
  },
  get encoding() {
    return this.enctype;
  },
  submit() {
    submitForm(this, null, true);
  },
  requestSubmit(submitter) {
    if (submitter !== undefined && submitter !== null) {
      if (!isSubmitButton(submitter) || submitter.form !== this) {
        throw new TypeError("Failed to execute 'requestSubmit': The specified element is not a submit button of this form.");
      }
    }
    submitForm(this, submitter ?? null, false);
  },
  reset() {
    resetForm(this);
  },
  checkValidity() {
    return formControls(this).every((c) => !c.checkValidity || c.checkValidity());
  },
  reportValidity() {
    return this.checkValidity();
  },
  item(index) {
    return formControls(this)[index] ?? null;
  },
  namedItem(name) {
    return this.elements.namedItem(name);
  },
});
reflect(HTMLFormElement, 'string', { name: 'name', target: 'target', acceptCharset: 'accept-charset', autocomplete: 'autocomplete', rel: 'rel' });
reflect(HTMLFormElement, 'boolean', { noValidate: 'novalidate' });

// `form.username` and `form[0]` reach the form's controls, which old login
// pages rely on. A form wrapper is a proxy over the plain wrapper for that.
function decorate(node, id) {
  if (api.nodeType(id) !== 1 || api.tagName(id) !== 'form') return node;
  return new Proxy(node, {
    get(target, key, receiver) {
      if (typeof key === 'string' && !(key in target)) {
        const controls = formControls(receiver);
        if (/^\d+$/.test(key)) return controls[Number(key)];
        const hit = controls.find((c) => c.getAttribute('name') === key || c.getAttribute('id') === key);
        if (hit) return hit;
      }
      return Reflect.get(target, key, receiver);
    },
    has(target, key) {
      if (key in target) return true;
      if (typeof key !== 'string') return false;
      const controls = formControls(wrap(id));
      return /^\d+$/.test(key) ? Number(key) < controls.length : controls.some((c) => c.getAttribute('name') === key || c.getAttribute('id') === key);
    },
  });
}

const HTMLLabelElement = defineElement('HTMLLabelElement', HTMLElement, {
  get htmlFor() {
    return this.getAttribute('for') ?? '';
  },
  set htmlFor(v) {
    this.setAttribute('for', String(v));
  },
  // The labelled control: the one named by `for`, else the first labelable
  // descendant.
  get control() {
    const target = this.getAttribute('for');
    if (target !== null) {
      const el = document.getElementById(target);
      return el && labelable(el) ? el : null;
    }
    return wrapAll(api.byTag(own(this), '*')).find(labelable) ?? null;
  },
  get form() {
    return this.control?.form ?? null;
  },
});

function labelable(el) {
  const tag = el.localName;
  if (tag === 'input') return el.type !== 'hidden';
  return ['button', 'meter', 'output', 'progress', 'select', 'textarea'].includes(tag);
}

function isSubmitButton(el) {
  if (!nodeIds.has(el)) return false;
  if (el.localName === 'button') return el.type === 'submit';
  return el.localName === 'input' && (el.type === 'submit' || el.type === 'image');
}

// ---- submission -------------------------------------------------------------

function disabledControl(el) {
  return isDisabled(el) || !!el.closest('datalist');
}

// The name/value pairs a submission sends.
function entryList(form, submitter) {
  const entries = [];
  for (const el of formControls(form)) {
    if (disabledControl(el)) continue;
    const tag = el.localName;
    const isButton = tag === 'button' || (tag === 'input' && BUTTON_TYPES.has(el.type));
    if (isButton && el !== submitter) continue;
    const name = el.getAttribute('name');
    if (tag === 'input' && el.type === 'image') {
      const prefix = name ? name + '.' : '';
      entries.push([prefix + 'x', '0'], [prefix + 'y', '0']);
      continue;
    }
    if (!name || tag === 'fieldset' || tag === 'object' || tag === 'output') continue;
    if (tag === 'input') {
      const type = el.type;
      if ((type === 'checkbox' || type === 'radio') && !el.checked) continue;
      if (type === 'file') {
        entries.push([name, new File([], '', { type: 'application/octet-stream' })]);
        continue;
      }
      entries.push([name, el.value]);
    } else if (tag === 'select') {
      for (const option of optionsOf(el)) if (selectedness(option) && !option.disabled) entries.push([name, option.value]);
    } else {
      entries.push([name, el.value]);
    }
  }
  return entries;
}

function missingValue(el) {
  if (!el.hasAttribute('required') || disabledControl(el)) return false;
  if (el.localName === 'input' && (el.type === 'checkbox')) return !el.checked;
  if (el.localName === 'input' && el.type === 'radio') return !radioGroup(el).some((r) => r.checked);
  if (el.localName === 'select') return el.value === '';
  return 'value' in el && el.value === '';
}

// Submits `form` as a browser would: validate and fire `submit` unless this
// came from `form.submit()`, then turn the controls into a request and hand
// it to the engine, which navigates.
function submitForm(form, submitter, fromSubmitMethod) {
  if (!fromSubmitMethod) {
    const skipValidation = form.noValidate || (submitter && submitter.hasAttribute('formnovalidate'));
    if (!skipValidation) {
      const invalid = formControls(form).filter(missingValue);
      if (invalid.length) {
        for (const el of invalid) fire(el, 'invalid', { cancelable: true });
        return;
      }
    }
    const proceed = dispatch(form, new SubmitEvent('submit', { bubbles: true, cancelable: true, submitter }));
    if (!proceed) return;
  }
  const override = (name) => (submitter && submitter.hasAttribute('form' + name) ? submitter.getAttribute('form' + name) : form.getAttribute(name));
  const actionText = override('action');
  const action = actionText ? resolveUrl(actionText) : api.url();
  if (!action) return;
  const method = (override('method') || 'get').toLowerCase();
  const enctype = (override('enctype') || 'application/x-www-form-urlencoded').toLowerCase();
  const entries = entryList(form, submitter);

  if (method === 'dialog') {
    form.closest('dialog')?.close(submitter ? submitter.value : undefined);
    return;
  }
  if (/^javascript:/i.test(action)) {
    runJavascriptUrl(action);
    return;
  }
  if (method === 'post') {
    if (enctype === 'multipart/form-data') {
      const { text, contentType } = encodeMultipart(entries);
      api.submit('POST', action, text, contentType);
    } else if (enctype === 'text/plain') {
      const text = entries.map(([k, v]) => `${k}=${typeof v === 'string' ? v : v.name}\r\n`).join('');
      api.submit('POST', action, text, 'text/plain');
    } else {
      api.submit('POST', action, urlencodeEntries(entries), 'application/x-www-form-urlencoded');
    }
    return;
  }
  const url = new URL(action);
  url.search = urlencodeEntries(entries);
  api.submit('GET', url.href, '', undefined);
}

function urlencodeEntries(entries) {
  return new URLSearchParams(entries.map(([k, v]) => [k, typeof v === 'string' ? v : v.name])).toString();
}

function resetForm(form) {
  if (!fire(form, 'reset', { bubbles: true, cancelable: true })) return;
  for (const el of formControls(form)) {
    const id = own(el);
    api.setControlValue(id, undefined);
    api.setControlChecked(id, undefined);
    if (el.localName === 'select') for (const o of optionsOf(el)) api.setControlSelected(own(o), undefined);
  }
}

// ---- activation: what a click does by default --------------------------------

function hasActivation(el) {
  switch (el.localName) {
    case 'a':
    case 'area':
      return el.hasAttribute('href');
    case 'button':
    case 'label':
    case 'summary':
      return true;
    case 'input':
      return BUTTON_TYPES.has(el.type) || el.type === 'checkbox' || el.type === 'radio';
  }
  return false;
}

// The element whose default action a click on `el` triggers: `el` or the
// nearest ancestor that has one, so a click on a <span> inside a link follows
// the link.
function activationTarget(el) {
  for (let n = el; n && n.nodeType === 1; n = n.parentElement) if (hasActivation(n)) return n;
  return null;
}

// Dispatches `event` at `el` and runs the default action unless a listener
// cancelled it. A checkbox flips before listeners run and flips back when
// cancelled, which is the legacy behaviour every page expects.
function activate(el, event) {
  const target = activationTarget(el);
  let restore = null;
  let changed = false;
  if (target && target.localName === 'input' && !isDisabled(target)) {
    if (target.type === 'checkbox') {
      const was = target.checked;
      setChecked(target, !was);
      changed = true;
      restore = () => setChecked(target, was);
    } else if (target.type === 'radio') {
      const previous = radioGroup(target).find((r) => r.checked) ?? null;
      if (previous !== target) {
        setChecked(target, true);
        changed = true;
        restore = () => (previous ? setChecked(previous, true) : setChecked(target, false));
      }
    }
  }
  const proceed = dispatch(el, event);
  if (!target) return proceed;
  if (!proceed) {
    if (restore) restore();
    return false;
  }
  activationBehavior(target, event, changed);
  return true;
}

function activationBehavior(target, event, changed) {
  if (isDisabled(target)) return;
  switch (target.localName) {
    case 'a':
    case 'area':
      if (!target.hasAttribute('download')) navigateTo(target.href);
      return;
    case 'button': {
      const form = target.form;
      if (!form) return;
      if (target.type === 'submit') submitForm(form, target, false);
      else if (target.type === 'reset') resetForm(form);
      return;
    }
    case 'input': {
      const type = target.type;
      if ((type === 'submit' || type === 'image') && target.form) submitForm(target.form, target, false);
      else if (type === 'reset' && target.form) resetForm(target.form);
      else if ((type === 'checkbox' || type === 'radio') && changed && target.isConnected) {
        dispatch(target, new InputEvent('input', { bubbles: true, composed: true }));
        fire(target, 'change', { bubbles: true });
      }
      return;
    }
    case 'label': {
      const control = target.control;
      if (!control || control.contains(event.target)) return;
      if (control.localName !== 'input' || !BUTTON_TYPES.has(control.type)) control.focus();
      control.click();
      return;
    }
    case 'summary': {
      const details = target.parentElement;
      if (details && details.localName === 'details') details.toggleAttribute('open');
    }
  }
}
