// @ts-nocheck -- a throwaway Node stub; typing the request plumbing would outweigh it.
// Stands in for the Rust API during browser tests: only what the pages call.
// Sign in as requester, operator, specialist, lead, or admin with PASSWORD.
import { randomUUID } from 'node:crypto'
import { createServer } from 'node:http'

const PASSWORD = 'correct horse battery'
const ROLES = { requester: 'user', operator: 'operator', specialist: 'specialist', lead: 'team_lead', admin: 'admin' }
// Language preference per session, so tests running in parallel do not share state.
const languages = {}
// Live session tokens; POST /api/test/expire drops the caller's, like an expired session.
const sessions = new Set()

const reply = (res, status, body, headers = {}) => {
  res.writeHead(status, { 'content-type': 'application/json', ...headers })
  res.end(body === undefined ? undefined : JSON.stringify(body))
}
const fail = (res, status, code) => reply(res, status, { error: { code, message: code }, request_id: 'stub' })
const readBody = (req) =>
  new Promise((resolve) => {
    let raw = ''
    req.on('data', (chunk) => (raw += chunk))
    req.on('end', () => resolve(raw ? JSON.parse(raw) : {}))
  })

const tokenOf = (req) => /(?:^|;\s*)tikkit_session=([^;]+)/.exec(req.headers.cookie ?? '')?.[1]
const sessionUser = (req) => {
  const token = tokenOf(req)
  return sessions.has(token) ? token.split('.')[0] : null
}

const settings = {
  name: 'Example support',
  timezone: 'Europe/Berlin',
  default_language: 'en',
  allow_reopening: false,
  requester_cancellation_policy: 'anytime_while_open',
  resolution_confirmation_hours: 168,
  allow_requester_ai_skip: false,
}

// Queue fixtures: 28 general-queue tickets (more than one page), three dispatched to the Desk team,
// and records the real API never lists (AI-solved, still in AI): they must never appear.
const OPEN = ['queued', 'in_progress', 'waiting_for_requester', 'awaiting_confirmation']
const CATEGORY = '10000000-0000-4000-8000-000000000001'
const PRIORITIES = [
  { id: '20000000-0000-4000-8000-000000000001', name: 'Urgent' },
  { id: '20000000-0000-4000-8000-000000000002', name: 'Normal' },
]
const ticket = (reference, subject, extra = {}) => ({
  id: `30000000-0000-4000-8000-${String(reference).padStart(12, '0')}`,
  reference,
  status: 'queued',
  subject,
  category_name: 'Hardware',
  priority_id: PRIORITIES[reference % 5 === 0 ? 0 : 1].id,
  priority_name: PRIORITIES[reference % 5 === 0 ? 0 : 1].name,
  requester: { id: '40000000-0000-4000-8000-000000000001', username: 'requester', display_name: 'requester' },
  assignment: { team_name: null, assignee_display_name: null },
  created_at: '2026-10-20T08:00:00Z',
  updated_at: '2026-10-20T08:00:00Z',
  ...extra,
})
const TICKETS = [
  ...Array.from({ length: 28 }, (_, i) => ticket(i + 1, `General ticket ${i + 1}`)),
  ticket(101, 'Desk unclaimed', { assignment: { team_name: 'Desk', assignee_display_name: null } }),
  ticket(102, 'Desk claimed', { status: 'in_progress', assignment: { team_name: 'Desk', assignee_display_name: 'operator' } }),
  ticket(103, 'Desk finished', { status: 'closed', assignment: { team_name: 'Desk', assignee_display_name: 'operator' } }),
  ticket(201, 'AI solved record', { status: 'ai_solved' }),
  ticket(202, 'Still with the AI', { status: 'ai_processing' }),
]

const account = (username, token) => ({
  id: `00000000-0000-4000-8000-${String(Object.keys(ROLES).indexOf(username)).padStart(12, '0')}`,
  username,
  display_name: username,
  email: `${username}@example.com`,
  language: languages[token] ?? 'en',
  role: ROLES[username],
})

