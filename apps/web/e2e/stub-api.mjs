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

// --- Ticket form: catalog, drafts, submission, screenshots. State is per session, so tests that
// run in parallel as the same account never see each other's drafts or tickets.
const ID = (n) => `00000000-0000-4000-9000-${String(n).padStart(12, '0')}`
const SOFTWARE = ID(1)
const NETWORK = ID(2)
const PROGRAM = ID(10)
const LOCATION = ID(11)
const CATALOG = {
  // Hardware, Urgent, and Normal are the queue fixtures' catalog; the rest serve the form.
  categories: [
    { id: CATEGORY, name: 'Hardware', skip_ai: false, archived: false },
    { id: SOFTWARE, name: 'Software', skip_ai: false, archived: false },
    { id: NETWORK, name: 'Network', skip_ai: true, archived: false },
  ],
  priorities: [
    { id: PRIORITIES[0].id, name: 'Urgent', archived: false },
    { id: ID(22), name: 'High', archived: false },
    { id: PRIORITIES[1].id, name: 'Normal', archived: false },
    { id: ID(20), name: 'Low', archived: false },
  ],
  fields: [
    {
      id: PROGRAM, label: 'Program', guidance: 'Which program is affected?', kind: 'select', required: true, allow_other: true,
      category_ids: [SOFTWARE], archived: false,
      choices: [{ id: ID(30), label: 'Word', archived: false }, { id: ID(31), label: 'Excel', archived: false }],
    },
    {
      id: LOCATION, label: 'Office location', guidance: '', kind: 'text', required: false, allow_other: false,
      category_ids: [NETWORK], choices: [], archived: false,
    },
  ],
}
const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64')
const sessionState = {}
const stateOf = (req) => (sessionState[tokenOf(req)] ??= { drafts: new Map(), tickets: [], receipts: new Map(), shots: new Map(), reference: 1000 })
const validation = (res, fields) => reply(res, 422, { error: { code: 'validation_failed', message: 'validation_failed', details: { fields } }, request_id: 'stub' })

// Mirrors the server rules the form relies on (docs/submission.md); hidden answers are dropped.
function checkIntake(intake) {
  const fields = {}
  if (!intake.subject?.trim()) fields.subject = ['required']
  if (!intake.description?.trim()) fields.description = ['required']
  const category = CATALOG.categories.find((c) => c.id === intake.category_id)
  if (!intake.category_id) fields.category_id = ['required']
  else if (!category) fields.category_id = ['invalid']
  if (!intake.priority_id) fields.priority_id = ['required']
  else if (!CATALOG.priorities.some((p) => p.id === intake.priority_id)) fields.priority_id = ['invalid']
  const answers = {}
  for (const field of CATALOG.fields.filter((x) => category && x.category_ids.includes(category.id))) {
    const a = intake.answers?.[field.id]
    if (a?.choice_id && !field.choices.some((c) => c.id === a.choice_id)) fields[`answers.${field.id}`] = ['invalid']
    else if (a?.choice_id || a?.text?.trim()) answers[field.id] = a
    else if (field.required) fields[`answers.${field.id}`] = ['required']
  }
  return { fields, category, answers }
}

function submit(res, st, intake, key, sameRequest, draftId) {
  const prior = st.receipts.get(key)
  if (prior) return prior.same === sameRequest ? reply(res, 200, prior.receipt) : fail(res, 409, 'conflict')
  const { fields, answers } = checkIntake(intake)
  if (Object.keys(fields).length) return validation(res, fields)
  const ticket = { id: randomUUID(), reference: ++st.reference, status: 'queued' }
  st.tickets.push({ ...ticket, intake: { ...intake, answers } })
  st.receipts.set(key, { same: sameRequest, receipt: ticket })
  st.drafts.delete(draftId)
  // Slow enough that a second click or Enter lands while the first request is still running.
  setTimeout(() => reply(res, 201, ticket), 400)
}

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

  // --- Ticket form
  const st = stateOf(req)
  const draftId = /^\/api\/ticket-drafts\/([^/]+)(\/submit)?$/.exec(pathname)
  const shotId = /^\/api\/screenshots\/([^/]+)(\/preview)?$/.exec(pathname)
  if (route === 'GET /api/catalog') return reply(res, 200, CATALOG)
  if (route === 'GET /api/test/tickets') return reply(res, 200, st.tickets)
  if (route === 'POST /api/test/draft') {
    const draft = { id: randomUUID(), version: 1, intake: (await readBody(req)).intake, created_at: new Date().toISOString(), updated_at: new Date().toISOString() }
    st.drafts.set(draft.id, draft)
    return reply(res, 201, draft)
  }
  if (route === 'POST /api/test/bump-drafts') {
    // Another tab saved every draft: versions move on and the text changes.
    for (const d of st.drafts.values()) (d.version++, (d.intake = { ...d.intake, subject: `${d.intake.subject} (other tab)` }))
    return reply(res, 204)
  }
  if (route === 'POST /api/test/remove-drafts') return void (st.drafts.clear(), reply(res, 204))
  if (route === 'GET /api/ticket-drafts')
    return reply(res, 200, { items: [...st.drafts.values()].reverse(), next_cursor: null })
  if (route === 'POST /api/ticket-drafts') {
    const now = new Date().toISOString()
    const draft = { id: randomUUID(), version: 1, intake: await readBody(req), created_at: now, updated_at: now }
    st.drafts.set(draft.id, draft)
    return reply(res, 201, draft)
  }
  if (draftId) {
    const draft = st.drafts.get(draftId[1])
    if (!draft) return fail(res, 404, 'not_found')
    if (req.method === 'GET') return reply(res, 200, draft)
    if (req.method === 'DELETE') {
      if (Number(new URL(req.url, 'http://stub').searchParams.get('version')) !== draft.version) return fail(res, 409, 'conflict')
      return void (st.drafts.delete(draft.id), reply(res, 204))
    }
    const body = await readBody(req)
    if (body.version !== draft.version) return fail(res, 409, 'conflict')
    if (req.method === 'PATCH') {
      Object.assign(draft, { version: draft.version + 1, intake: body.intake, updated_at: new Date().toISOString() })
      return reply(res, 200, draft)
    }
    if (draftId[2]) {
      return submit(res, st, draft.intake, body.idempotency_key, `${draft.id}:${body.version}`, draft.id)
    }
  }
  if (route === 'POST /api/tickets') {
    const body = await readBody(req)
    return submit(res, st, body.intake, body.idempotency_key, JSON.stringify(body.intake))
  }
  if (route === 'POST /api/screenshots') {
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    const filename = new URL(req.url, 'http://stub').searchParams.get('filename')
    if (filename.startsWith('corrupt')) return validation(res, { file: ['corrupt'] })
    const shot = { id: randomUUID(), filename, content_type: req.headers['content-type'], byte_size: Buffer.concat(chunks).length, width: 1, height: 1, created_at: new Date().toISOString() }
    st.shots.set(shot.id, shot)
    return reply(res, 201, shot)
  }
  if (shotId) {
    const shot = st.shots.get(shotId[1])
    if (!shot) return fail(res, 404, 'not_found')
    if (shotId[2]) return (res.writeHead(200, { 'content-type': 'image/png' }), res.end(PNG))
    return reply(res, 200, shot)
  }
  fail(res, 404, 'not_found')
}).listen(Number(process.env.PORT), '127.0.0.1')
