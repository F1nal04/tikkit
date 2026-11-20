// @ts-nocheck -- a throwaway Node stub; typing the request plumbing would outweigh it.
// Stands in for the Rust API during browser tests: only what the pages call.
// Sign in as requester, operator, specialist, lead, or admin with PASSWORD.
import { randomUUID } from 'node:crypto'
import { createServer } from 'node:http'

const PASSWORD = 'correct horse battery'
const ROLES = { requester: 'user', requester2: 'user', operator: 'operator', specialist: 'specialist', lead: 'team_lead', admin: 'admin' }
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

// Staff detail: the queue fixtures plus tickets reachable only by link. Desk members are the operator and
// the lead; Admins see everything. State is per session, like the form's.
const DESK = ['operator', 'lead']
const TEAMS = [{ id: '50000000-0000-4000-8000-000000000001', name: 'Desk' }, { id: '50000000-0000-4000-8000-000000000002', name: 'Field' }]
const LINK_ONLY = [
  ticket(104, 'Desk claimed by lead', { status: 'in_progress', assignment: { team_name: 'Desk', assignee_display_name: 'lead' } }),
  ticket(105, 'Desk human closed', { status: 'closed', assignment: { team_name: 'Desk', assignee_display_name: 'operator' }, resolution_source: 'operator', closure_reason: 'requester_confirmed' }),
]
const STAFF_TICKETS = [...TICKETS, ...LINK_ONLY]
// Only Desk has members here; the Field team has none, so a dispatch there drops the dispatcher.
const isMember = (user, t) => ROLES[user] === 'admin' || (t.assignment.team_name === 'Desk' && DESK.includes(user))
const staffCanSee = (user, t) =>
  ['ai_processing', 'awaiting_ai_decision', 'ai_solved'].includes(t.status) ? ROLES[user] === 'admin' : !t.assignment.team_name || isMember(user, t)
// Writers: open, in a team, and unclaimed or the actor is the assignee, the lead, or an Admin.
const leads = (user, t) => ROLES[user] === 'admin' || (user === 'lead' && t.assignment.team_name === 'Desk')
const inCharge = (user, t) =>
  !!t.assignment.team_name && isMember(user, t) && (!t.assignment.assignee_display_name || t.assignment.assignee_display_name === user || leads(user, t))
const canHandle = (user, t) => OPEN.includes(t.status) && inCharge(user, t)
const access = (user, t) => {
  const open = OPEN.includes(t.status), team = t.assignment.team_name
  const canAssign = open && !!team && leads(user, t)
  const canDispatch = open && (!team || inCharge(user, t))
  return {
    can_dispatch: canDispatch,
    can_claim: open && !!team && !t.assignment.assignee_display_name && isMember(user, t) && ROLES[user] !== 'admin',
    can_assign: canAssign,
    dispatch_teams: canDispatch ? TEAMS.filter((x) => x.name !== team) : [],
    assignees: canAssign && team === 'Desk' ? DESK.filter((u) => u !== t.assignment.assignee_display_name).map((u) => ({ id: u, name: u })) : [],
  }
}

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
// `ai` is the Admin's AI configuration for this session (POST /api/test/ai): off by default, like the
// product. `providerCalls` counts attempts that would reach a provider; `aiActions` logs decision requests.
const stateOf = (req) => (sessionState[tokenOf(req)] ??= { drafts: new Map(), tickets: [], receipts: new Map(), shots: new Map(), submitted: [], reference: 1000, ai: { ai_assistance: false, requester_skip_allowed: false }, providerCalls: 0, aiActions: [] })
const aiPolicy = (st) => ({ ai_assistance: st.ai.ai_assistance, requester_skip_allowed: st.ai.ai_assistance && st.ai.requester_skip_allowed })
// Model output is untrusted text: the page must show markup, markdown, and links literally.
const SUGGESTION = {
  explanation: 'Word keeps a damaged profile. <img src=x onerror="window.aiXss = true"> **Reset** it: [click here](javascript:window.aiXss=true) or https://evil.example/reset\nYour screenshot seems to show error 0x80070005.',
  steps: ['Close Word. <script>window.aiXss = true</script>', 'Rename the folder "Normal.dotm".', 'Start Word again.'],
}
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

// Routing as in docs/submission.md: AI off, then category exemption, then the requester's skip
// (403 unless the Admin allows it), otherwise the one AI attempt.
function submit(res, st, intake, key, sameRequest, draftId, skip, language) {
  // What the client actually sent, before hidden answers are dropped the way the server does.
  st.submitted.push(JSON.parse(JSON.stringify(intake ?? {})))
  const prior = st.receipts.get(key)
  if (prior) return prior.same === sameRequest ? reply(res, 200, prior.receipt) : fail(res, 409, 'conflict')
  const { fields, answers, category } = checkIntake(intake)
  if (Object.keys(fields).length) return validation(res, fields)
  const policy = aiPolicy(st), now = new Date().toISOString()
  let status = 'queued', route = 'ai_disabled', ai = null
  if (policy.ai_assistance) {
    if (category.skip_ai) route = 'category_exempt'
    else if (skip) {
      if (!policy.requester_skip_allowed) return fail(res, 403, 'forbidden')
      route = 'requester_skip'
    } else {
      status = 'ai_processing'; route = 'ai'; st.providerCalls++
      ai = { state: 'processing', language, suggestion: null, reason: null, created_at: now, finished_at: null, decision: null, decided_at: null }
    }
  }
  const ticket = { id: randomUUID(), reference: ++st.reference, status }
  st.tickets.push({ ...ticket, intake: { ...intake, answers }, ai, created_at: now, updated_at: now, transitions: [{ id: 1, event_type: 'submit', from_status: 'draft', to_status: status, actor_account_id: null, payload: { route }, created_at: now }] })
  st.receipts.set(key, { same: sameRequest, receipt: ticket })
  st.drafts.delete(draftId)
  // Slow enough that a second click or Enter lands while the first request is still running.
  setTimeout(() => reply(res, 201, ticket), 400)
}


