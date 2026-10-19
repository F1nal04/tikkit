import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'
vi.mock('$app/navigation', () => ({ invalidateAll: vi.fn() }))
const settings = { name: 'Desk', timezone: 'UTC', default_language: 'en' as const, allow_reopening: false, requester_cancellation_policy: 'anytime_while_open' as const, resolution_confirmation_hours: 168, allow_requester_ai_skip: false }
afterEach(() => vi.unstubAllGlobals())

test('submits organization policy and preserves input after a localized validation failure', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { code: 'validation_failed', details: { fields: { timezone: ['invalid'] } } }, request_id: 'r' }), { status: 422, headers: { 'content-type': 'application/json' } }))
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: { locale: 'de', user: null, settings }, params: {} } })
  await fireEvent.input(screen.getByLabelText('Zeitzone'), { target: { value: 'Wrong/Zone' } })
  await fireEvent.click(screen.getByLabelText('Erneutes Öffnen menschlich geschlossener Tickets erlauben'))
  await fireEvent.submit(screen.getByRole('button', { name: 'Speichern' }).closest('form')!)
  expect(await screen.findByRole('alert')).toBeTruthy()
  expect((screen.getByLabelText('Zeitzone') as HTMLInputElement).value).toBe('Wrong/Zone')
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/admin/settings')
  expect(JSON.parse(init.body)).toMatchObject({ timezone: 'Wrong/Zone', allow_reopening: true, resolution_confirmation_hours: 168 })
  expect(screen.getByLabelText('Zeitzone').getAttribute('aria-invalid')).toBe('true')
})
