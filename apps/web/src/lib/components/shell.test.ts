import type { Account, Role } from '@tikkit/api-client'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import { session } from '#lib/session.svelte.ts'
import { createRawSnippet } from 'svelte'
import LanguageSelect from './LanguageSelect.svelte'
import Shell from './Shell.svelte'

const { goto, invalidateAll } = vi.hoisted(() => ({ goto: vi.fn(), invalidateAll: vi.fn() }))
vi.mock('$app/navigation', () => ({ goto, invalidateAll }))

afterEach(() => {
  vi.unstubAllGlobals()
  goto.mockReset()
  invalidateAll.mockReset()
  session.expired = false
})

const account = (role: Role): Account => ({
  id: '1',
  username: 'alice',
  display_name: 'alice',
  email: 'a@example.com',
  language: 'en',
  role,
})
const children = createRawSnippet(() => ({ render: () => '<h1>Page</h1>' }))

function shell(role: Role, locale: 'en' | 'de' = 'en', path = '/', onlogout = vi.fn()) {
  render(Shell, { props: { user: account(role), locale, path, onlogout, children } })
  return onlogout
}
const links = () => within(screen.getByRole('navigation')).getAllByRole('link').map((a) => a.textContent)

test('shows each role only its own navigation', () => {
  shell('user')
  expect(links()).toEqual(['My tickets', 'New ticket'])
})

test('Operator and Specialist share one workspace, Team Lead adds the team, Admin adds administration', () => {
  shell('specialist')
  expect(links()).toEqual(['My tickets', 'New ticket', 'General queue'])
})

test('Team Lead sees team tools, never administration', () => {
  shell('team_lead')
  expect(links()).toEqual(['My tickets', 'New ticket', 'General queue', 'My team'])
})

test('Admin sees global controls', () => {
  shell('admin')
  expect(links()).toEqual(['My tickets', 'New ticket', 'General queue', 'Overview', 'Accounts'])
})

test('names the account, its role, and the current page for assistive technology', () => {
  shell('team_lead', 'en', '/team')
  expect(screen.getByText('alice')).toBeTruthy()
  expect(screen.getByText('Team Lead')).toBeTruthy()
  expect(screen.getByRole('link', { name: 'My team' }).getAttribute('aria-current')).toBe('page')
  expect(screen.getByRole('link', { name: 'My tickets' }).getAttribute('aria-current')).toBeNull()
  expect(screen.getByRole('main')).toBeTruthy()
})

test('renders German labels', () => {
  shell('admin', 'de')
  expect(links()).toContain('Allgemeine Warteschlange')
  expect(screen.getByText('Administrator')).toBeTruthy()
  expect(screen.getByRole('navigation').getAttribute('aria-label')).toBe('Hauptnavigation')
})

test('signs out', async () => {
  const onlogout = shell('user')
  await fireEvent.click(screen.getByRole('button', { name: 'Sign out' }))
  expect(onlogout).toHaveBeenCalled()
})

test('the mobile menu toggles with aria-expanded', async () => {
  shell('user')
  const toggle = await screen.findByRole('button', { name: 'Menu' })
  expect(toggle.getAttribute('aria-expanded')).toBe('false')
  await fireEvent.click(toggle)
  expect(toggle.getAttribute('aria-expanded')).toBe('true')
  expect(document.getElementById(toggle.getAttribute('aria-controls')!)).toBeTruthy()
})

test('an expired session keeps the page and offers sign-in in a new tab', () => {
  session.expired = true
  shell('user', 'en', '/profile')
  const link = screen.getByRole('link', { name: /Sign in/ })
  expect(link.getAttribute('href')).toBe('/login?next=%2Fprofile')
  expect(link.getAttribute('target')).toBe('_blank')
  expect(screen.getByRole('heading', { name: 'Page' })).toBeTruthy()
})

test('switching language persists the choice and reloads data without navigating away', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('{}', { headers: { 'content-type': 'application/json' } }))
  vi.stubGlobal('fetch', fetch)
  render(LanguageSelect, { props: { locale: 'en', user: account('user') } })
  await fireEvent.change(screen.getByLabelText('Language'), { target: { value: 'de' } })
  await waitFor(() => expect(document.cookie).toContain('lang=de'))
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/auth/me')
  expect(init.method).toBe('PATCH')
  expect(JSON.parse(init.body)).toEqual({ language: 'de' })
  await waitFor(() => expect(invalidateAll).toHaveBeenCalled())
  expect(goto).not.toHaveBeenCalled()
})

test('before sign-in the language is only a cookie', async () => {
  const fetch = vi.fn()
  vi.stubGlobal('fetch', fetch)
  render(LanguageSelect, { props: { locale: 'de', user: null } })
  await fireEvent.change(screen.getByLabelText('Sprache'), { target: { value: 'en' } })
  expect(document.cookie).toContain('lang=en')
  expect(fetch).not.toHaveBeenCalled()
  await waitFor(() => expect(invalidateAll).toHaveBeenCalled())
})

test.each([401, 503])('a failed language save (%s) leaves the mounted form in place and reports the failure', async (status) => {
  const code = status === 401 ? 'unauthorized' : 'unavailable'
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code }, request_id: 'r' }), {
    status, headers: { 'content-type': 'application/json' },
  })))
  document.cookie = 'lang=en; path=/'
  render(LanguageSelect, { props: { locale: 'en', user: account('user') } })
  await fireEvent.change(screen.getByLabelText('Language'), { target: { value: 'de' } })
  await screen.findByRole('alert')
  expect(invalidateAll).not.toHaveBeenCalled()
  expect(goto).not.toHaveBeenCalled()
  expect(document.cookie).toContain('lang=en')
  expect((screen.getByLabelText('Language') as HTMLSelectElement).value).toBe('en')
  expect(session.expired).toBe(status === 401)
})