function detailOf(t) {
  const c = CATALOG.categories.find((c) => c.id === t.intake?.category_id) ?? CATALOG.categories[0]
  const p = CATALOG.priorities.find((p) => p.id === t.intake?.priority_id) ?? CATALOG.priorities[0]
  return {
    subject: t.subject ?? t.intake?.subject ?? 'Printer', description: t.description ?? t.intake?.description ?? 'Jammed',
    view: 'requester', category_id: c.id, category_name: c.name, priority_id: p.id, priority_name: p.name,
    assignment: { team_name: null, assignee_display_name: null },
    intake_snapshot: { subject: t.subject ?? t.intake?.subject ?? 'Printer', description: t.description ?? t.intake?.description ?? 'Jammed', catalog: { category_id: c.id, category_name: c.name, skip_ai: c.skip_ai, priority_id: p.id, priority_name: p.name, answers: [] }, screenshot_ids: t.intake?.screenshot_ids ?? [] },
    resolution_source: null, closure_reason: null, requester: null, ai: null,
    proposal_version: 0, confirmation_deadline: null, transitions: [],
    created_at: '2026-10-01T10:00:00Z', updated_at: '2026-10-01T10:00:00Z', ...t,
  }
}
function changeTicket(t, event, to, payload = {}) {
  const now = new Date().toISOString()
  t.transitions ??= []
  t.transitions.push({ id: t.transitions.length + 1, event_type: event, from_status: t.status, to_status: to, actor_account_id: null, payload, created_at: now })
  t.status = to; t.updated_at = now; t.confirmation_deadline = null
}
function seedHistory(st, user) {
  st.tickets = []
  const add = (name, status, extra = {}) => {
    const t = detailOf({ id: randomUUID(), reference: ++st.reference, status, subject: `${user}: ${name}`, description: 'Original problem <script>window.intakeXss = true</script>', ...extra })
    st.tickets.push(t); return t
  }
  for (let i = 0; i < 27; i++) add(`Older ticket ${i}`, 'queued')
  const queued = add('Waiting in general queue', 'queued')
  const screenshot = { id: randomUUID(), filename: 'original.png', content_type: 'image/png', byte_size: PNG.length, width: 1, height: 1, created_at: queued.created_at }
  st.shots.set(screenshot.id, screenshot); queued.intake_snapshot.screenshot_ids = [screenshot.id]
  queued.intake_snapshot.description = queued.description
  const pending = add('Resolution pending', 'in_progress', { assignment: { team_name: 'Desk', assignee_display_name: 'Olga Support' } })
  changeTicket(pending, 'start_work', 'in_progress')
  changeTicket(pending, 'propose_resolution', 'awaiting_confirmation', { summary: 'Installed a new driver. <img src=x onerror=alert(1)>', proposal_version: 2 })
  pending.proposal_version = 2; pending.confirmation_deadline = '2026-12-01T10:00:00Z'
  const closed = add('Human closed', 'closed', { resolution_source: 'operator', closure_reason: 'requester_confirmed', assignment: pending.assignment })
  const timeout = add('Automatically closed', 'awaiting_confirmation', { resolution_source: 'operator', closure_reason: 'confirmation_timeout', assignment: pending.assignment })
  changeTicket(timeout, 'confirmation_timeout', 'closed')
  const ai = add('AI solved final', 'awaiting_ai_decision', { resolution_source: 'ai', closure_reason: 'requester_confirmed' })
  changeTicket(ai, 'ai_solved', 'ai_solved')
  ai.ai = { state: 'suggestion_ready', language: 'en', suggestion: SUGGESTION, reason: null, created_at: ai.created_at, finished_at: ai.created_at, decision: 'solved', decided_at: '2026-10-01T10:05:00Z' }
  const privateShot = randomUUID()
  st.shots.set(privateShot, { ...screenshot, id: privateShot, private: true })
  const publicMessage = (i) => ({ id: randomUUID(), kind: 'public_reply', author_kind: 'staff', author_account_id: null, author_display_name: 'Olga Support', created_at: pending.created_at, body: `Public update ${i}: <script>window.messageXss = true</script>`, screenshot_ids: i === 0 ? [screenshot.id] : [] })
  st.messages.set(queued.id, [ ...Array.from({ length: 27 }, (_, i) => publicMessage(i)), { ...publicMessage(99), kind: 'internal_note', body: 'PRIVATE NOTE MUST NEVER LEAK', screenshot_ids: [privateShot] } ])
  return { queued: queued.id, pending: pending.id, closed: closed.id, timeout: timeout.id, ai: ai.id, screenshot: screenshot.id, privateShot }
}


// Admin audit log (docs/audit.md): newest first, Admin only. Metadata is untrusted text, and one
// resource ID is a spreadsheet formula. The export is CSV and records itself, as the API does.
const auditActor = (username, role, team = null) => ({ account_id: `40000000-0000-4000-8000-00000000000${{ admin: 6, requester: 1, operator: 3 }[username]}`, username, role, team_id: team && '50000000-0000-4000-8000-000000000001', team_name: team, service: null })
const auditService = (service) => ({ account_id: null, username: null, role: null, team_id: null, team_name: null, service })
const AUDIT_TICKET = '60000000-0000-4000-8000-000000000301'
const auditTicket = { resource_type: 'ticket', resource_id: AUDIT_TICKET, ticket_id: AUDIT_TICKET, ticket_reference: 301 }
const auditEvent = (id, minute, action, actor, extra = {}) => ({
  id, created_at: `2026-11-02T09:${String(minute).padStart(2, '0')}:00Z`, action, actor, subject: null,
  resource_type: null, resource_id: null, ticket_id: null, ticket_reference: null, request_id: `70000000-0000-4000-8000-${String(id).padStart(12, '0')}`, metadata: {}, ...extra,
})
const AUDIT = [
  auditEvent(1, 1, 'login_failed', auditService('anonymous'), { subject: { account_id: '40000000-0000-4000-8000-000000000003', username: 'operator' }, resource_type: 'account', resource_id: '40000000-0000-4000-8000-000000000003', metadata: { reason: 'wrong_password' } }),
  auditEvent(2, 2, 'ticket_submit', auditActor('requester', 'user'), { ...auditTicket, metadata: { route: 'ai', to_status: 'ai_processing' } }),
  auditEvent(3, 3, 'ticket_ai_suggestion_ready', auditService('ai_attempts'), { ...auditTicket, request_id: null }),
  auditEvent(4, 4, 'ticket_ai_solved', auditActor('requester', 'user'), { ...auditTicket, metadata: { resolution_source: 'ai', closure_reason: 'requester_confirmed', note: '<img src=x onerror="window.auditXss = true">' } }),
  auditEvent(5, 5, 'ai_solved_change_denied', auditActor('operator', 'operator', 'Desk'), { ...auditTicket, metadata: { attempted: 'reopen', status: 'ai_solved', error: 'not_found' } }),
  auditEvent(6, 6, 'organization_settings_updated', auditActor('admin', 'admin'), { resource_type: 'organization', resource_id: '=HYPERLINK("http://evil.example")', metadata: { from: { allow_reopening: false }, to: { allow_reopening: true } } }),
]
const auditMatches = (q) => (e) =>
  (!q.get('action') || e.action === q.get('action')) &&
  (!q.get('actor_username') || e.actor.username === q.get('actor_username').toLowerCase()) &&
  (!q.get('ticket_reference') || String(e.ticket_reference) === q.get('ticket_reference')) &&
  (!q.get('resource_type') || e.resource_type === q.get('resource_type')) &&
  (!q.get('resource_id') || e.resource_id === q.get('resource_id')) &&
  (!q.get('request_id') || e.request_id === q.get('request_id')) &&
  (!q.get('created_from') || e.created_at >= q.get('created_from')) &&
  (!q.get('created_before') || e.created_at < q.get('created_before'))
