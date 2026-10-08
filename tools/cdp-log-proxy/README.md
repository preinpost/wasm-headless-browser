# cdp-log-proxy

A logging proxy that sits between a CDP client and a CDP server, appending every
HTTP path and every WebSocket frame to `frames.jsonl`. It is how
`docs/capture/frames.jsonl` was taken, and it is the fastest way to find out
what a client actually needs from an engine.

```bash
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
"$CHROME" --headless=new --remote-debugging-port=9222 \
  --user-data-dir=/tmp/cdpprofile about:blank &

bun run tools/cdp-log-proxy/proxy.ts          # listens on 9223, reads LOG paths

agent-browser --cdp http://127.0.0.1:9223 open https://example.com
agent-browser --cdp http://127.0.0.1:9223 snapshot
```

Edit the `LOG` constant at the top of `proxy.ts` to change the output path, and
`9200`-series ports to move either end. The upstream endpoint is discovered from
`http://127.0.0.1:9222/json/version` rather than hard-coded.
