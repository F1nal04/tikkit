// Same-origin gateway for the built web app and the test-only Axum executable.
import { spawn } from 'node:child_process'
import { createServer, request } from 'node:http'
import { once } from 'node:events'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../../..', import.meta.url))
const api = spawn('timeout', [
  '900', 'cargo', 'test', '--locked', '-p', 'tikkit-api', '--test', 'workflow_server', '--', '--serve-workflows',
], { cwd: root, stdio: ['pipe', 'inherit', 'inherit'] })
const web = spawn(process.execPath, ['build/index.js'], {
  stdio: ['ignore', 'inherit', 'inherit'],
  env: {
    ...process.env,
    HOST: '127.0.0.1',
    PORT: '18502',
    API_ORIGIN: 'http://127.0.0.1:18501',
    PROTOCOL_HEADER: 'x-forwarded-proto',
  },
})

const gateway = createServer((incoming, outgoing) => {
  const path = incoming.url ?? '/'
  const port = path.startsWith('/api/') ? 18501 : 18502
  const upstream = request({
    hostname: '127.0.0.1',
    port,
    path,
    method: incoming.method,
    headers: { ...incoming.headers, 'x-forwarded-proto': 'http' },
  }, (response) => {
    outgoing.writeHead(response.statusCode ?? 502, response.rawHeaders)
    response.pipe(outgoing)
  })
  upstream.on('error', () => {
    if (!outgoing.headersSent) outgoing.writeHead(503)
    outgoing.end()
  })
  outgoing.on('close', () => upstream.destroy())
  incoming.pipe(upstream)
})

let stopping = false
/** @param {number} code */
async function stop(code) {
  if (stopping) return
  stopping = true
  gateway.close()
  gateway.closeAllConnections()
  const children = [api, web].filter((child) => child.exitCode === null && child.signalCode === null)
  const exited = children.map((child) => once(child, 'exit'))
  api.stdin.end()
  web.kill()
  await Promise.all(exited)
  process.exitCode = code
}
api.stdin.on('error', () => {})
for (const child of [api, web]) child.on('exit', () => void stop(1))
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => void stop(0))
gateway.listen(18500, '127.0.0.1')