const auditExports = new Map()
const csvCell = (value) => `"${/^[=+\-@\t\r]/.test(value) ? "'" : ''}${String(value).replaceAll('"', '""')}"`


// Admin submission view (docs/admin-submissions.md): mixed outcomes, Admin only. The suggestion is
// untrusted text, and the AI settings carry a credential hint that must never reach these pages.
const submission = (reference, subject, extra = {}) => ({
  id: `60000000-0000-4000-8000-${String(reference).padStart(12, '0')}`,
  reference, subject, status: 'queued', category_id: CATEGORY, category_name: 'Hardware', priority_name: 'Normal',
  requester: { id: '40000000-0000-4000-8000-000000000001', username: 'requester', display_name: 'requester' },
  team_name: null, assignee_display_name: null, resolution_source: null, closure_reason: null, resolution: 'open',
  route: 'ai', ai_outcome: null, created_at: `2026-10-${String(30 - (reference % 30)).padStart(2, '0')}T08:00:00Z`, updated_at: '2026-10-30T08:00:00Z',
  ...extra,
})
const SUBMISSIONS = [
  submission(301, 'Solved by the AI suggestion', { status: 'ai_solved', resolution: 'ai_confirmed', resolution_source: 'ai', closure_reason: 'requester_confirmed', ai_outcome: 'solved' }),
  submission(302, 'Suggestion waiting for a decision', { status: 'awaiting_ai_decision', ai_outcome: 'awaiting_decision' }),
  submission(303, 'Suggestion abandoned', { status: 'cancelled', resolution: 'cancelled', ai_outcome: 'cancelled' }),
  submission(304, 'AI timed out', { ai_outcome: 'failed' }),
  submission(305, 'Escalated after the suggestion', { status: 'in_progress', ai_outcome: 'not_solved', team_name: 'Desk', assignee_display_name: 'operator' }),
  submission(306, 'Resolved by the help desk', { status: 'closed', resolution: 'requester_confirmed', resolution_source: 'operator', closure_reason: 'requester_confirmed', route: 'ai_disabled', team_name: 'Desk', assignee_display_name: 'operator' }),
]
const submittedScope = (x, q) =>
  (!q.get('created_from') || Date.parse(x.created_at) >= Date.parse(q.get('created_from'))) &&
  (!q.get('created_before') || Date.parse(x.created_at) < Date.parse(q.get('created_before'))) &&
  (!q.get('team_id') || TEAMS.find((t) => t.name === x.team_name)?.id === q.get('team_id')) &&
  (!q.get('category_id') || x.category_id === q.get('category_id'))
