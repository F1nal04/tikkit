import {
  Client, assert, delay, eventually, events, integer, measurements, percentiles,
  propose, report, settings, sql, submission, ticket,
} from './client.mjs'
import { sampler } from './observe.mjs'

const smoke = process.argv[2] === 'smoke'
const fixture = settings()
const { password, users, teams, accounts, tickets } = fixture
const seconds = integer('LOAD_SECONDS', smoke ? 5 : 300, 2, 600)
const warmupSeconds = smoke ? 0 : integer('LOAD_WARMUP_SECONDS', 12, 0, 60)
const cycleMs = Math.max(smoke ? 2000 : 6000, Math.ceil(users / accounts * 30000))
const admin = await new Client().login('load-admin', password, null)
let sentinelTicket
const visibilityLead = smoke ? null : await new Client().login('load-lead-1', password, null)
const assignTarget = smoke ? null : sql("SELECT id FROM accounts WHERE username='load-operator-1'")
const expected = { submitted: 0, ai: 0, ai_solved: 0, confirmed: 0, public_replies: 0, notes: 0, attachments: 0, claims: 0 }
const checks = { submit_races: 0, claim_races: 0, confirmation_races: 0, ai_decision_races: 0, operator_passes: 0 }
let running = true

async function submit(requester, category, subject, shot) {
  const data = submission(category, fixture.priority, subject, shot ? [shot] : [])
  const receipts = await Promise.all([1, 2].map(
    () => requester.post('submission', '/api/tickets', data, [200, 201]),
  ))
  assert.deepEqual(receipts.map(r => r.code).sort(), [200, 201], 'one submission committed')
  assert.deepEqual(receipts[0].body, receipts[1].body, 'one submission receipt')
  checks.submit_races++
  expected.submitted++
  expected.attachments += Number(Boolean(shot))
  return receipts[0].body
}

async function decide(requester, receipt, action) {
  expected.ai++
  const detail = await eventually('AI suggestion', async () => {
    const result = await requester.get('requester_reads', ticket(receipt.id))
    assert(result.status !== 'queued', 'AI unexpectedly handed off during steady profile')
    return result.status === 'awaiting_ai_decision' && result
  }, 30, 500)
  const quietCount = async () => (await visibilityLead.get(null, '/api/queue/counts')).mine
  const before = action === 'ai_solved' ? await quietCount() : null
  const responses = await Promise.all([1, 2].map(
    () => requester.post('staff_writes', ticket(receipt.id) + '/actions', { action }),
  ))
  assert.deepEqual(responses[0].body, responses[1].body, 'one AI decision replayed')
  checks.ai_decision_races++
  if (action === 'ai_solved') {
    assert.equal(await quietCount(), before, 'AI-solved changed the Team Lead personal queue count')
    expected.ai_solved++
    return { ...receipt, updated: detail.updated_at, screenshot: detail.intake_snapshot?.screenshot_ids[0] }
  }
}

async function verifyHidden(actor, entry) {
  for (const suffix of ['', '/activity', '/messages']) {
    await actor.request('queue_staff_detail', ticket(entry.id) + suffix, { expect: [404] })
  }
  if (entry.screenshot) for (const action of ['preview', 'download']) {
    await actor.request('queue_staff_detail', '/api/screenshots/' + entry.screenshot + '/' + action, { expect: [404] })
  }
  for (const view of ['general', 'team', 'mine']) {
    const query = 'view=' + view + '&q=%23' + entry.reference
    const result = await actor.get('queue_staff_detail', '/api/queue?' + query)
    assert.deepEqual(result.items, [], 'AI-solved reference appeared in the queue')
    assert(!(await actor.get('queue_staff_detail', '/api/queue?view=' + view)).items.some(item => item.status === 'ai_solved'))
  }
  const inbox = await actor.get('queue_staff_detail', '/api/notifications')
  assert(!inbox.items.some(item => item.ticket_id === entry.id), 'AI-solved notification exposed to staff')
  checks.operator_passes++
}

