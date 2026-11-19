import { readFileSync } from 'node:fs'
import { randomBytes } from 'node:crypto'
import { Client, assert, integer, recordSettings, sql, stack, submission, ticket, eventually } from './client.mjs'

const mode = process.argv[2]
const smoke = mode === 'smoke'
const users = integer('LOAD_USERS', smoke ? 2 : 30, 1, 100)
const accounts = integer('LOAD_ACCOUNTS', smoke ? 12 : 300, Math.max(users, mode === 'recovery' ? 63 : 1), 10000)
const tickets = integer('LOAD_TICKETS', smoke ? 12 : 30000, 4, 500000)
const password = randomBytes(20).toString('hex')
const admin = new Client()
await admin.post(null, '/api/setup', {
  username: 'load-admin', password, email: 'load-admin@example.test', language: 'en',
})
if (!admin.cookie) await admin.login('load-admin', password, null)
const create = async (path, data) => (await admin.post(null, '/api/' + path, data, [201])).body.id
const direct = await create('categories', { name: 'Load desk', skip_ai: true })
const ai = await create('categories', { name: 'Load AI' })
const priority = await create('priorities', { name: 'Normal' })
await create('priorities', { name: 'Urgent' })

const historicAi = mode === 'profile' || mode === 'report-diagnostic' ? Math.min(48, tickets - 4, accounts) : 0
sql(readFileSync('ops/load/seed.sql', 'utf8'), [
  '-v', `accounts=${accounts}`, '-v', `operators=${users + Math.ceil(users / 3) * 3 + 1}`, '-v', `tickets=${tickets - historicAi}`,
])
const teams = sql('SELECT id FROM teams ORDER BY name').split('\n')
const put = (path, data) => admin.request(null, '/api/settings/' + path, { method: 'PUT', data })
if (!smoke) {
  await put('ai', { enabled: false, provider: 'openai', model: 'gpt-6-astra' })
  await put('ai/credentials/openai', { api_key: ['sk', 'load', randomBytes(20).toString('hex')].join('-') })
  await put('ai', { enabled: true, provider: 'openai', model: 'gpt-6-astra' })
}
// A small historical slice uses the full upload and AI flows, keeping its
// image files, immutable attempts and authorization history consistent.
for (let first = 0; first < historicAi; first += 4) {
  await Promise.all(Array.from({ length: Math.min(4, historicAi - first) }, async (_, offset) => {
    const index = first + offset
    const owner = await new Client().login('load-user-' + (1 + index), password, null)
    const screenshot = await owner.screenshot()
    const { body } = await owner.post(null, '/api/tickets',
      submission(ai, priority, 'Historical AI fixture ' + index, [screenshot]), [201])
    await eventually('historical AI suggestion', async () =>
      (await owner.get(null, ticket(body.id))).status === 'awaiting_ai_decision', 45)
    await owner.post(null, ticket(body.id) + '/actions', { action: 'ai_solved' })
  }))
}
assert.equal(Number(sql('SELECT count(*) FROM tickets')), tickets)
const history = JSON.parse(sql(`SELECT json_build_object(
  'ai_solved', count(*) FILTER (WHERE status='ai_solved'),
  'confirmed', count(*) FILTER (WHERE closure_reason='requester_confirmed' AND status='closed'),
  'awaiting_confirmation', count(*) FILTER (WHERE status='awaiting_confirmation'),
  'screenshot_bearing', (SELECT count(DISTINCT ticket_id) FROM screenshots))
  FROM tickets`))
if (process.env.LOAD_SMTP === '1') {
  await put('smtp', {
    host: 'smtp.load.test', port: 2465, security: 'implicit_tls', sender_name: 'Load desk',
    sender_address: 'help@example.test', username: 'load-relay', password, enabled: true,
  })
  const result = await admin.post(null, '/api/settings/smtp/test', { to: 'load-admin@example.test' }, [200, 502])
  assert.equal(result.code, 200, 'SMTP probe: ' + (result.body.error?.code ?? result.code))
}
recordSettings({ password, direct, ai, priority, teams, users, accounts, tickets, history })
assert.equal(JSON.parse(stack(['exec', '-T', '--index', '1', 'api', 'curl', '-fsS', 'http://localhost:3001/api/ready'])).status, 'ready')
