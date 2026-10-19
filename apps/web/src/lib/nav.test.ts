import type { Role } from '@tikkit/api-client'
import { afterEach, expect, test, vi } from 'vitest'
import { api } from './api.ts'
import { activeHref, canAccess, navFor } from './nav.ts'
import { safeNext } from './redirect.ts'
import { session } from './session.svelte.ts'
import { TICKET_STATUSES } from './status.ts'
import { messages } from './i18n.ts'

const hrefs = (role: Role) => navFor(role).flatMap((g) => g.items.map((i) => i.href))

test('navigation follows the role', () => {
  expect(hrefs('user')).toEqual(['/', '/tickets/new'])
  // Operator and Specialist are identical.
  expect(hrefs('specialist')).toEqual(hrefs('operator'))
  expect(hrefs('operator')).toEqual(['/', '/tickets/new', '/queue'])
  // Team management only for Team Lead; global administration only for Admin.
  expect(hrefs('team_lead')).toEqual(['/', '/tickets/new', '/queue', '/team'])
  expect(hrefs('admin')).toEqual(['/', '/tickets/new', '/queue', '/admin', '/admin/accounts', '/admin/teams', '/admin/settings', '/admin/email'])
})

test('route guards deny by prefix, including pages that have no nav entry yet', () => {
  expect(canAccess('user', '/queue')).toBe(false)
  expect(canAccess('operator', '/admin/accounts')).toBe(false)
  expect(canAccess('team_lead', '/admin')).toBe(false)
  expect(canAccess('operator', '/team')).toBe(false)
  expect(canAccess('admin', '/team')).toBe(false)
  expect(canAccess('admin', '/admin/teams/42')).toBe(true)
  expect(canAccess('team_lead', '/team/members')).toBe(true)
  expect(canAccess('user', '/profile')).toBe(true)
  // A prefix match needs a path boundary.
  expect(canAccess('user', '/administration')).toBe(true)
  expect(canAccess('user', '/%61dmin')).toBe(false)
  expect(canAccess('operator', '/%61dmin/accounts')).toBe(false)
  expect(canAccess('user', '/%71ueue')).toBe(false)
  expect(canAccess('user', '/%74eam')).toBe(false)
})

test('the active entry is the most specific one', () => {
  expect(activeHref('/')).toBe('/')
  expect(activeHref('/admin/accounts')).toBe('/admin/accounts')
  expect(activeHref('/admin')).toBe('/admin')
  expect(activeHref('/admin/settings')).toBe('/admin/settings')
  expect(activeHref('/tickets/new')).toBe('/tickets/new')
  expect(activeHref('/profile')).toBeUndefined()
  expect(activeHref('/%61dmin/accounts')).toBe('/admin/accounts')
})

test('next only accepts same-origin paths', () => {
  expect(safeNext('/queue?x=1')).toBe('/queue?x=1')
  expect(safeNext('//evil.example')).toBeNull()
  expect(safeNext('/\\evil.example')).toBeNull()
  expect(safeNext('https://evil.example')).toBeNull()
  expect(safeNext('/a\nb')).toBeNull()
  expect(safeNext('/login?next=/')).toBeNull()
  expect(safeNext(null)).toBeNull()
})

test('every status has an English and a German label', () => {
  for (const locale of ['en', 'de'] as const)
    for (const status of TICKET_STATUSES) expect(messages[locale].statuses[status]).toBeTruthy()
})

afterEach(() => {
  vi.unstubAllGlobals()
  session.expired = false
})

const respond = (status: number, body: unknown) =>
  vi.stubGlobal(
    'fetch',
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } }),
    ),
  )

test('a 401 flags the expired session and a later success clears it', async () => {
  respond(401, { error: { code: 'unauthorized', message: 'x' }, request_id: 'r' })
  await expect(api('en').request('get', '/api/auth/me')).rejects.toMatchObject({ code: 'unauthorized' })
  expect(session.expired).toBe(true)
  respond(200, { id: '1' })
  await api('en').request('get', '/api/auth/me')
  expect(session.expired).toBe(false)
})

test('a failed sign-in is not an expired session', async () => {
  respond(401, { error: { code: 'invalid_credentials', message: 'x' }, request_id: 'r' })
  await expect(
    api('en').request('post', '/api/auth/login', { body: { username: 'a', password: 'b' } }),
  ).rejects.toBeDefined()
  expect(session.expired).toBe(false)
})
