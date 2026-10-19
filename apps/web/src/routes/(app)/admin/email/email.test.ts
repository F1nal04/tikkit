import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'
const connection = { host: 'smtp.example.com', port: 587, security: 'starttls' as const, sender_name: 'Desk', sender_address: 'help@example.com', password_set: true, enabled: true, updated_at: '2026-01-01T00:00:00Z', last_delivery_error: null, last_delivery_error_at: null, last_test_at: null, last_test_error: null }
afterEach(() => vi.unstubAllGlobals())

test('keeps stored credentials when left blank and never prefills them', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ connection }), { status: 200, headers: { 'content-type': 'application/json' } }))
  vi.stubGlobal('fetch', fetch)
  render(Page, { props: { data: { locale: 'de', user: null, smtp: { connection }, accountEmail: 'admin@example.com' }, params: {} } })
  expect((screen.getByLabelText('Benutzername') as HTMLInputElement).value).toBe('')
  await fireEvent.input(screen.getByLabelText('Absendername'), { target: { value: 'Help Desk' } })
  await fireEvent.submit(screen.getByRole('button', { name: 'Speichern' }).closest('form')!)
  expect(await screen.findByRole('status')).toBeTruthy()
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/settings/smtp')
  const body = JSON.parse(init.body)
  expect(body).toMatchObject({ host: 'smtp.example.com', port: 587, sender_name: 'Help Desk', enabled: true })
  expect(body).not.toHaveProperty('username')
  expect(body).not.toHaveProperty('password')
})
