#!/usr/bin/env bash
# Lays a page out in this engine and in Chrome at the same viewport, and
# compares where the elements a user sees and clicks end up.
#
#   ./scripts/compare-layout.sh https://gw.example.com/
#   SELECTOR='input,button' ./scripts/compare-layout.sh URL
#   WAIT_FOR='#login' ./scripts/compare-layout.sh URL   # for a script-rendered page
#
# After that, it waits SETTLE_MS (default 2500) more, because pages animate
# into place in Chrome and this engine shows only where they end up.
#
# Chrome is Playwright's headless shell, so the user's own Chrome and its
# profile are not touched. Elements are paired by tag, id, first class and
# position among those with the same key; each pair's x, y, width and height
# are compared, and the script prints the pairs that differ by more than
# TOLERANCE px (default 2) and a summary. It reads only: nothing is typed
# or clicked.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
URL="${1:?usage: compare-layout.sh URL}"
SELECTOR="${SELECTOR:-input,button,select,textarea,label,a,img,h1,h2,h3,p,li,span,form,[class*=login],[class*=Login],[id*=login]}"
TOLERANCE="${TOLERANCE:-2}"
ENGINE_PORT="${ENGINE_PORT:-9470}"
CHROME_PORT="${CHROME_PORT:-9471}"
CHROME="$(ls "$HOME"/Library/Caches/ms-playwright/chromium_headless_shell-*/*/chrome-headless-shell 2>/dev/null | tail -1)"
[[ -x "$CHROME" ]] || { echo "Playwright's chrome-headless-shell is not installed" >&2; exit 2; }

WORK="$(mktemp -d)"
cleanup() {
  for port in "$ENGINE_PORT" "$CHROME_PORT"; do
    AGENT_BROWSER_SESSION="cmp-$port-$$" agent-browser --cdp "http://127.0.0.1:$port" close >/dev/null 2>&1 || true
  done
  kill "${ENGINE_PID:-}" "${CHROME_PID:-}" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

cargo build --release --manifest-path "$ROOT/Cargo.toml" --quiet
"$ROOT/target/release/wasm-headless-browser" --engine html "127.0.0.1:$ENGINE_PORT" >/dev/null 2>&1 &
ENGINE_PID=$!
"$CHROME" --remote-debugging-port="$CHROME_PORT" --user-data-dir="$WORK/profile" about:blank >/dev/null 2>&1 &
CHROME_PID=$!
sleep 1.5

EXPRESSION="JSON.stringify([...document.querySelectorAll('$SELECTOR')].map((e) => {
  const r = e.getBoundingClientRect();
  const c = typeof e.className === 'string' && e.className.trim() ? '.' + e.className.trim().split(/\\s+/)[0] : '';
  const visible = r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden';
  return [e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + c, r.x, r.y, r.width, r.height, visible];
}))"

for port in "$ENGINE_PORT" "$CHROME_PORT"; do
  export AGENT_BROWSER_SESSION="cmp-$port-$$" AGENT_BROWSER_NAMESPACE="cmp-$port-$$"
  agent-browser --cdp "http://127.0.0.1:$port" set viewport 800 600 >/dev/null 2>&1 || true
  agent-browser --cdp "http://127.0.0.1:$port" open "$URL" >/dev/null 2>&1
  if [[ -n "${WAIT_FOR:-}" ]]; then
    agent-browser --cdp "http://127.0.0.1:$port" wait "$WAIT_FOR" >/dev/null 2>&1 || true
  else
    agent-browser --cdp "http://127.0.0.1:$port" wait --load networkidle >/dev/null 2>&1 || true
  fi
  agent-browser --cdp "http://127.0.0.1:$port" wait "${SETTLE_MS:-2500}" >/dev/null 2>&1 || true
  agent-browser --cdp "http://127.0.0.1:$port" eval "$EXPRESSION" >"$WORK/$port.json" 2>&1
done

python3 - "$WORK/$ENGINE_PORT.json" "$WORK/$CHROME_PORT.json" "$TOLERANCE" <<'PY'
import json, sys, collections
def load(path):
    text = open(path).read().strip()
    data = json.loads(text)
    if isinstance(data, str):
        data = json.loads(data)
    seen, out = collections.Counter(), {}
    for key, x, y, w, h, visible in data:
        seen[key] += 1
        if visible:
            out[f'{key}[{seen[key] - 1}]'] = (x, y, w, h)
    return out
ours, chrome = load(sys.argv[1]), load(sys.argv[2])
tolerance = float(sys.argv[3])
both = [k for k in chrome if k in ours]
worst = []
for key in both:
    d = [abs(a - b) for a, b in zip(ours[key], chrome[key])]
    worst.append((max(d), key, d))
worst.sort(reverse=True)
off = [w for w in worst if w[0] > tolerance]
for m, key, d in off[:25]:
    o, c = ours[key], chrome[key]
    print(f'  {m:6.1f}px  {key[:48]:48s} ours {o[0]:.0f},{o[1]:.0f} {o[2]:.0f}x{o[3]:.0f}  chrome {c[0]:.0f},{c[1]:.0f} {c[2]:.0f}x{c[3]:.0f}')
only_chrome = [k for k in chrome if k not in ours]
only_ours = [k for k in ours if k not in chrome]
mean = sum(w[0] for w in worst) / len(worst) if worst else 0
print(f'visible in both: {len(both)}, within {tolerance:g}px: {len(both) - len(off)}, mean worst-edge error {mean:.1f}px')
print(f'visible only in Chrome: {len(only_chrome)} {only_chrome[:6]}')
print(f'visible only here: {len(only_ours)} {only_ours[:6]}')
PY