createServer(async (req, res) => {
  const { pathname } = new URL(req.url, 'http://stub')
  const route = `${req.method} ${pathname}`
  if (route === 'GET /api/health') return reply(res, 200, { status: 'ok' })
  if (route === 'GET /api/setup') return reply(res, 200, { setup_required: false })
  if (route === 'POST /api/test/expire') return void (sessions.delete(tokenOf(req)), reply(res, 204))
  if (route === 'POST /api/auth/login') {
    const { username, password } = await readBody(req)
    if (!(username in ROLES) || password !== PASSWORD) return fail(res, 401, 'invalid_credentials')
    const token = `${username}.${randomUUID()}`
    sessions.add(token)
    return reply(res, 200, account(username, token), {
      'set-cookie': `tikkit_session=${token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=3600`,
    })
  }
  if (route === 'POST /api/auth/logout')
    return void (sessions.delete(tokenOf(req)), reply(res, 204, undefined, { 'set-cookie': 'tikkit_session=; Path=/; HttpOnly; Max-Age=0' }))

  const user = sessionUser(req)
  if (!user) return fail(res, 401, 'unauthorized')
  if (route === 'GET /api/auth/me') return reply(res, 200, account(user, tokenOf(req)))
  if (route === 'PATCH /api/auth/me') {
    const { language } = await readBody(req)
    if (language) languages[tokenOf(req)] = language
    return reply(res, 200, account(user, tokenOf(req)))
  }
  if (route === 'GET /api/organization') return reply(res, 200, settings)
  if (route === 'GET /api/settings/smtp') return ROLES[user] === 'admin' ? reply(res, 200, { connection: null }) : fail(res, 403, 'forbidden')
  if (pathname === '/api/accounts' || pathname === '/api/invitations' || pathname === '/api/teams' || pathname === '/api/admin/settings') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    if (pathname === '/api/admin/settings') return reply(res, 200, settings)
    const items =
      pathname === '/api/accounts'
        ? Object.keys(ROLES).map((u) => ({ ...account(u, null), is_active: true, expertise: null, team_id: null, is_team_lead: false, created_at: '2026-10-01T00:00:00Z' }))
        : []
    return reply(res, 200, { items, next_cursor: null })
  }
  if (pathname === '/api/catalog') {
    return reply(res, 200, {
      categories: [{ id: CATEGORY, name: 'Hardware', skip_ai: false, archived: false }],
      priorities: PRIORITIES.map(({ id, name }) => ({ id, name, archived: false })),
      fields: [],
    })
  }
  if (pathname === '/api/queue' || pathname === '/api/queue/counts') {
    if (ROLES[user] === 'user') return fail(res, 403, 'forbidden')
    const admin = ROLES[user] === 'admin'
    const dispatched = (t) => !!t.assignment.team_name
    const inView = (t, view) =>
      view === 'general' ? !dispatched(t) : view === 'team' ? dispatched(t) && (admin || user !== 'specialist') : view === 'all' ? admin : false
    const open = TICKETS.filter((t) => OPEN.includes(t.status))
    if (pathname === '/api/queue/counts')
      return reply(res, 200, {
        general: open.filter((t) => inView(t, 'general')).length,
        team: open.filter((t) => inView(t, 'team')).length,
        mine: 0,
        all: admin ? open.length : null,
      })
    const q = new URL(req.url, 'http://stub').searchParams
    const view = q.get('view') ?? 'general'
    if (view === 'all' && !admin) return fail(res, 403, 'forbidden')
    const status = q.get('status')
    let items = TICKETS.filter((t) => (view === 'all' ? true : inView(t, view)) && (status ? t.status === status : OPEN.includes(t.status)))
    if (q.get('unassigned') === 'true') items = items.filter((t) => !t.assignment.assignee_display_name)
    if (q.get('priority_id')) items = items.filter((t) => t.priority_id === q.get('priority_id'))
    const sort = q.get('sort') ?? 'newest'
    items = [...items].sort((a, b) =>
      sort === 'priority'
        ? PRIORITIES.findIndex((p) => p.id === a.priority_id) - PRIORITIES.findIndex((p) => p.id === b.priority_id) || a.reference - b.reference
        : sort === 'oldest' ? a.reference - b.reference : b.reference - a.reference)
    // The cursor is just the offset here; the real API's is opaque to the client too.
    const from = Number(q.get('cursor') ?? 0)
    const limit = Number(q.get('limit') ?? 25)
    const page = items.slice(from, from + limit)
    return reply(res, 200, { items: page, next_cursor: from + limit < items.length ? String(from + limit) : null })
  }
  fail(res, 404, 'not_found')
}).listen(Number(process.env.PORT), '127.0.0.1')
