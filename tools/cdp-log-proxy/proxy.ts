// CDP logging proxy: 9223 -> 9222. Logs every HTTP path and every WS frame.
import { appendFileSync, writeFileSync } from "node:fs";

const LOG = "/tmp/cdpcap/frames.jsonl";
const CHROME = "http://127.0.0.1:9222";
writeFileSync(LOG, "");

const ver = await (await fetch(`${CHROME}/json/version`)).json();
const UP = ver.webSocketDebuggerUrl as string;
console.log("upstream:", UP);

type Sock = {
  up?: WebSocket;
  queue: string[];
};

Bun.serve({
  port: 9223,
  idleTimeout: 0,
  async fetch(req, server) {
    const url = new URL(req.url);
    if (server.upgrade(req, { data: { queue: [] } })) return;
    appendFileSync(LOG, JSON.stringify({ http: req.method + " " + url.pathname + url.search }) + "\n");
    const r = await fetch(`${CHROME}${url.pathname}${url.search}`, {
      method: req.method,
      headers: req.headers,
      body: req.method === "GET" || req.method === "HEAD" ? undefined : await req.text(),
    });
    return new Response(await r.arrayBuffer(), {
      status: r.status,
      headers: { "content-type": r.headers.get("content-type") ?? "application/json" },
    });
  },
  websocket: {
    open(ws) {
      const s = ws.data as Sock;
      appendFileSync(LOG, JSON.stringify({ ws: "open" }) + "\n");
      const up = new WebSocket(UP);
      s.up = up;
      up.onopen = () => {
        for (const m of s.queue.splice(0)) up.send(m);
      };
      up.onmessage = (e) => {
        appendFileSync(LOG, JSON.stringify({ dir: "<<", data: e.data }) + "\n");
        try {
          ws.send(e.data as string);
        } catch {}
      };
      up.onclose = () => { try { ws.close(); } catch {} };
      up.onerror = () => { try { ws.close(); } catch {} };
    },
    message(ws, msg) {
      const s = ws.data as Sock;
      const t = typeof msg === "string" ? msg : new TextDecoder().decode(msg);
      appendFileSync(LOG, JSON.stringify({ dir: ">>", data: t }) + "\n");
      if (s.up && s.up.readyState === WebSocket.OPEN) s.up.send(t);
      else s.queue.push(t);
    },
    close(ws) {
      appendFileSync(LOG, JSON.stringify({ ws: "close" }) + "\n");
      (ws.data as Sock).up?.close();
    },
  },
});

console.log("proxy on 9223");
