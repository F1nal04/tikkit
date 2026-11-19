import {
  Client, assert, delay, eventually, events, propose, put, report, settings, sql, stack, stackAsync,
  submission, ticket,
} from './client.mjs'

const fixture = settings()
const { password, teams } = fixture
const admin = await new Client().login('load-admin', password, null)
const staff = await new Client().login('load-operator-1', password, null)
const probe = new Client()
const signIn = n => new Client().login('load-user-' + n, password, null)
const ready = () => eventually('readiness 200', async () => {
  const { code } = await probe.request(null, '/api/ready', { expect: [200, 502, 503, 504, 'transport'] })
  return code === 200
}, 90)
const outcome = {}
const db = (query, id) => sql(query, ['-v', 'id=' + id])
const attempts = id => JSON.parse(db(`SELECT row_to_json(a) FROM (
  SELECT state, claims, outcome, explanation IS NULL AS empty FROM ai_attempts WHERE ticket_id = :'id'
) a`, id))
const eventCount = (id, kind) => Number(sql('SELECT count(*) FROM ticket_events WHERE ticket_id = :\'id\' AND event_type = :\'kind\'',
  ['-v', 'id=' + id, '-v', 'kind=' + kind]))
const accepted = () => events('smtp').filter(([kind]) => kind === 'accepted').map(([, hash]) => hash)
const drain = () => eventually('outbox drain', () => sql("SELECT count(*) FROM notification_outbox WHERE status='pending'") === '0', 180)
const get = (actor, id) => actor.get(null, ticket(id))
const make = (ai, label) => submission(ai ? fixture.ai : fixture.direct, fixture.priority, 'Recovery ' + label)
const submit = async (actor, data) => (await actor.post(null, '/api/tickets', data, [201])).body
const awaitStatus = (actor, id, status) => eventually('ticket became ' + status, async () => (await get(actor, id)).status === status, 120)
const exactHandoff = async (actor, id) => {
  await awaitStatus(actor, id, 'queued')
  assert.equal(eventCount(id, 'ai_failed'), 1)
  assert.equal(eventCount(id, 'ai_solved'), 0)
  assert.equal((await get(staff, id)).status, 'queued')
  assert.equal(attempts(id).empty, true)
}

async function scenario(name, checks) {
  const start = performance.now()
  console.error('recovery:', name)
  try {
    const detail = await checks()
    outcome[name] = { passed: true, ms: Math.round(performance.now() - start), ...detail }
  } catch (error) {
    outcome[name] = { passed: false, ms: Math.round(performance.now() - start), failure: error.message }
    throw error
  }
}

