import assert from 'node:assert/strict'
import { readFileSync, writeFileSync, renameSync } from 'node:fs'
import { randomUUID } from 'node:crypto'
import { setTimeout as delay } from 'node:timers/promises'
import { screenshot } from './screenshot.mjs'

export { assert, delay }
export const base = process.env.LOAD_BASE
export const work = process.env.LOAD_WORK
const ca = readFileSync(work + '/portal-ca.crt', 'utf8')
let png
export const stack = (args, stdin) => command(['bash', '-c', '. ops/load/stack.sh; stack "$@"', 'stack', ...args], stdin)
export async function stackAsync(args) {
  const child = Bun.spawn(['bash', '-c', '. ops/load/stack.sh; stack "$@"', 'stack', ...args],
    { stdout: 'ignore', stderr: 'inherit' })
  assert.equal(await child.exited, 0, `Compose ${args[0]} failed`)
}
export const sql = (statement, args = []) => command(['bash', '-c', '. ops/load/stack.sh; sql "$@"', 'sql', ...args], statement)
export function command(args, stdin) {
  const completed = Bun.spawnSync(args, { stdin: stdin === undefined ? 'inherit' : Buffer.from(stdin), stderr: 'inherit' })
  assert.equal(completed.exitCode, 0, `command failed: ${args[0]}`)
  return completed.stdout.toString().trim()
}
export function put(name, value) {
  writeFileSync(`${work}/control/${name}.tmp`, value + '\n')
  renameSync(`${work}/control/${name}.tmp`, `${work}/control/${name}`)
}
export function events(name) {
  try { return readFileSync(`${work}/control/${name}-events`, 'utf8').trim().split('\n').map(line => line.split(' ')) }
  catch (error) { if (error.code !== 'ENOENT') throw error; return [] }
}
export function integer(name, value, min, max) {
  const n = Number(process.env[name] ?? value)
  assert(Number.isSafeInteger(n) && n >= min && n <= max, `${name} must be between ${min} and ${max}`)
  return n
}
export const settings = () => JSON.parse(readFileSync(work + '/fixture.json', 'utf8'))
export function recordSettings(data) { writeFileSync(work + '/fixture.json', JSON.stringify(data)) }

export const measurements = new Map()
export function percentiles(values) {
  if (!values.length) return { count: 0 }
  const ordered = [...values].sort((a, b) => a - b)
  const quantile = q => Math.round(ordered[Math.ceil(q * ordered.length) - 1] * 10) / 10
  return { count: values.length, p50: quantile(.5), p95: quantile(.95), p99: quantile(.99), max: quantile(1) }
}
function record(group, duration, code, expected) {
  if (!group) return
  const samples = measurements.get(group) ?? { successes: [], errors: {} }
  if (expected.includes(code)) samples.successes.push(duration)
  else samples.errors[code] = (samples.errors[code] ?? 0) + 1
  measurements.set(group, samples)
}

export class Client {
  cookie = ''
  async request(group, path, { method = 'GET', data, expect = [200], raw = false } = {}) {
    const start = performance.now()
    let response, body, code = 'transport'
    try {
      response = await fetch(base + path, {
        method, redirect: 'manual', tls: { ca }, signal: AbortSignal.timeout(65_000),
        headers: {
          'X-Requested-With': 'tikkit', Origin: base,
          ...(this.cookie ? { Cookie: this.cookie } : {}),
          ...(data === undefined ? {} : { 'Content-Type': raw ? 'image/png' : 'application/json' }),
        },
        body: raw || data === undefined ? data : JSON.stringify(data),
      })
      const text = await response.text()
      code = response.status
      if (text && expect.includes(code) && response.headers.get('content-type')?.includes('application/json')) body = JSON.parse(text)
      const cookie = response.headers.get('set-cookie')
      if (cookie) this.cookie = cookie.split(';', 1)[0]
    } catch (error) {
      if (code !== 'transport' || !expect.includes('transport')) throw error
    } finally {
      record(group, performance.now() - start, code, expect)
    }
    // Omit paths' variable portions and bodies from failure logs.
    assert(expect.includes(code), `${group ?? method} returned ${code}, expected ${expect.join('/')}`)
    return { code, body }
  }
  async get(group, path) { return (await this.request(group, path)).body }
  async post(group, path, data, expect) { return this.request(group, path, { method: 'POST', data, expect }) }
  async login(username, password, group = 'sign_in') {
    await this.post(group, '/api/auth/login', { username, password })
    assert(this.cookie, 'signed-in account has no cookie')
    return this
  }
  async screenshot() {
    png ??= screenshot()
    return (await this.request('submit_upload', '/api/screenshots?filename=synthetic.png',
      { method: 'POST', data: png, raw: true, expect: [201] })).body.id
  }
}

export const reportRange = () => new URLSearchParams({
  created_from: new Date(Date.now() - 350 * 864e5).toISOString(),
  created_before: new Date(Date.now() + 864e5).toISOString(),
})
export const report = admin => admin.get('admin', '/api/admin/reports?' + reportRange())
export const ticket = id => '/api/tickets/' + id
export const message = body => ({ idempotency_key: randomUUID(), body, screenshot_ids: [] })
export function submission(category_id, priority_id, subject, screenshot_ids = []) {
  return {
    idempotency_key: randomUUID(), requester_skip: false,
    intake: { category_id, priority_id, subject, description: 'The synthetic workstation lost its settings.', answers: {}, screenshot_ids },
  }
}
export async function eventually(label, check, seconds = 60, interval = 500) {
  const expires = Date.now() + seconds * 1000
  do {
    const result = await check()
    if (result) return result
    await delay(interval)
  } while (Date.now() < expires)
  assert.fail(label + ' did not converge')
}

export async function propose(operator, contender, team, id) {
  const path = ticket(id)
  const current = () => operator.get('queue_staff_detail', path)
  const update = async data => ({ ...data, expected_updated_at: (await current()).updated_at })
  await operator.post('staff_writes', path + '/assignment', await update({ action: 'dispatch', team_id: team }))
  const claim = await update({ action: 'claim' })
  const claims = contender ? await Promise.all([operator, contender].map(
    actor => actor.post('staff_writes', path + '/assignment', claim, [200, 403, 409]),
  )) : [await operator.post('staff_writes', path + '/assignment', claim)]
  assert.equal(claims.filter(result => result.code === 200).length, 1, 'one claim won')
  const winner = claims[0].code === 200 ? operator : contender
  await winner.post('staff_writes', path + '/staff-actions', await update({ action: 'start_work' }))
  const reply = message('The replacement profile is ready.')
  const replies = await Promise.all([1, 2].map(() => winner.post('staff_writes', path + '/replies', reply, [200, 201])))
  assert.equal(replies[0].body.id, replies[1].body.id, 'one public reply receipt')
  await winner.post('staff_writes', path + '/notes', message('Workstation inventory checked.'), [201])
  await winner.post('staff_writes', path + '/staff-actions', await update({
    action: 'propose_resolution', summary: 'Replaced the profile.',
  }))
  return (await current()).proposal_version
}
