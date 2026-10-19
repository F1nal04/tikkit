import { fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

const { goto } = vi.hoisted(() => ({ goto: vi.fn() }))
vi.mock('$app/navigation', () => ({ goto }))

afterEach(() => {
  vi.unstubAllGlobals()
  goto.mockReset()
})

const invitation = {
  role: 'operator',
  email: 'x@example.com',
  language: 'de',
  expires_at: '2026-10-20T00:00:00Z',
} as const

test('accepts with the token from the link and never sends a role', async () => {
  const fetch = vi.fn().mockResolvedValue(
    new Response(JSON.stringify({ id: '1' }), { headers: { 'content-type': 'application/json' } }),
  )
  vi.stubGlobal('fetch', fetch)
  render(Page, {
    props: { data: { locale: 'en', user: null, invitation, lookupError: null }, params: { token: 'tok' } },
  })
  expect(screen.getByText('Operator')).toBeTruthy()
  await fireEvent.input(screen.getByLabelText(/^Username/), { target: { value: 'newbie' } })
  await fireEvent.input(screen.getByLabelText(/^Password/), { target: { value: 'correct horse battery' } })
  await fireEvent.submit(screen.getByRole('button').closest('form')!)
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/invitations/accept')
  expect(JSON.parse(init.body)).toEqual({
    token: 'tok',
    username: 'newbie',
    email: 'x@example.com',
    password: 'correct horse battery',
    language: 'de',
  })
  await waitFor(() => expect(goto).toHaveBeenCalledWith('/', { invalidateAll: true }))
})

test('explains an expired, revoked, or used link instead of the form', () => {
  render(Page, {
    props: {
      data: { locale: 'de', user: null, invitation: null, lookupError: 'invitation_invalid' },
      params: { token: 'tok' },
    },
  })
  expect(screen.getByRole('alert').textContent).toContain('Einladungslink ist ungültig')
  expect(screen.queryByRole('button')).toBeNull()
})