async function verifyWrites(actor, entry) {
  const target = { action: 'assign', assignee_account_id: assignTarget }
  for (const change of [{ action: 'claim' }, target]) {
    await actor.post(null, ticket(entry.id) + '/assignment',
      { ...change, expected_updated_at: entry.updated }, [404])
    const { body } = await actor.post(null, '/api/tickets/bulk', {
      change, tickets: [{ id: entry.id, expected_updated_at: entry.updated }],
    })
    assert.equal(body.results[0].error, 'not_found', 'bulk action exposed AI-solved record')
  }
}

if (!smoke) {
  // Establish an AI-solved screenshot sentinel before starting the load clock.
  const sentinel = await new Client().login('load-user-' + accounts, password, null)
  const visibilityActor = await new Client().login('load-operator-1', password, null)
  const counts = () => visibilityActor.get(null, '/api/queue/counts')
  const before = await counts()
  const screenshot = await sentinel.screenshot()
  const receipt = await submit(sentinel, fixture.ai, 'Load AI visibility sentinel', screenshot)
  sentinelTicket = { ...await decide(sentinel, receipt, 'ai_solved'), screenshot }
  const after = await counts()
  for (const view of ['general', 'team', 'mine']) assert.equal(after[view], before[view], 'AI-solved changed operator counts')
}
if (warmupSeconds) {
  const end = Date.now() + warmupSeconds * 1000
  while (Date.now() < end) {
    await report(admin, null)
    await visibilityLead.get(null, '/api/queue?view=general&limit=25&q=VPN')
    await admin.get(null, '/api/admin/submissions?limit=25')
    await admin.get(null, '/api/admin/audit?limit=25')
    await delay(Math.max(0, Math.min(1000, end - Date.now())))
  }
}
measurements.clear()
const providerMark = events('provider').length
const stopSampling = smoke ? async () => ({}) : sampler()
const driverStart = process.cpuUsage()
const start = Date.now()
const expires = start + seconds * 1000
let driverRss = process.memoryUsage.rss()
const lanes = []
const adminLane = { scheduled: smoke ? 1 : Math.ceil(seconds / 3), completed: 0 }
const adminReads = (async () => {
  for (let slot = 0; slot < adminLane.scheduled && running; slot++) {
    const due = start + slot * 3000
    await delay(Math.max(0, due - Date.now()))
    if (Date.now() >= expires || Date.now() - due >= 3000) continue
    await report(admin)
    await admin.get('admin_submissions', '/api/admin/submissions?limit=25')
    await admin.get('admin_audit', '/api/admin/audit?limit=25')
    adminLane.completed++
  }
})()
adminReads.catch(() => { running = false })
const observer = (async () => {
  if (smoke) return
  const actor = await new Client().login('load-operator-' + (users + Math.ceil(users / 3) * 3 + 1), password, null)
  {
    const lead = await new Client().login('load-lead-2', password, null)
    // Reuse the sessions on alternating passes to stay below normal budgets.
    while (running) {
      await verifyHidden(actor, sentinelTicket)
      await delay(4500)
      if (running) await verifyHidden(lead, sentinelTicket)
      await delay(4500)
    }
  }
})()
observer.catch(() => { running = false })

