// Stands in for the Rust API during browser tests: only what the pages call.
import { createServer } from 'node:http'

createServer((req, res) => {
  const ok = req.url === '/api/health'
  res.writeHead(ok ? 200 : 404, { 'content-type': 'application/json' })
  res.end(JSON.stringify(ok ? { status: 'ok' } : { error: { code: 'not_found' } }))
}).listen(Number(process.env.PORT), '127.0.0.1')
