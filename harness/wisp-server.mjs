// WISP relay for the sandbox's outbound TCP.
//
// The browser sandbox has no direct network access, so egress goes over a
// WebSocket relay that opens the real TCP connection on its own side. This is
// the same arrangement wasmer-wasix-pi uses.
//
//   node wisp-server.mjs            # listens on 127.0.0.1:5001
//
// wisp-js refuses private and loopback destinations by default, which is why
// the outbound probe points at a public host.
import http from 'node:http'
import { server as wisp, logging } from '@mercuryworkshop/wisp-js/server'

const PORT = Number(process.env.PORT ?? 5001)
const HOST = process.env.HOST ?? '127.0.0.1'

logging.set_level(logging.INFO)

const server = http.createServer((_req, res) => res.writeHead(404).end())
server.on('upgrade', (req, socket, head) => wisp.routeRequest(req, socket, head))
server.listen(PORT, HOST, () => console.log(`wisp listening on ${HOST}:${PORT}`))