async function lane(index) {
  const offset = Math.floor(index * (smoke ? 100 : 160))
  const stats = { lane: index + 1, scheduled: Math.max(0, Math.ceil((seconds * 1000 - offset) / cycleMs)), completed: 0 }
  lanes.push(stats)
  await delay(Math.max(0, start + offset - Date.now()))
  const operator = await new Client().login('load-operator-' + (index + 1), password)
  const competitor = await new Client().login('load-operator-' + (1 + index + Math.ceil(users / 3) * 3), password, null)
  for (let cycle = 0; running && cycle < stats.scheduled; cycle++) {
    const due = start + offset + cycle * cycleMs
    await delay(Math.max(0, due - Date.now()))
    if (Date.now() >= expires || Date.now() - due >= cycleMs) continue
    const n = cycle * users + index
    const owner = await new Client().login('load-user-' + (1 + n % accounts), password)
    const ai = !smoke && n % 6 === 0
    const screenshot = n % 5 === 0 ? await owner.screenshot() : null
    await owner.get('requester_reads', '/api/tickets?limit=25')
    const list = await operator.get('queue_staff_detail', '/api/queue?view=general&limit=25')
    if (list.next_cursor) {
      await operator.get('queue_staff_detail', '/api/queue?view=general&limit=25&cursor=' + encodeURIComponent(list.next_cursor))
    }
    await operator.get('queue_staff_detail', '/api/queue?view=general&status=queued&limit=25&priority_id=' + fixture.priority + '&q=' +
      encodeURIComponent(['VPN', 'Printer', 'replacement profile'][n % 3]))
    if (list.items.length) await operator.get('queue_staff_detail', ticket(list.items[0].id))
    const receipt = await submit(owner, ai ? fixture.ai : fixture.direct, 'Live load ' + n, screenshot)
    const path = ticket(receipt.id)
    await owner.get('requester_reads', path)
    if (ai) {
      const final = await decide(owner, receipt, n % 12 === 0 ? 'ai_solved' : 'ai_not_solved')
      if (final) {
        await verifyHidden(operator, final)
        await verifyWrites(operator, final)
        // The dedicated Lead observer checks sentinel reads throughout the run.
        await verifyWrites(visibilityLead, final)
      }
    }
    if (!ai && (smoke || (cycle + index) % 4 === 1)) {
      const version = await propose(operator, competitor, teams[index % 3], receipt.id)
      checks.claim_races++
      expected.claims++
      expected.public_replies++
      expected.notes++
      const decisions = await Promise.all([1, 2].map(() => owner.post('staff_writes', path + '/actions', {
        action: 'accept_resolution', proposal_version: version,
      }, [200, 409])))
      assert.deepEqual(decisions.map(r => r.code).sort(), [200, 409], 'one confirmation committed')
      checks.confirmation_races++
      expected.confirmed++
    }
    stats.completed++
    driverRss = Math.max(driverRss, process.memoryUsage.rss())
  }
}

const outcomes = await Promise.allSettled(Array.from({ length: users }, (_, i) => lane(i)))
const adminOutcome = await Promise.allSettled([adminReads])
const cpu = process.cpuUsage(driverStart)
const driver = { cpu_seconds: (cpu.user + cpu.system) / 1e6, peak_sampled_rss_bytes: driverRss,
  mean_cores: (cpu.user + cpu.system) / ((Date.now() - start) * 1000) }
