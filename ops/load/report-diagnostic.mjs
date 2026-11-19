import { Client, assert, eventually, measurements, percentiles, report, settings, sql, stackAsync } from './client.mjs'

const { password, tickets } = settings()
const admin = await new Client().login('load-admin', password, null)
async function sample() {
  const warm = await report(admin)
  assert.equal(warm.submissions, tickets)
  measurements.clear()
  for (let i = 0; i < 7; i++) assert.deepEqual(await report(admin), warm)
  return { report: warm, ms: percentiles(measurements.get('admin').successes) }
}

const baseline = await sample()
sql('ALTER ROLE tikkit IN DATABASE tikkit SET jit = off')
await stackAsync(['restart', 'api'])
await eventually('API ready', async () => (await admin.request(null, '/api/ready',
  { expect: [200, 502, 503, 'transport'] })).code === 200)
const diagnostic = await sample()
assert.deepEqual(diagnostic.report, baseline.report, 'changing JIT changes the report')
console.log(JSON.stringify({ seeded_tickets: tickets, default_jit_ms: baseline.ms, jit_off_ms: diagnostic.ms }, null, 2))
