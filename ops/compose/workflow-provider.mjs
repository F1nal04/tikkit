// Deterministic HTTPS peer for the production API in the isolated Compose test.
import { readFileSync } from 'node:fs'
import { createServer } from 'node:https'

const tls = {
  cert: readFileSync('/workflow-tls/stub.crt'),
  key: readFileSync('/workflow-tls/stub.key'),
}
const answer = {
  en: {
    explanation: 'The application profile may be damaged.',
    steps: ['Close the application.', 'Open it again.'],
  },
  de: {
    explanation: 'Das Anwendungsprofil ist möglicherweise beschädigt.',
    steps: ['Anwendung schließen.', 'Anwendung neu öffnen.'],
  },
}
createServer(tls, async (request, response) => {
  if (request.headers.host !== 'api.openai.com' || request.method !== 'POST' || request.url !== '/v1/responses') {
    response.writeHead(404).end()
    return
  }
  try {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const send = (status, result) => response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(result))
    if (JSON.stringify(body.input).includes('Compose provider rejection fixture')) {
      send(401, { error: { code: 'invalid_api_key', message: 'Test credential rejected' } })
      return
    }
    const locale = body.instructions.includes('in German') ? 'de' : 'en'
    send(200, {
      status: 'completed',
      output: [{
        type: 'message', role: 'assistant', status: 'completed',
        content: [{ type: 'output_text', text: JSON.stringify(answer[locale]) }],
      }],
    })
  } catch {
    response.writeHead(400).end()
  }
}).listen(443, '0.0.0.0')
