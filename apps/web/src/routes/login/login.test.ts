import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

const { goto } = vi.hoisted(() => ({ goto: vi.fn() }))
vi.mock('$app/navigation', () => ({ goto }))

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

afterEach(() => {
  vi.unstubAllGlobals()
  goto.mockReset()
})

async function signIn(locale: 'en' | 'de', response: Response) {
  const fetch = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: { locale, user: null, next: null }, params: {} } })
  await fireEvent.input(screen.getByLabelText(locale === 'en' ? 'Username' : 'Benutzername'), {
    target: { value: 'Alice' },
  })
  await fireEvent.input(screen.getByLabelText(locale === 'en' ? 'Password' : 'Passwort'), {
    target: { value: 'correct horse battery' },
  })
  await fireEvent.submit(screen.getByRole('button').closest('form')!)
  return fetch
}

test('posts credentials with the CSRF header and navigates home', async () => {
  const fetch = await signIn('en', json(200, { id: '1', username: 'alice' }))
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/auth/login')
  expect(init.method).toBe('POST')
  expect(init.headers['x-requested-with']).toBe('tikkit')
  expect(JSON.parse(init.body)).toEqual({ username: 'Alice', password: 'correct horse battery' })
  await waitFor(() => expect(goto).toHaveBeenCalledWith('/', { invalidateAll: true }))
})

test('shows one generic message for failed logins', async () => {
  await signIn(
    'de',
    json(401, { error: { code: 'invalid_credentials', message: 'x' }, request_id: 'r' }),
  )
  const alert = await screen.findByRole('alert')
  expect(alert.textContent).toBe('Benutzername oder Passwort ist falsch.')
  expect(goto).not.toHaveBeenCalled()
  expect((screen.getByLabelText('Passwort') as HTMLInputElement).value).toBe('')
})

test('explains rate limiting', async () => {
  await signIn('en', json(429, { error: { code: 'rate_limited', message: 'x' }, request_id: 'r' }))
  expect((await screen.findByRole('alert')).textContent).toContain('Too many attempts')
})

test('keeps the entered password when a temporary outage prevents sign-in', async () => {
  await signIn('en', json(503, { error: { code: 'unavailable', message: 'x' }, request_id: 'r' }))
  await screen.findByRole('alert')
  expect((screen.getByLabelText('Password') as HTMLInputElement).value).toBe('correct horse battery')
})