const disposition = (x) => x.ai_outcome === 'failed' ? 'provider_failed' : (x.ai_outcome ?? x.route)
const enteredQueue = (x) => ['failed', 'not_solved', null].includes(x.ai_outcome)
function reportFixture(items, teamFiltered) {
  const counts = (group, key) => Object.entries(Object.groupBy(items, group))
    .map(([value, rows]) => ({ [key]: value, count: rows.length }))
  const queued = items.filter(enteredQueue)
  const closed = items.filter((x) => x.status === 'closed').length
  const assigned = queued.filter((x) => x.team_name).length
  const solved = items.filter((x) => x.ai_outcome === 'solved').length
  return {
    submissions: items.length,
    daily: counts((x) => x.created_at.slice(0, 10), 'day'),
    categories: counts((x) => x.category_name, 'name').map((row) => ({ id: CATEGORY, ...row })),
    priorities: counts((x) => x.priority_name, 'name').map((row) => ({ id: PRIORITIES[1].id, ...row })),
    ai_outcomes: counts(disposition, 'outcome'),
    ai_attempts: items.filter((x) => x.ai_outcome).length,
    ai_rate_numerator: teamFiltered ? null : solved,
    ai_rate_denominator: teamFiltered ? null : items.filter((x) => ['solved', 'not_solved', 'failed'].includes(x.ai_outcome)).length,
    deflection_rate_numerator: teamFiltered ? null : solved,
    deflection_rate_denominator: teamFiltered ? null : items.filter((x) => x.status !== 'cancelled').length,
    closures: counts((x) => x.resolution, 'resolution'),
    awaiting_confirmation: 0,
    operator: {
      queued: queued.length, first_response_samples: closed, mean_first_response_seconds: closed ? 1800 : null,
      initial_closures: closed, initial_requester_confirmed: closed, initial_confirmation_timeout: 0, subsequent_closures: 0, reopens: 0, rejected_proposals: 0,
      time_to_proposal_samples: closed, mean_queue_to_proposal_seconds: closed ? 3600 : null,
      confirmation_wait_samples: closed, mean_proposal_to_closure_seconds: closed ? 5400 : null,
      resolution_samples: closed, mean_queue_to_closure_seconds: closed ? 9000 : null,
    },
    workload: assigned ? [{ team_id: TEAMS[0].id, team_name: TEAMS[0].name, assignee_id: account('operator', null).id,
      assignee_name: 'operator', assignee_role: 'operator', queued: assigned, active: assigned - closed, awaiting_confirmation: 0 }] : [],
  }
}
const requesterActor = { id: '40000000-0000-4000-8000-000000000001', username: 'requester', display_name: 'requester' }
function submissionDetail(s) {
  const ev = (id, event_type, from_status, to_status, actor = null, payload = {}) => ({ id, event_type, from_status, to_status, actor, payload, created_at: s.created_at })
  const events = [ev(1, 'submit', 'draft', s.route === 'ai' ? 'ai_processing' : 'queued', requesterActor, { route: s.route })]
  const suggested = ['solved', 'awaiting_decision', 'cancelled', 'not_solved'].includes(s.ai_outcome)
  if (suggested) events.push(ev(2, 'ai_suggestion_ready', 'ai_processing', 'awaiting_ai_decision'))
  if (s.ai_outcome === 'solved') events.push(ev(3, 'ai_solved', 'awaiting_ai_decision', 'ai_solved', requesterActor, { resolution_source: 'ai', closure_reason: 'requester_confirmed' }))
  if (s.ai_outcome === 'not_solved') events.push(ev(3, 'ai_not_solved', 'awaiting_ai_decision', 'queued', requesterActor, { route: 'not_solved' }))
  if (s.ai_outcome === 'cancelled') events.push(ev(3, 'cancel', 'awaiting_ai_decision', 'cancelled', requesterActor, { reason: 'Found it myself' }))
  if (s.ai_outcome === 'failed') events.push(ev(2, 'ai_failed', 'ai_processing', 'queued', null, { route: 'ai_failed' }))
  if (s.resolution === 'requester_confirmed') events.push(ev(5, 'propose_resolution', 'in_progress', 'awaiting_confirmation', { id: 'op', username: 'operator', display_name: 'operator' }, { summary: 'Replaced the toner.' }), ev(6, 'accept_resolution', 'awaiting_confirmation', 'closed', requesterActor))
  const decision = s.ai_outcome === 'solved' ? 'solved' : s.ai_outcome === 'not_solved' ? 'not_solved' : null
  const ai = s.ai_outcome && {
    attempt_id: randomUUID(), outcome: s.ai_outcome, state: suggested ? 'suggestion_ready' : 'handed_off', outcome_code: s.ai_outcome === 'failed' ? 'timeout' : null,
    provider: 'openai', model: 'openai-vision', language: 'en', suggestion: suggested ? SUGGESTION : null,
    decision, decision_event_id: decision ? 3 : null, decided_at: decision ? s.created_at : null, created_at: s.created_at, finished_at: s.created_at,
  }
  return {
    summary: s, description: 'Printer jams on page two.', confirmation_deadline: null, ai: ai || null, events,
    intake_snapshot: { subject: s.subject, description: 'Printer jams on page two.', catalog: { category_id: CATEGORY, category_name: 'Hardware', skip_ai: false, priority_id: PRIORITIES[1].id, priority_name: 'Normal', answers: [] }, screenshot_ids: [] },
  }
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
  if (route === 'GET /api/organization') return reply(res, 200, { ...settings, ...stateOf(req).policy })
  if (route === 'GET /api/ai/policy') return reply(res, 200, aiPolicy(stateOf(req)))
  if (route === 'GET /api/settings/ai') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    const credential = { hint: 'WXYZ', set_at: '2026-01-01T00:00:00Z', set_by: null, set_by_username: 'admin', readable: true }
    return reply(res, 200, {
      enabled: false,
      provider: null,
      model: null,
      status: 'disabled',
      allow_requester_skip: false,
      providers: [
        { provider: 'openai', models: [{ id: 'openai-vision', image_input: true }], credential },
        { provider: 'gemini', models: [{ id: 'gemini-vision', image_input: true }, { id: 'gemini-text', image_input: false }], credential: null },
        { provider: 'anthropic', models: [{ id: 'anthropic-vision', image_input: true }], credential: null },
      ],
      connection: { status: 'untested', error: null, tested_at: null },
      updated_at: '2026-01-01T00:00:00Z',
    })
  }
  if (route === 'GET /api/settings/ai/limits' || route === 'GET /api/settings/ai/usage') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    if (route.endsWith('limits'))
      return reply(res, 200, {
        window_hours: 24,
        org_request_limit: 500,
        user_request_limit: 10,
        org_token_limit: null,
        user_token_limit: null,
        max_concurrency: 2,
        concurrency_wait_seconds: 30,
        breaker_failure_threshold: 5,
        breaker_cooldown_seconds: 60,
        retention_days: 90,
        updated_at: '2026-01-01T00:00:00Z',
      })
    const none = { requests: 0, succeeded: 0, failed: 0, refused: 0, images: 0, input_tokens: null, output_tokens: null, total_tokens: null, usage_unreported: 0 }
    const closed = { state: 'closed', consecutive_failures: 0, open_until: null }
    return reply(res, 200, {
      days: 30,
      window: { window_hours: 24, requests: 3, request_limit: 500, budget_tokens: 495, estimated_tokens: 0, token_limit: null, in_flight: 0, max_concurrency: 2 },
      totals: { ...none, requests: 3, succeeded: 2, failed: 1, refused: 1, images: 2, input_tokens: 240, output_tokens: 90 },
      providers: [
        { provider: 'openai', usage: none, circuit: closed },
        { provider: 'gemini', usage: none, circuit: closed },
        { provider: 'anthropic', usage: { ...none, requests: 3, succeeded: 2, failed: 1, refused: 1, images: 2, input_tokens: 240, output_tokens: 90 }, circuit: closed },
      ],
      outcomes: [
        { status: 'failed', outcome: 'rate_limited', count: 1 },
        { status: 'refused', outcome: 'budget_user_requests', count: 1 },
      ],
    })
  }
  if (route === 'GET /api/catalog/all') return ROLES[user] === 'admin' ? reply(res, 200, CATALOG) : fail(res, 403, 'forbidden')
  if (pathname.startsWith('/api/admin/audit')) {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    const q = new URL(req.url, 'http://stub').searchParams
    // Per session, so parallel tests do not see each other's exports.
    const token = tokenOf(req)
    const log = [...AUDIT, ...(auditExports.get(token) ?? [])]
    if (route === 'GET /api/admin/audit/actions') return reply(res, 200, { actions: [...new Set(log.map((e) => e.action))].sort() })
    if (route === 'POST /api/admin/audit/export') {
      if (req.headers['x-requested-with'] !== 'tikkit') return fail(res, 403, 'csrf_rejected')
      const rows = log.filter(auditMatches(q)).reverse()
      const mine = auditExports.get(token) ?? []
      mine.push(auditEvent(100 + mine.length, 30 + mine.length, 'audit_exported', auditActor('admin', 'admin'), { resource_type: 'audit_log', metadata: { filters: Object.fromEntries(q), rows: rows.length, truncated: false } }))
      auditExports.set(token, mine)
      const lines = [['id', 'time_utc', 'action', 'actor_username', 'resource_id'], ...rows.map((e) => [String(e.id), e.created_at, e.action, e.actor.username ?? '', e.resource_id ?? ''])]
      res.writeHead(200, { 'content-type': 'text/csv; charset=utf-8', 'content-disposition': 'attachment; filename="tikkit-audit-2026-11-18.csv"', 'x-export-rows': String(rows.length), 'x-export-truncated': 'false' })
      return res.end(lines.map((cells) => `${cells.map(csvCell).join(',')}\r\n`).join(''))
    }
    if (route !== 'GET /api/admin/audit') return fail(res, 405, 'method_not_allowed')
    const matching = log.filter(auditMatches(q)).reverse()
    // Two pages of the unfiltered log, so paging is exercised.
    const limit = 4
    const start = q.get('cursor') ? Number(q.get('cursor')) : 0
    const next = start + limit < matching.length ? String(start + limit) : null
    return reply(res, 200, { items: matching.slice(start, start + limit), next_cursor: next })
  }
  if (route === 'GET /api/admin/reports' || route === 'POST /api/admin/reports/export') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    const q = new URL(req.url, 'http://stub').searchParams
    const duration = Date.parse(q.get('created_before')) - Date.parse(q.get('created_from'))
    if (!(duration > 0 && duration <= 366 * 86_400_000)) return fail(res, 400, 'invalid_query')
    const items = SUBMISSIONS.filter((x) => submittedScope(x, q))
    if (req.method === 'GET') return reply(res, 200, reportFixture(items, q.has('team_id')))
    if (req.headers['x-requested-with'] !== 'tikkit') return fail(res, 403, 'csrf_rejected')
    res.writeHead(200, { 'content-type': 'text/csv', 'content-disposition': 'attachment; filename="ticket-report.csv"' })
    return res.end(['reference,ai_outcome', ...items.map((x) => `${x.reference},${disposition(x)}`)].join('\r\n'))
  }
  if (pathname.startsWith('/api/admin/submissions')) {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    if (req.method !== 'GET') return fail(res, 405, 'method_not_allowed')
    const q = new URL(req.url, 'http://stub').searchParams
    const id = /^\/api\/admin\/submissions\/([^/]+)$/.exec(pathname)?.[1]
    if (id) {
      const found = SUBMISSIONS.find((x) => x.id === id)
      return found ? reply(res, 200, submissionDetail(found)) : fail(res, 404, 'not_found')
    }
    const items = SUBMISSIONS.filter((x) =>
      submittedScope(x, q) &&
      (!q.get('assignee_id') || (x.assignee_display_name && account(x.assignee_display_name, null).id === q.get('assignee_id'))) &&
      (!q.has('entered_queue') || enteredQueue(x) === (q.get('entered_queue') === 'true')) &&
      (!q.get('status') || x.status === q.get('status')) &&
      (!q.get('ai_disposition') || disposition(x) === q.get('ai_disposition')) &&
      (!q.get('resolution') || x.resolution === q.get('resolution')) &&
      (!q.get('resolution_source') || (x.resolution_source ?? 'none') === q.get('resolution_source')) &&
      (!q.get('ai_outcome') || (x.ai_outcome ?? 'none') === q.get('ai_outcome')))
    return reply(res, 200, { items, next_cursor: null })
  }
  if (route === 'GET /api/settings/smtp') return ROLES[user] === 'admin' ? reply(res, 200, { connection: null }) : fail(res, 403, 'forbidden')
  if (pathname === '/api/accounts' || pathname === '/api/invitations' || pathname === '/api/teams' || pathname === '/api/admin/settings') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    if (pathname === '/api/admin/settings') return reply(res, 200, settings)
    const items =
      pathname === '/api/accounts'
        ? Object.keys(ROLES).map((u) => ({ ...account(u, null), is_active: true, expertise: null, team_id: null, is_team_lead: false, created_at: '2026-10-01T00:00:00Z' }))
        : pathname === '/api/teams' ? TEAMS.map((team) => ({ ...team, archived: false })) : []
    return reply(res, 200, { items, next_cursor: null })
  }
  if (pathname === '/api/queue' || pathname === '/api/queue/counts') {
    if (ROLES[user] === 'user') return fail(res, 403, 'forbidden')
    const admin = ROLES[user] === 'admin'
    const dispatched = (t) => !!t.assignment.team_name
    const inView = (t, view) =>
      view === 'general' ? !dispatched(t) : view === 'team' ? dispatched(t) && isMember(user, t) : view === 'all' ? admin : false
    // Dispatches and claims made in this session move the queue fixtures.
    const live = TICKETS.map((t) => stateOf(req).staff?.get(t.id) ?? t)
    const open = live.filter((t) => OPEN.includes(t.status))
    if (pathname === '/api/queue/counts')
      return reply(res, 200, {
        general: open.filter((t) => inView(t, 'general')).length,
        team: open.filter((t) => inView(t, 'team')).length,
        mine: 0,
        all: admin ? open.length : null,
        // Opaque change marker over the caller's visible open work, like the real API.
        version: JSON.stringify(open.filter((t) => staffCanSee(user, t)).map((t) => [t.id, t.status, t.updated_at, t.assignment])),
      })
    const q = new URL(req.url, 'http://stub').searchParams
    const view = q.get('view') ?? 'general'
    if (view === 'all' && !admin) return fail(res, 403, 'forbidden')
    const status = q.get('status')
    let items = live.filter((t) => (view === 'all' ? true : inView(t, view)) && (status ? t.status === status : OPEN.includes(t.status)))
    if (q.get('unassigned') === 'true') items = items.filter((t) => !t.assignment.assignee_display_name)
    if (q.get('priority_id')) items = items.filter((t) => t.priority_id === q.get('priority_id'))
    // Search: a reference, or 3+ characters of the subject (the real API also searches more fields).
    const text = q.get('q')?.trim()
    if (text) {
      const reference = /^#?(\d+)$/.exec(text)?.[1]
      if (!reference && text.length < 3) return validation(res, { q: ['too_short'] })
      items = items.filter((t) => String(t.reference) === reference || (text.length >= 3 && t.subject.toLowerCase().includes(text.toLowerCase())))
    }
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

  // --- Saved views and bulk actions (docs/queues.md)
  const st = stateOf(req)
  if (pathname.startsWith('/api/saved-views') || pathname === '/api/queue/bulk-options' || route === 'POST /api/tickets/bulk') {
    if (ROLES[user] === 'user') return fail(res, 403, 'forbidden')
    st.views ??= []
    if (route === 'GET /api/saved-views') return reply(res, 200, st.views)
    if (route === 'POST /api/saved-views') {
      const { name, filters } = await readBody(req)
      if (!name?.trim()) return validation(res, { name: ['required'] })
      if (st.views.some((v) => v.name === name.trim())) return validation(res, { name: ['taken'] })
      const view = { id: randomUUID(), name: name.trim(), filters, stale: [], created_at: new Date().toISOString() }
      st.views.push(view)
      return reply(res, 201, view)
    }
    if (req.method === 'DELETE') {
      const before = st.views.length
      st.views = st.views.filter((v) => `/api/saved-views/${v.id}` !== pathname)
      return before === st.views.length ? fail(res, 404, 'not_found') : reply(res, 204)
    }
    if (pathname === '/api/queue/bulk-options') {
      const assignees = ['team_lead', 'admin'].includes(ROLES[user]) ? DESK.map((u) => ({ id: u, display_name: u, team_id: TEAMS[0].id, team_name: 'Desk' })) : []
      return reply(res, 200, { teams: TEAMS, assignees })
    }
    // Like the real API, each item runs the single-ticket endpoint on its own.
    const { change, tickets } = await readBody(req)
    const endpoint = ['dispatch', 'return_to_general', 'claim', 'assign', 'unassign'].includes(change.action) ? 'assignment' : 'staff-actions'
    const results = []
    for (const t of tickets) {
      const r = await fetch(`http://127.0.0.1:${process.env.PORT}/api/tickets/${t.id}/${endpoint}`, {
        method: 'POST',
        headers: { cookie: req.headers.cookie, 'content-type': 'application/json' },
        body: JSON.stringify({ ...change, expected_updated_at: t.expected_updated_at }),
      })
      results.push({ id: t.id, error: r.ok ? null : (await r.json()).error.code })
    }
    return reply(res, 200, { results })
  }

  // --- Ticket form
  const draftId = /^\/api\/ticket-drafts\/([^/]+)(\/submit)?$/.exec(pathname)
  const shotId = /^\/api\/screenshots\/([^/]+)(\/(?:preview|download))?$/.exec(pathname)
  st.policy ??= {}
  st.messages ??= new Map()
  if (route === 'POST /api/test/policy') { Object.assign(st.policy, await readBody(req)); return reply(res, 204) }
  if (route === 'POST /api/test/assignment') { const input = await readBody(req); const t = st.tickets.find((t) => t.id === input.id); if (!t) return fail(res, 404, 'not_found'); t.assignment = input.assignment; return reply(res, 204) }
  if (route === 'POST /api/test/history') return reply(res, 200, seedHistory(st, user))
  // --- AI step test hooks: the Admin's configuration, the worker's result, and what was requested.
  if (route === 'POST /api/test/ai') { Object.assign(st.ai, await readBody(req)); return reply(res, 204) }
  if (route === 'GET /api/test/ai-log') return reply(res, 200, { provider_calls: st.providerCalls, actions: st.aiActions })
  if (route === 'POST /api/test/ai-result') {
    const { id, outcome = 'suggestion', suggestion = SUGGESTION, force = false } = await readBody(req)
    const t = st.tickets.find((t) => t.id === id)
    if (!t) return fail(res, 404, 'not_found')
    // `force` is a result that reached the attempt after the handoff: the status does not move.
    if (force) { t.ai = { ...t.ai, suggestion }; t.updated_at = new Date().toISOString(); return reply(res, 200, { applied: false }) }
    // Like the worker, a result only counts while the attempt is processing; a late one is dropped.
    if (t.status !== 'ai_processing') return reply(res, 200, { applied: false })
    const finished_at = new Date().toISOString()
    if (outcome === 'suggestion') {
      changeTicket(t, 'ai_suggestion_ready', 'awaiting_ai_decision')
      t.ai = { ...t.ai, state: 'suggestion_ready', suggestion, finished_at }
    } else {
      changeTicket(t, 'ai_failed', 'queued', { route: outcome })
      t.ai = { ...t.ai, state: 'handed_off', reason: outcome, finished_at }
    }
    return reply(res, 200, { applied: true })
  }
  if (route === 'GET /api/tickets') {
    const q = new URL(req.url, 'http://stub').searchParams
    let items = st.tickets.map(detailOf).reverse()
    if (q.get('status')) items = items.filter((t) => t.status === q.get('status'))
    if (q.get('created_from')) items = items.filter((t) => new Date(t.created_at) >= new Date(q.get('created_from')))
    if (q.get('created_before')) items = items.filter((t) => new Date(t.created_at) < new Date(q.get('created_before')))
    const from = Number(q.get('cursor') ?? 0), limit = Math.min(100, Math.max(1, Number(q.get('limit') ?? 25)))
    return reply(res, 200, { items: items.slice(from, from + limit), next_cursor: from + limit < items.length ? String(from + limit) : null })
  }
  // --- Staff ticket detail and handling (docs/lifecycle.md "Staff HTTP actions")
  const staffRoute = /^\/api\/tickets\/([^/]+)(?:\/(messages|replies|notes|staff-actions|assignment|activity))?$/.exec(pathname)
  const staffSource = staffRoute && ROLES[user] !== 'user' && STAFF_TICKETS.find((t) => t.id === staffRoute[1])
  if (staffSource) {
    const store = (st.staff ??= new Map())
    if (!store.has(staffSource.id))
      store.set(staffSource.id, { ...structuredClone(staffSource), proposal_version: 0, confirmation_deadline: null, transitions: [], priority_changes: [], assignment_changes: [], messages: [
        { id: randomUUID(), kind: 'public_reply', author_kind: 'requester', author_account_id: null, author_display_name: 'requester', created_at: '2026-10-20T08:05:00Z', body: 'The printer jams on every page.', screenshot_ids: [] },
        { id: randomUUID(), kind: 'internal_note', author_kind: 'staff', author_account_id: null, author_display_name: 'lead', created_at: '2026-10-20T08:10:00Z', body: 'Same model failed last month.', screenshot_ids: [] },
      ] })
    const t = store.get(staffSource.id), endpoint = staffRoute[2]
    if (!staffCanSee(user, t)) return fail(res, 404, 'not_found')
    const touch = () => (t.updated_at = new Date().toISOString())
    const transition = (event, to, payload = {}) => {
      t.transitions.push({ id: t.transitions.length + 1, event_type: event, from_status: t.status, to_status: to, actor_account_id: account(user, null).id, actor_display_name: user, payload, created_at: new Date().toISOString() })
      t.status = to; touch()
    }
    if (!endpoint)
      return reply(res, 200, {
        ...t, messages: undefined, view: 'staff', category_id: CATEGORY, requester: t.requester,
        intake_snapshot: { subject: t.subject, description: 'Printer jams', catalog: { category_id: CATEGORY, category_name: 'Hardware', skip_ai: false, priority_id: t.priority_id, priority_name: t.priority_name, answers: [{ field_id: PROGRAM, label: 'Program', value: 'Word' }] }, screenshot_ids: [] },
        description: 'Printer jams', resolution_source: t.resolution_source ?? null, closure_reason: t.closure_reason ?? null,
        staff_access: { can_handle: canHandle(user, t), can_reopen: t.status === 'closed' && !!{ ...settings, ...st.policy }.allow_reopening && inCharge(user, t), ...access(user, t) },
      })
    if (endpoint === 'activity') return reply(res, 200, { version: `${t.updated_at}.${t.messages.length}` })
    if (endpoint === 'messages') {
      const q = new URL(req.url, 'http://stub').searchParams, start = Number(q.get('cursor') ?? 0), limit = Number(q.get('limit') ?? 25)
      return reply(res, 200, { items: t.messages.slice(start, start + limit), next_cursor: start + limit < t.messages.length ? String(start + limit) : null })
    }
    const input = await readBody(req)
    if (endpoint === 'replies' || endpoint === 'notes') {
      const prior = t.messages.find((m) => m.key === input.idempotency_key)
      if (prior) return reply(res, 201, prior)
      if (!canHandle(user, t)) return fail(res, 403, 'forbidden')
      if (!input.body?.trim()) return validation(res, { body: ['required'] })
      const message = { id: randomUUID(), kind: endpoint === 'notes' ? 'internal_note' : 'public_reply', author_kind: 'staff', author_account_id: account(user, null).id, author_display_name: user, created_at: new Date().toISOString(), body: input.body, screenshot_ids: input.screenshot_ids ?? [], key: input.idempotency_key }
      t.messages.push(message)
      if (endpoint === 'replies') touch()
      return reply(res, 201, message)
    }
    if (endpoint === 'assignment') {
      const a = input.action, can = access(user, t), before = { ...t.assignment }
      // 404 already returned. The requester is refused before a stale timestamp, then the policy.
      if (t.requester?.username === user) return fail(res, 403, 'forbidden')
      if (input.expected_updated_at !== t.updated_at) return fail(res, 409, 'conflict')
      if (a === 'dispatch' || a === 'return_to_general') {
        if (!can.can_dispatch) return fail(res, 403, 'forbidden')
        const team = a === 'dispatch' ? can.dispatch_teams.find((x) => x.id === input.team_id) : null
        if (a === 'dispatch' && !team) return validation(res, { team_id: ['invalid'] })
        if (a === 'return_to_general' && !before.team_name) return fail(res, 409, 'conflict')
        t.assignment = { team_name: team?.name ?? null, assignee_display_name: null }
      } else if (a === 'claim') {
        if (!can.can_claim) return fail(res, 403, 'forbidden')
        t.assignment = { ...t.assignment, assignee_display_name: user }
      } else if (a === 'assign' || a === 'unassign') {
        if (!can.can_assign) return fail(res, 403, 'forbidden')
        if (a === 'assign' && !can.assignees.some((m) => m.id === input.assignee_account_id)) return validation(res, { assignee_account_id: ['invalid'] })
        if (a === 'unassign' && !before.assignee_display_name) return fail(res, 409, 'conflict')
        t.assignment = { ...t.assignment, assignee_display_name: a === 'assign' ? input.assignee_account_id : null }
      } else return validation(res, { action: ['invalid'] })
      t.assignment_changes.push({ id: t.assignment_changes.length + 1, action: a, reason: null, previous_team_name: before.team_name, team_name: t.assignment.team_name, previous_assignee_display_name: before.assignee_display_name, assignee_display_name: t.assignment.assignee_display_name, actor_display_name: user, created_at: new Date().toISOString() })
      touch()
      return reply(res, 200, { reference: t.reference, ...t.assignment, visible: staffCanSee(user, t) })
    }
    // staff-actions. Same order as the API: requester `403`, then stale `409`, then the policy.
    const a = input.action
    if (t.requester?.username === user) return fail(res, 403, 'forbidden')
    if (input.expected_updated_at !== t.updated_at) return fail(res, 409, 'conflict')
    if (!['start_work', 'request_information', 'propose_resolution', 'reopen', 'set_priority'].includes(a)) return validation(res, { action: ['invalid'] })
    const needsOpen = a !== 'reopen'
    if (needsOpen ? !canHandle(user, t) : !(t.status === 'closed' && inCharge(user, t))) return fail(res, 403, 'forbidden')
    if (a === 'reopen' && !{ ...settings, ...st.policy }.allow_reopening) return fail(res, 403, 'forbidden')
    const from = { start_work: ['queued', 'waiting_for_requester'], request_information: ['in_progress'], propose_resolution: ['in_progress', 'waiting_for_requester', 'awaiting_confirmation'], reopen: ['closed'], set_priority: OPEN }[a]
    if (!from.includes(t.status)) return fail(res, 409, 'conflict')
    if (a === 'propose_resolution' && !input.summary?.trim()) return validation(res, { summary: ['required'] })
    if (a === 'set_priority' && (input.priority_id === t.priority_id || !CATALOG.priorities.some((p) => p.id === input.priority_id))) return validation(res, { priority_id: ['invalid'] })
    if (a === 'set_priority') {
      const next = CATALOG.priorities.find((p) => p.id === input.priority_id)
      t.priority_changes.push({ id: t.priority_changes.length + 1, previous_priority_name: t.priority_name, priority_name: next.name, actor_account_id: account(user, null).id, actor_display_name: user, created_at: new Date().toISOString() })
      Object.assign(t, { priority_id: next.id, priority_name: next.name }); touch()
    } else if (a === 'propose_resolution') {
      t.proposal_version++; t.confirmation_deadline = new Date(Date.now() + 168 * 3600_000).toISOString()
      transition('propose_resolution', 'awaiting_confirmation', { summary: input.summary.trim(), proposal_version: t.proposal_version })
    } else if (a === 'request_information') transition(a, 'waiting_for_requester')
    else { if (a === 'reopen') (t.closure_reason = null, t.resolution_source = null); transition(a, 'in_progress') }
    if (a !== 'propose_resolution') t.confirmation_deadline = null
    return reply(res, 200, { status: t.status })
  }
  // Test hooks: a concurrent change by someone else, a mid-session reassignment, or a transfer.
  if (route === 'POST /api/test/concurrent') {
    const { id, assignee, team } = await readBody(req), t = (st.staff ??= new Map()).get(id)
    if (!t) return fail(res, 404, 'not_found')
    if (assignee !== undefined) t.assignment = { ...t.assignment, assignee_display_name: assignee }
    if (team !== undefined) t.assignment = { team_name: team, assignee_display_name: null }
    t.messages.push({ id: randomUUID(), kind: 'public_reply', author_kind: 'requester', author_account_id: null, author_display_name: 'requester', created_at: new Date().toISOString(), body: 'Newer reply from the requester', screenshot_ids: [] })
    t.updated_at = new Date().toISOString()
    return reply(res, 204)
  }
  const ownTicket = /^\/api\/tickets\/([^/]+)(?:\/(messages|replies|actions|activity))?$/.exec(pathname)
  if (ownTicket) {
    const source = st.tickets.find((t) => t.id === ownTicket[1])
    if (!source) return fail(res, 404, 'not_found')
    const t = detailOf(source), endpoint = ownTicket[2]
    const all = st.messages.get(t.id) ?? []
    if (!endpoint && req.method === 'GET') return reply(res, 200, t)
    if (endpoint === 'activity') return reply(res, 200, { version: `${t.updated_at}.${t.status}.${all.length}` })
    if (endpoint === 'messages') {
      const q = new URL(req.url, 'http://stub').searchParams, start = Number(q.get('cursor') ?? 0), limit = Number(q.get('limit') ?? 25)
      const visible = all.filter((m) => m.kind === 'public_reply')
      return reply(res, 200, { items: visible.slice(start, start + limit), next_cursor: start + limit < visible.length ? String(start + limit) : null })
    }
    const input = await readBody(req)
    if (endpoint === 'replies') {
      const prior = all.find((m) => m.key === input.idempotency_key)
      if (prior) return prior.body === input.body ? reply(res, 201, prior) : fail(res, 409, 'conflict')
      if (!OPEN.includes(t.status)) return fail(res, 409, 'conflict')
      if (!input.body?.trim()) return validation(res, { body: ['required'] })
      if ((input.screenshot_ids ?? []).some((id) => !st.shots.has(id))) return validation(res, { screenshot_ids: ['invalid'] })
      const message = { id: randomUUID(), kind: 'public_reply', author_kind: 'requester', author_account_id: null, author_display_name: user, created_at: new Date().toISOString(), body: input.body, screenshot_ids: input.screenshot_ids ?? [], key: input.idempotency_key }
      all.push(message); st.messages.set(t.id, all)
      if (t.status === 'waiting_for_requester') changeTicket(source, 'requester_responded', 'in_progress')
      return reply(res, 201, message)
    }
    if (endpoint === 'actions') {
      const policy = { ...settings, ...st.policy }, a = input.action
      // AI decisions (docs/ai-decisions.md): a repeat of the committed decision is 200, another
      // decision after the stage ended is 409 with the committed status, a forbidden skip is 403.
      if (['ai_solved', 'ai_not_solved', 'skip_ai'].includes(a)) {
        st.aiActions.push(a)
        const conflict = () => reply(res, 409, { error: { code: 'conflict', message: 'conflict', details: { fields: {}, status: source.status } }, request_id: 'stub' })
        const decide = () => {
          const now = new Date().toISOString(), skipAllowed = aiPolicy(st).requester_skip_allowed
          if ((source.transitions ?? []).some((e) => e.event_type === a)) return reply(res, 200, { status: source.status })
          if (a === 'skip_ai') {
            if (source.status !== 'ai_processing') return conflict()
            if (!skipAllowed) return fail(res, 403, 'forbidden')
            changeTicket(source, 'skip_ai', 'queued', { route: 'requester_skip' })
            source.ai = { ...source.ai, state: 'handed_off', reason: 'requester_skip', finished_at: now }
          } else {
            if (source.status === 'ai_processing' && a === 'ai_not_solved' && !skipAllowed) return fail(res, 403, 'forbidden')
            if (source.status !== 'awaiting_ai_decision') return conflict()
            if (a === 'ai_solved') {
              changeTicket(source, 'ai_solved', 'ai_solved')
              Object.assign(source, { resolution_source: 'ai', closure_reason: 'requester_confirmed' })
            } else changeTicket(source, 'ai_not_solved', 'queued', { route: 'not_solved' })
            source.ai = { ...source.ai, decision: a === 'ai_solved' ? 'solved' : 'not_solved', decided_at: now }
          }
          reply(res, 200, { status: source.status })
        }
        // Slow enough that a second click lands while the first request is still running.
        return void setTimeout(decide, 300)
      }
      if (!['accept_resolution', 'reject_resolution', 'cancel', 'reopen'].includes(a)) return validation(res, { action: ['invalid'] })
      if (['ai_solved', 'cancelled'].includes(t.status)) return fail(res, 409, 'conflict')
      if (a === 'reopen') {
        if (t.status !== 'closed') return fail(res, 409, 'conflict')
        if (!policy.allow_reopening) return fail(res, 403, 'forbidden')
        changeTicket(source, 'reopen', 'in_progress'); source.closure_reason = null; source.resolution_source = null
      } else if (a === 'cancel') {
        if (!input.reason?.trim()) return validation(res, { reason: ['required'] })
        if (input.reason.length > 1000) return validation(res, { reason: ['too_long'] })
        if (!['ai_processing', 'awaiting_ai_decision', ...OPEN].includes(t.status)) return fail(res, 409, 'conflict')
        if (policy.requester_cancellation_policy === 'disabled' || (policy.requester_cancellation_policy === 'before_work_starts' && t.transitions.some((e) => e.to_status === 'in_progress'))) return fail(res, 403, 'forbidden')
        if (source.ai?.state === 'processing') source.ai = { ...source.ai, state: 'cancelled', finished_at: new Date().toISOString() }
        changeTicket(source, 'cancel', 'cancelled', { reason: input.reason.trim() })
      } else {
        if (t.status !== 'awaiting_confirmation' || t.proposal_version !== input.proposal_version) return fail(res, 409, 'conflict')
        changeTicket(source, a, a === 'accept_resolution' ? 'closed' : 'in_progress', { proposal_version: input.proposal_version })
        if (a === 'accept_resolution') { source.resolution_source = 'operator'; source.closure_reason = 'requester_confirmed' }
      }
      return reply(res, 200, { status: source.status })
    }
  }
  if (route === 'GET /api/catalog') return reply(res, 200, CATALOG)
  if (route === 'GET /api/test/tickets') return reply(res, 200, st.tickets)
  if (route === 'GET /api/test/submitted') return reply(res, 200, st.submitted)
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
      return submit(res, st, draft.intake, body.idempotency_key, `${draft.id}:${body.version}:${!!body.requester_skip}`, draft.id, !!body.requester_skip, languages[tokenOf(req)] ?? 'en')
    }
  }
  if (route === 'POST /api/tickets') {
    const body = await readBody(req)
    return submit(res, st, body.intake, body.idempotency_key, JSON.stringify([body.intake, !!body.requester_skip]), undefined, !!body.requester_skip, languages[tokenOf(req)] ?? 'en')
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
    if (!shot || shot.private) return fail(res, 404, 'not_found')
    if (shotId[2]) return (res.writeHead(200, { 'content-type': 'image/png' }), res.end(PNG))
    return reply(res, 200, shot)
  }
  fail(res, 404, 'not_found')
}).listen(Number(process.env.PORT), '127.0.0.1')
