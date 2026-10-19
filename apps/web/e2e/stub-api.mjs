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
  if (pathname === '/api/accounts' || pathname === '/api/invitations') {
    if (ROLES[user] !== 'admin') return fail(res, 403, 'forbidden')
    const items = pathname === '/api/accounts' ? Object.keys(ROLES).map((u) => ({ ...account(u, null), is_active: true, created_at: '2026-10-01T00:00:00Z' })) : []
    return reply(res, 200, { items, next_cursor: null })
  }
  fail(res, 404, 'not_found')
}).listen(Number(process.env.PORT), '127.0.0.1')