running = false
const [visibility, observed] = await Promise.allSettled([observer, stopSampling()])
const result = {
  clock_seconds: (Date.now() - start) / 1000, users, seconds, accounts, seeded_tickets: tickets,
  configured_provider_ms: Number(process.env.LOAD_PROVIDER_MS),
  smtp_enabled: process.env.LOAD_SMTP === '1',
  groups: Object.fromEntries(Array.from(measurements, ([group, values]) =>
    [group, { ...percentiles(values.successes), errors: values.errors }])),
  warmup_seconds: warmupSeconds, history: fixture.history, driver, lanes, admin_lane: adminLane,
  checks, expected, resources: observed.value,
}
try {
  for (const outcome of [...outcomes, ...adminOutcome, visibility, observed]) if (outcome.status === 'rejected') throw outcome.reason
  const adminSamples = ['admin_report', 'admin_submissions', 'admin_audit']
    .flatMap(group => measurements.get(group)?.successes ?? [])
  result.groups.admin_combined = percentiles(adminSamples)
  const totals = await report(admin, null)
  assert.equal(totals.submissions, tickets + expected.submitted)
  assert.equal(totals.ai_attempts, fixture.history.ai_solved + expected.ai)
  assert.equal(totals.ai_rate_numerator ?? 0, fixture.history.ai_solved + expected.ai_solved)
  assert.equal(totals.operator.queued, tickets - fixture.history.ai_solved + expected.submitted - expected.ai_solved)
  assert.equal(totals.operator.initial_requester_confirmed, fixture.history.confirmed + expected.confirmed)
  for (const buckets of [totals.daily, totals.categories, totals.priorities]) {
    assert.equal(buckets.reduce((sum, entry) => sum + entry.count, 0), totals.submissions)
  }
  await eventually('outbox drain', () =>
    sql("SELECT count(*) FROM notification_outbox WHERE status='pending'") === '0', 180)
  result.reconciliation = JSON.parse(sql(`
    WITH live AS (SELECT id, status FROM tickets WHERE subject LIKE 'Live load %' OR subject = 'Load AI visibility sentinel'),
    ev AS (SELECT e.* FROM ticket_events e JOIN live t ON t.id=e.ticket_id)
    SELECT json_build_object(
      'submitted', (SELECT count(*) FROM live),
      'submit_keys', (SELECT count(*) FROM ticket_submission_keys WHERE ticket_id IN (SELECT id FROM live)),
      'submit_events', (SELECT count(*) FROM ev WHERE event_type='submit'),
      'ai_solved', (SELECT count(*) FROM ev WHERE event_type='ai_solved'),
      'ai_not_solved', (SELECT count(*) FROM ev WHERE event_type='ai_not_solved'),
      'reply_events', (SELECT count(*) FROM ev WHERE event_type='public_reply_added'),
      'note_events', (SELECT count(*) FROM ev WHERE event_type='internal_note_added'),
      'ai', (SELECT count(*) FROM ai_attempts WHERE ticket_id IN (SELECT id FROM live)),
      'confirmed', (SELECT count(*) FROM ev WHERE event_type='accept_resolution'),
      'public_replies', (SELECT count(*) FROM ticket_messages WHERE kind='public_reply' AND ticket_id IN (SELECT id FROM live)),
      'notes', (SELECT count(*) FROM ticket_messages WHERE kind='internal_note' AND ticket_id IN (SELECT id FROM live)),
      'attachments', (SELECT count(*) FROM screenshots WHERE ticket_id IN (SELECT id FROM live)),
      'claims', (SELECT count(*) FROM ev WHERE event_type='assignment_changed' AND payload->>'assignee_account_id' IS NOT NULL),
      'duplicate_notifications', (SELECT count(*) FROM (SELECT event_id, account_id, channel
        FROM notification_outbox GROUP BY 1,2,3 HAVING count(*) > 1) d),
      'ai_solved_staff_notifications', (SELECT count(*) FROM notification_outbox o
        JOIN ev e ON e.id=o.event_id JOIN live t ON t.id=e.ticket_id
        WHERE t.status='ai_solved' AND o.audience='staff'),
      'emails_sent', (SELECT count(*) FROM notification_outbox WHERE status='sent' AND channel='email'),
      'outbox_pending', (SELECT count(*) FROM notification_outbox WHERE status='pending'),
      'ai_duration_ms', (SELECT json_build_object('p50', percentile_disc(.5) WITHIN GROUP (ORDER BY ms),
         'p95', percentile_disc(.95) WITHIN GROUP (ORDER BY ms), 'p99', percentile_disc(.99) WITHIN GROUP (ORDER BY ms),
         'max', max(ms)) FROM (SELECT extract(epoch FROM finished_at-created_at)*1000 AS ms FROM ai_attempts WHERE ticket_id IN (SELECT id FROM live)) durations)
    )`))
  for (const [name, count] of Object.entries(expected)) assert.equal(result.reconciliation[name], count, name)
  assert.equal(result.reconciliation.submit_keys, expected.submitted)
  assert.equal(result.reconciliation.submit_events, expected.submitted)
  assert.equal(result.reconciliation.ai_not_solved, expected.ai - expected.ai_solved)
  assert.equal(result.reconciliation.reply_events, expected.public_replies)
  assert.equal(result.reconciliation.note_events, expected.notes)
  assert.equal(result.reconciliation.duplicate_notifications, 0)
  assert.equal(result.reconciliation.ai_solved_staff_notifications, 0)
  const accepted = events('smtp').filter(([kind]) => kind === 'accepted').map(([, hash]) => hash)
  assert.equal(accepted.length, new Set(accepted).size, 'duplicate captured mail')
  if (process.env.LOAD_SMTP === '1') {
    assert.equal(accepted.length, result.reconciliation.emails_sent + 1, 'SMTP receipts plus setup probe')
    assert(result.reconciliation.emails_sent > 0, 'SMTP received no ticket notification')
    result.smtp_accepted = accepted.length
  }
  result.provider_duration_ms = percentiles(events('provider').slice(providerMark).filter(([kind]) => kind === 'duration_ms').map(([, ms]) => Number(ms)))
  result.result = 'pass'
} finally {
  console.log(JSON.stringify(result, null, 2))
}
