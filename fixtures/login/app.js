// The fixture's single-page app. It renders like a framework would: state in
// JavaScript, the DOM rebuilt from it, and inputs whose value lives in state.

const state = { user: '', password: '', error: '', busy: false, me: null };
const app = document.getElementById('app');

function h(tag, props, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (key.startsWith('on')) el.addEventListener(key.slice(2).toLowerCase(), value);
    else if (key === 'value') el.value = value;
    else el.setAttribute(key, value);
  }
  for (const child of children) el.append(child);
  return el;
}

function render() {
  app.replaceChildren();
  if (state.me) {
    app.append(h('h1', {}, `Welcome, ${state.me}`), h('p', {}, 'You are signed in.'));
    return;
  }
  const form = h(
    'form',
    { onSubmit: submit },
    h('h1', {}, 'Sign in'),
    h('label', { for: 'user' }, 'User'),
    h('input', { id: 'user', name: 'user', autocomplete: 'username', value: state.user, onInput: (e) => (state.user = e.target.value) }),
    h('label', { for: 'password' }, 'Password'),
    h('input', { id: 'password', type: 'password', value: state.password, onInput: (e) => (state.password = e.target.value) }),
    h('button', { type: 'submit' }, state.busy ? 'Signing in' : 'Sign in'),
  );
  if (state.error) form.append(h('p', { role: 'alert' }, state.error));
  app.append(form);
}

async function submit(event) {
  event.preventDefault();
  state.busy = true;
  state.error = '';
  render();
  const response = await fetch('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ user: state.user, password: state.password }),
  });
  state.busy = false;
  state.password = '';
  if (!response.ok) {
    state.error = (await response.json()).error;
    render();
    return;
  }
  history.pushState({}, '', '/home');
  await loadMe();
}

async function loadMe() {
  const response = await fetch('/api/me');
  if (response.ok) state.me = (await response.json()).name;
  render();
}

if (location.pathname === '/home') loadMe();
else render();
