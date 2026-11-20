import { Client, assert, eventually, measurements, percentiles, report, settings, sql, stackAsync } from './client.mjs'

const { password, tickets } = settings()
assert(tickets >= 10000, 'the CSV diagnostic needs at least 10,000 tickets')
const actors = await Promise.all([1, 2, 3].map(n => new Client().login('load-export-' + n, password, null)))
const [from, before] = sql(`SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
  FROM (SELECT created_at, row_number() OVER (ORDER BY created_at) n FROM tickets) t
  WHERE n IN (1, 10001) ORDER BY n`).split('\n')
const range = new URLSearchParams({ created_from: from, created_before: before ?? new Date().toISOString() })

async function exports(actor, record) {
  for (const [label, path] of [
    ['report_csv', '/api/admin/reports/export?' + range], ['audit_csv', '/api/admin/audit/export'],
  ]) {
    const { body } = await actor.request(record ? label : null, path, { method: 'POST', csv: true })
    assert.equal(body.rows, 10000, label)
  }
}

async function sample() {
  const warm = await report(actors[0], null)
  assert.equal(warm.submissions, tickets)
  await exports(actors[2], false)
  measurements.clear()
  for (let i = 0; i < 8; i++) await exports(actors[i % actors.length], true)
  return { report: warm, timings: Object.fromEntries(
    Array.from(measurements, ([group, values]) => [group, percentiles(values.successes)])) }
}

const baseline = await sample()
sql('ALTER ROLE tikkit IN DATABASE tikkit SET jit = off')
await stackAsync(['restart', 'api'])
await eventually('API ready', async () => (await actors[0].request(null, '/api/ready',
  { expect: [200, 502, 503, 'transport'] })).code === 200)
const diagnostic = await sample()
assert.deepEqual(diagnostic.report, baseline.report, 'changing JIT changes the report')
console.log(JSON.stringify({ seeded_tickets: tickets, rows_per_export: 10000,
  default_ms: baseline.timings, jit_off_ms: diagnostic.timings }, null, 2))