try {
  await scenario('api_restart_mid_load', async () => {
    const owner = await signIn(1)
    put('provider', '3000')
    const job = await submit(owner, make(true, 'during graceful restart'))
    const before = await submit(owner, make(false, 'committed before restart'))
    const requesters = await Promise.all(Array.from({ length: 8 }, (_, i) => signIn(10 + i)))
    const pending = requesters.map((requester, i) => ({ requester, data: make(false, 'restart wave ' + i) }))
    const during = pending.map(async ({ requester, data }, i) => {
      await delay(150 * i)
      return requester.post(null, '/api/tickets', data, [201, 502, 503, 504, 'transport'])
    })
    await delay(25)
    const restarting = stackAsync(['restart', 'api'])
    const results = await Promise.allSettled([...during, restarting])
    for (const result of results) if (result.status === 'rejected') throw result.reason
    const receipts = results.slice(0, pending.length).map(result => result.value)
    assert(receipts.some(result => result.code !== 201), 'restart did not overlap traffic')
    await ready()
    assert.equal((await get(owner, before.id)).status, 'queued')
    assert.equal(eventCount(before.id, 'submit'), 1)
    assert(Number(db(`SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id=o.event_id
      WHERE e.ticket_id=:'id'`, before.id)) > 0)
    for (const [i, { requester, data }] of pending.entries()) {
      const replay = await requester.post(null, '/api/tickets', data, [200, 201])
      if (receipts[i].body) assert.deepEqual(replay.body, receipts[i].body)
      assert.equal(eventCount(replay.body.id, 'submit'), 1)
    }
    await awaitStatus(owner, job.id, 'awaiting_ai_decision')
    assert.equal(attempts(job.id).state, 'suggestion_ready')
    assert.equal(eventCount(job.id, 'ai_solved'), 0)
    put('provider', '250')
    return { committed: receipts.filter(result => result.code === 201).length, clean_failures: receipts.filter(result => result.code !== 201).length }
  })

  await scenario('postgres_restart', async () => {
    const owner = await signIn(2)
    const receipt = await submit(owner, make(false, 'before db restart'))
    const id = stack(['ps', '-q', 'api'])
    await stackAsync(['stop', 'postgres'])
    assert.equal((await probe.request(null, '/api/health')).code, 200)
    await probe.request(null, '/api/ready', { expect: [503] })
    await stackAsync(['start', 'postgres'])
    await ready()
    assert.equal(stack(['ps', '-q', 'api']), id)
    assert.equal((await get(owner, receipt.id)).status, 'queued')
    assert.equal(eventCount(receipt.id, 'submit'), 1)
  })

  await scenario('sigkill_ai_in_flight', async () => {
    put('provider', '8000')
    const owner = await signIn(3)
    const start = Date.now()
    const calls = events('provider').filter(([type]) => type === 'active').length
    const ai = await submit(owner, make(true, 'AI SIGKILL'))
    await eventually('provider received old request', () => events('provider').filter(([type]) => type === 'active').length > calls)
    assert.equal(attempts(ai.id).claims, 1)
    await stackAsync(['kill', '-s', 'SIGKILL', 'api'])
    assert.equal(attempts(ai.id).state, 'processing')
    put('provider', 'outage 80')
    await stackAsync(['start', 'api'])
    await ready()
    await exactHandoff(owner, ai.id)
    assert(attempts(ai.id).claims >= 2, 'attempt lease never recovered')
    assert(events('provider').some(([type]) => type === 'disconnected'), 'old response remained connected')
    // The crashed process cannot release the guard's separate usage lease.
    // Wait for the real 120 s call timeout plus 30 s margin before saturating
    // both slots in the next scenario.
    await eventually('crashed provider reservation expires', () => sql(
      "SELECT count(*) FROM ai_usage WHERE status='reserved'",
    ) === '0', 160, 3000)
    assert.equal(sql("SELECT count(*) FROM ai_usage WHERE outcome='abandoned'"), '1')
    return { usage_lease_released_after_ms: Date.now() - start }
  })

  await scenario('provider_slowness_and_outage', async () => {
    put('provider', '4000')
    const wave = await Promise.all(Array.from({ length: 8 }, (_, i) => signIn(20 + i)))
    const mark = events('provider').length
    const aiReceipts = await Promise.all(wave.map((owner, i) => submit(owner, make(true, 'slowness ' + i))))
    await eventually('two calls held by provider', () => events('provider').slice(mark).some(([type, n]) => type === 'active' && n === '2'))
    const direct = Date.now()
    const item = await submit(wave[0], make(false, 'direct during slowness'))
    assert.equal(item.status, 'queued')
    const directMs = Date.now() - direct
    assert(directMs < 1000, 'provider wait delayed direct submission')
    put('provider', 'outage 120')
    // The first two calls already in flight may succeed; subsequent ones should hand off.
    const breakerWave = await Promise.all(wave.map((owner, i) => submit(owner, make(true, 'outage ' + i))))
    const resolved = await Promise.all(aiReceipts.map((receipt, i) => eventually(
      'slow provider outcome', async () => {
        const status = (await get(wave[i], receipt.id)).status
        return status !== 'ai_processing' && status
      }, 120,
    )))
    for (const [i, status] of resolved.entries()) {
      assert(['queued', 'awaiting_ai_decision'].includes(status))
      if (status === 'queued') await exactHandoff(wave[i], aiReceipts[i].id)
    }
    await Promise.all(breakerWave.map((receipt, i) => exactHandoff(wave[i], receipt.id)))
    const peak = Math.max(0, ...events('provider').slice(mark).filter(([kind]) => kind === 'active').map(([, n]) => Number(n)))
    assert.equal(peak, Number(sql('SELECT max_concurrency FROM ai_limits')))
    const refusals = Number(sql("SELECT count(*) FROM ai_usage WHERE outcome='circuit_open'"))
    assert(refusals > 0, 'circuit breaker never refused a generation')
    return { peak_provider_calls: peak, circuit_open_refusals: refusals, direct_submit_ms: directMs }
  })

  if (process.env.LOAD_SMTP === '1') {
    await scenario('sigkill_unacknowledged_smtp', async () => {
      await drain()
      const before = accepted().length
      const held = events('smtp').filter(([kind]) => kind === 'held').length
      put('smtp', 'hold')
      const owner = await signIn(4)
      const receipt = await submit(owner, make(false, 'SMTP SIGKILL'))
      await eventually('relay received DATA without acknowledging it', () => events('smtp').filter(([kind]) => kind === 'held').length > held)
      await stackAsync(['kill', '-s', 'SIGKILL', 'api'])
      put('smtp', 'ready')
      await stackAsync(['start', 'api'])
      await ready()
      await drain()
      assert.equal(accepted().length, before + 1)
      assert.equal((await get(owner, receipt.id)).status, 'queued')
      assert.equal(eventCount(receipt.id, 'submit'), 1)
    })

    await scenario('sigkill_after_smtp_receipt', async () => {
      await drain()
      const before = accepted().length
      // Stop the worker between the independently committed SMTP receipt and
      // the outbox status commit. The trigger is confined to this disposable DB.
      sql(`CREATE FUNCTION load_pause_outbox_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN PERFORM pg_sleep(8); RETURN NEW; END $$;
        CREATE TRIGGER load_pause_outbox_commit BEFORE UPDATE ON notification_outbox
          FOR EACH ROW WHEN (NEW.channel = 'email' AND NEW.status = 'sent')
          EXECUTE FUNCTION load_pause_outbox_commit();`)
      const owner = await signIn(6)
      const receipt = await submit(owner, make(false, 'durable SMTP receipt'))
      await eventually('receipt durable, outbox uncommitted', () => Number(db(`
        SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id=o.event_id
        JOIN smtp_deliveries d ON d.idempotency_key='notification:' || o.id
        WHERE e.ticket_id=:'id' AND o.channel='email' AND o.status='pending'`, receipt.id)) === 1)
      await stackAsync(['kill', '-s', 'SIGKILL', 'api'])
      sql('DROP TRIGGER load_pause_outbox_commit ON notification_outbox; DROP FUNCTION load_pause_outbox_commit()')
      await stackAsync(['start', 'api'])
      await ready()
      await drain()
      assert.equal(accepted().length, before + 1, 'durable SMTP receipt was sent again')
    })

    await scenario('mail_outage_retry', async () => {
      put('smtp', 'reject')
      const start = Date.now()
      const owner = await signIn(5)
      const before = accepted().length
      const receipt = await submit(owner, make(false, 'mail outage'))
      await eventually('email retry scheduled', () => Number(db(`
        SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id=o.event_id
        WHERE e.ticket_id=:'id' AND o.channel='email' AND o.attempts=1 AND o.status='pending'
          AND o.available_at > clock_timestamp() + interval '20 seconds'`, receipt.id)) === 1)
      put('smtp', 'ready')
      await drain()
      assert.equal(accepted().length, before + 1)
      const elapsed = Date.now() - start
      assert(elapsed >= 29_000, 'SMTP retry ignored backoff')
      return { accepted_after_ms: elapsed }
    })
  }

  await scenario('overdue_proposals_two_workers', async () => {
    const users = await Promise.all(Array.from({ length: 12 }, (_, i) => signIn(50 + i)))
    const proposals = []
    const reportBefore = await report(admin)
    const alternate = await new Client().login('load-operator-4', password, null)
    for (const [i, owner] of users.entries()) {
      const receipt = await submit(owner, make(false, 'overdue ' + i))
      const version = await propose(i % 2 ? staff : alternate, null, teams[0], receipt.id)
      proposals.push({ id: receipt.id, version })
    }
    await drain()
    await stackAsync(['stop', 'api'])
    const schedule = (entries, offset) => sql(`
      UPDATE tickets SET confirmation_deadline = clock_timestamp() + make_interval(secs => :offset)
      WHERE id = ANY(string_to_array(:'ids', ',')::uuid[])`,
    ['-v', 'ids=' + entries.map(entry => entry.id).join(','), '-v', 'offset=' + offset])
    schedule(proposals.slice(0, 6), -1)
    // Both replicas start against one DB. Compose owns the second container and its lifecycle.
    await stackAsync(['up', '-d', '--no-deps', '--scale', 'api=2', 'api'])
    await ready()
    const replicas = stack(['ps', '-q', 'api']).split('\n')
    assert.equal(replicas.length, 2)
    for (const index of [1, 2]) {
      await eventually('worker replica ready', () => {
        try {
          return JSON.parse(stack(['exec', '-T', '--index', String(index), 'api', 'curl', '-fsS',
            'http://localhost:3001/api/ready'])).status === 'ready'
        } catch { return false }
      })
    }
    schedule(proposals.slice(6), 1)
    const choices = proposals.slice(6).map(async ({ id, version }, i) => {
      await delay([700, 950, 1100, 1400, 2000, 3500][i])
      const raceStart = Date.now()
      while (Date.now() - raceStart < 30_000) {
        const result = await users[i + 6].post(null, ticket(id) + '/actions',
          { action: 'accept_resolution', proposal_version: version }, [200, 409, 502, 503, 'transport'])
        if ([200, 409].includes(result.code)) return result.code
        await delay(100)
      }
      assert.fail('requester race never got a decision')
    })
    const choiceCodes = await Promise.all(choices)
    for (const [i, { id }] of proposals.entries()) {
      await awaitStatus(users[i], id, 'closed')
      const auto = eventCount(id, 'confirmation_timeout')
      const manual = eventCount(id, 'accept_resolution')
      assert.equal(auto + manual, 1)
      assert.equal(Number(db("SELECT count(*) FROM ticket_messages WHERE ticket_id=:'id'", id)), 2)
      assert.equal(auto, i < 6 || choiceCodes[i - 6] === 409 ? 1 : 0)
      assert.equal(Number(db(`SELECT count(*) FROM notification_outbox o JOIN ticket_events e ON e.id=o.event_id
        WHERE e.ticket_id=:'id' AND e.event_type IN ('accept_resolution','confirmation_timeout') AND o.channel='in_app'`, id)), 1)
    }
    await drain()
    const after = await report(admin)
    const acceptedRaces = choiceCodes.filter(code => code === 200).length
    assert(acceptedRaces > 0 && acceptedRaces < choiceCodes.length, 'deadline races need both winning paths')
    assert.equal(after.operator.initial_confirmation_timeout - reportBefore.operator.initial_confirmation_timeout, 12 - acceptedRaces)
    assert.equal(after.operator.initial_requester_confirmed - reportBefore.operator.initial_requester_confirmed, acceptedRaces)
    await stackAsync(['up', '-d', '--no-deps', '--scale', 'api=1', 'api'])
    return { replicas: replicas.length, timed_out: 12 - acceptedRaces, requester_confirmed: acceptedRaces }
  })

  await scenario('retention_catches_up_after_restart', async () => {
    // A harmless empty run gets old while the worker is offline. Its policy keeps all user data.
    await stackAsync(['stop', 'api'])
    const id = sql(`INSERT INTO retention_runs (as_of, draft_days, created_at)
      VALUES (clock_timestamp(), 3650, clock_timestamp() - interval '5 seconds') RETURNING id`)
    await stackAsync(['start', 'api'])
    await ready()
    await eventually('retention run completion', () => db("SELECT state FROM retention_runs WHERE id=:'id'", id) === 'completed')
    const lag = Number(db("SELECT extract(epoch FROM (finished_at-created_at)) FROM retention_runs WHERE id=:'id'", id))
    assert(lag < 30)
    return { processing_lag_seconds: lag }
  })

  const hashes = accepted()
  if (process.env.LOAD_SMTP === '1') {
    assert.equal(hashes.length, 1 + Number(sql(
      "SELECT count(*) FROM notification_outbox WHERE status='sent' AND channel='email'",
    )), 'SMTP captures plus setup probe')
  }
  assert.equal(hashes.length, new Set(hashes).size, 'duplicate relay acceptance')
  assert.equal(sql(`SELECT count(*) FROM (SELECT event_id, account_id, channel
    FROM notification_outbox GROUP BY 1,2,3 HAVING count(*) > 1) duplicates`), '0')
  outcome.reconciliation = { accepted_mail: hashes.length, duplicate_mail: 0, duplicate_outbox: 0 }
} finally {
  console.log(JSON.stringify(outcome, null, 2))
}
