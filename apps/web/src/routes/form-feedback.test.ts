import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import type { Component } from 'svelte'
import Login from './login/+page.svelte'
import Setup from './setup/+page.svelte'
import Invite from './invite/[token]/+page.svelte'
import Recovery from './forgot-password/+page.svelte'
import Reset from './reset-password/[token]/+page.svelte'
import Profile from './(app)/profile/+page.svelte'
import Accounts from './(app)/admin/accounts/+page.svelte'

vi.mock('$app/navigation', () => ({ goto: vi.fn(), invalidateAll: vi.fn() }))
afterEach(() => vi.unstubAllGlobals())

const data = {
  locale: 'en' as 'en' | 'de', next: null, lookupError: null,
  user: { id: '1', username: 'alice', display_name: 'Alice', email: 'a@example.com', role: 'admin' as const, language: 'en' as const },
  invitation: { role: 'operator' as const, language: 'en' as const, email: 'a@example.com', expires_at: '2026-10-20T00:00:00Z' },
  accounts: [], invitations: [],
}

const forms: { page: Component<{ data: typeof data; params: { token: string } }>; button: string }[] = [
  { page: Login, button: 'Sign in' },
  { page: Setup, button: 'Create administrator' },
  { page: Invite, button: 'Create account' },
  { page: Recovery, button: 'Send reset link' },
  { page: Reset, button: 'Set password' },
  { page: Profile, button: 'Save' },
  { page: Profile, button: 'Change password' },
  { page: Accounts, button: 'Create invitation' },
]

test.each(forms)('$button feedback follows the current language after a failed request', async ({ page, button }) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
    error: { code: 'unavailable' }, request_id: 'r',
  }), { status: 503, headers: { 'content-type': 'application/json' } })))
  const { rerender } = render(page, { props: { data, params: { token: 'tok' } } })
  await fireEvent.submit(screen.getByRole('button', { name: button }).closest('form')!)
  expect((await screen.findByRole('alert')).textContent).toContain('The service is unavailable.')
  await rerender({ data: { ...data, locale: 'de' } })
  expect(screen.getByRole('alert').textContent).toContain('Der Dienst ist nicht erreichbar.')
})
