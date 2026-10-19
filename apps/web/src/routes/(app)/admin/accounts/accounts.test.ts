import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import type { AccountSummary, Schemas } from '@tikkit/api-client'
import AccountEditor from './AccountEditor.svelte'
import TeamEditor from '../teams/TeamEditor.svelte'
vi.mock('$app/navigation', () => ({ invalidateAll: vi.fn() }))
const account: AccountSummary = { id: 'account-1', username: 'operator.one', display_name: 'One', email: 'shared@example.com', role: 'operator', language: 'en', is_active: true, expertise: null, team_id: 'team-1', is_team_lead: false, created_at: '2026-01-01T00:00:00Z' }
const teams: Schemas['Team'][] = [{ id: 'team-1', name: 'Support', lead_account_id: 'lead', archived: false, created_at: account.created_at }]
afterEach(() => vi.unstubAllGlobals())

test('Admin role has no membership selector and promotion clears membership for the selected ID', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
  vi.stubGlobal('fetch', fetch)
  render(AccountEditor, { props: { account, teams, locale: 'en' } })
  await fireEvent.click(screen.getByText('Edit account: operator.one'))
  await fireEvent.change(screen.getByLabelText('Role'), { target: { value: 'admin' } })
  expect(screen.queryByLabelText('Team')).toBeNull()
  await fireEvent.submit(screen.getByRole('button', { name: 'Save' }).closest('form')!)
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/accounts/account-1')
  expect(JSON.parse(init.body)).toEqual({ role: 'admin', expertise: null, team_id: null })
  expect(screen.getByText('Account ID: account-1')).toBeTruthy()
})

test('a rejected lead demotion preserves input and explains replacement in German', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'active_team_lead' }, request_id: 'r' }), { status: 409, headers: { 'content-type': 'application/json' } })))
  render(AccountEditor, { props: { account: { ...account, role: 'team_lead', is_team_lead: true }, teams, locale: 'de' } })
  await fireEvent.click(screen.getByText('Konto bearbeiten: operator.one'))
  await fireEvent.change(screen.getByLabelText('Rolle'), { target: { value: 'operator' } })
  await fireEvent.submit(screen.getByRole('button', { name: 'Speichern' }).closest('form')!)
  expect((await screen.findByRole('alert')).textContent).toContain('andere Teamleitung')
  expect((screen.getByLabelText('Rolle') as HTMLSelectElement).value).toBe('operator')
})

test('lead appointment lists only active Team Leads who do not already lead another team', () => {
  const eligible = { ...account, id: 'eligible', username: 'eligible', role: 'team_lead' as const, team_id: null }
  render(TeamEditor, { props: { locale: 'en', accounts: [account, eligible, { ...eligible, id: 'admin', username: 'admin', role: 'admin' }, { ...eligible, id: 'inactive', username: 'inactive', is_active: false }, { ...eligible, id: 'busy', username: 'busy', is_team_lead: true, team_id: 'other' }] } })
  const options = Array.from((screen.getByLabelText('Team Lead account') as HTMLSelectElement).options)
  expect(options.map((o) => o.value)).toEqual(['', 'eligible'])
})
