import { fireEvent, render, screen } from '@testing-library/svelte'
import { afterEach, expect, test, vi } from 'vitest'
import Page from './+page.svelte'

// Assembled so the secret scanner does not take the fixture for a real key.
const SECRET = ['sk-test', '0123456789abcdefWXYZ'].join('-')
const stored = { hint: 'WXYZ', set_at: '2026-01-01T00:00:00Z', set_by: null, set_by_username: 'admin', readable: true }
const models = {
  openai: [{ id: 'openai-a', image_input: true }, { id: 'openai-b', image_input: true }, { id: 'openai-text', image_input: false }],
  gemini: [{ id: 'gemini-a', image_input: true }],
  anthropic: [{ id: 'anthropic-a', image_input: true }],
}
const settings = (over: object = {}) => ({
  enabled: false,
  provider: 'openai' as const,
  model: 'openai-b',
  status: 'disabled' as const,
  active: false,
  allow_requester_skip: false,
  providers: [
    { provider: 'openai' as const, models: models.openai, credential: stored },
    { provider: 'gemini' as const, models: models.gemini, credential: null },
    { provider: 'anthropic' as const, models: models.anthropic, credential: null },
  ],
  connection: { status: 'untested' as const, error: null, tested_at: null },
  updated_at: '2026-01-01T00:00:00Z',
  ...over,
})
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
const mount = (ai = settings(), locale: 'en' | 'de' = 'en') => render(Page, { props: { data: { locale, user: null, ai }, params: {} } })
afterEach(() => vi.unstubAllGlobals())

test('shows masked key metadata, never a key, and only the three providers', () => {
  mount(settings(), 'de')
  expect(screen.getByText(/Schlüssel gespeichert, endet auf …WXYZ/)).toBeTruthy()
  expect(screen.getAllByText('Kein Schlüssel gespeichert.')).toHaveLength(2)
  for (const input of document.querySelectorAll<HTMLInputElement>('input[type="password"]')) expect(input.value).toBe('')
  const options = [...(screen.getByLabelText('Anbieter') as HTMLSelectElement).options].map((o) => o.value)
  expect(options).toEqual(['openai', 'gemini', 'anthropic'])
  // The model is chosen from the API's table: image-capable models of the selected provider only.
  const model = screen.getByLabelText('Modell') as HTMLSelectElement
  expect(model.tagName).toBe('SELECT')
  expect([...model.options].map((o) => o.value)).toEqual(['openai-a', 'openai-b'])
  expect(model.value).toBe('openai-b')
  expect(screen.getByRole('status').textContent).toContain('Die KI-Unterstützung ist aus')
})

test('stores a key for exactly the provider it was typed for and clears the input', async () => {
  const saved = settings({ providers: settings().providers.map((p) => (p.provider === 'anthropic' ? { ...p, credential: { ...stored, hint: 'WXYZ' } } : p)) })
  const fetch = vi.fn().mockResolvedValue(json(saved))
  vi.stubGlobal('fetch', fetch)
  mount()
  const input = screen.getByLabelText('API key (Anthropic)') as HTMLInputElement
  await fireEvent.input(input, { target: { value: SECRET } })
  await fireEvent.submit(input.closest('form')!)
  expect(await screen.findByText('The API key was stored.')).toBeTruthy()
  expect(fetch).toHaveBeenCalledTimes(1)
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/settings/ai/credentials/anthropic')
  expect(init.method).toBe('PUT')
  expect(JSON.parse(init.body)).toEqual({ api_key: SECRET })
  expect(input.value).toBe('')
  expect(document.body.textContent).not.toContain(SECRET)
})

test('saving the selection sends no key and reports why enabling was refused', async () => {
  const fetch = vi.fn().mockResolvedValue(json({ error: { code: 'validation_failed', message: '', details: { fields: { enabled: ['credential_required'] } } }, request_id: 't' }, 422))
  vi.stubGlobal('fetch', fetch)
  mount()
  await fireEvent.change(screen.getByLabelText('Provider'), { target: { value: 'gemini' } })
  await fireEvent.click(screen.getByLabelText('Enable AI assistance'))
  await fireEvent.submit(screen.getByRole('button', { name: 'Save' }).closest('form')!)
  expect(await screen.findByText('Store an API key for this provider first.')).toBeTruthy()
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/settings/ai')
  // Switching the provider switched the model list with it.
  expect([...(screen.getByLabelText('Model') as HTMLSelectElement).options].map((o) => o.value)).toEqual(['gemini-a'])
  expect(JSON.parse(init.body)).toEqual({ enabled: true, provider: 'gemini', model: 'gemini-a' })
  // What was chosen stays after the rejection.
  expect((screen.getByLabelText('Enable AI assistance') as HTMLInputElement).checked).toBe(true)
})

test('explains an unreadable key and a failed connection test', async () => {
  const unreadable = settings({
    enabled: true,
    status: 'credential_unreadable' as const,
    providers: settings().providers.map((p) => (p.credential ? { ...p, credential: { ...p.credential, readable: false } } : p)),
  })
  const failed = { ...unreadable, connection: { status: 'failed' as const, error: 'ai_provider_auth_failed', tested_at: '2026-02-01T00:00:00Z' } }
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(json({ error: { code: 'ai_provider_auth_failed', message: '' }, request_id: 't' }, 502))
    .mockResolvedValueOnce(json(failed))
  vi.stubGlobal('fetch', fetch)
  mount(unreadable)
  expect(screen.getByText('The stored API key could not be read. Enter it again.')).toBeTruthy()
  await fireEvent.click(screen.getByRole('button', { name: 'Test connection' }))
  expect(await screen.findByText(/The last test failed\. The AI provider rejected the API key\./)).toBeTruthy()
  expect(fetch.mock.calls[0][0]).toBe('/api/settings/ai/test')
  expect(fetch.mock.calls[0][1].method).toBe('POST')
})

test('a stored model outside the table is not offered and a rejected model is explained', async () => {
  const fetch = vi.fn().mockResolvedValue(json({ error: { code: 'validation_failed', message: '', details: { fields: { model: ['unsupported'] } } }, request_id: 't' }, 422))
  vi.stubGlobal('fetch', fetch)
  mount(settings({ model: 'retired-model' }), 'de')
  const model = screen.getByLabelText('Modell') as HTMLSelectElement
  await vi.waitFor(() => expect(model.value).toBe('openai-a'))
  expect([...model.options].map((o) => o.value)).not.toContain('retired-model')
  await fireEvent.submit(screen.getByRole('button', { name: 'Speichern' }).closest('form')!)
  expect(await screen.findByText('Dieses Modell wird nicht unterstützt. Wählen Sie eines aus der Liste.')).toBeTruthy()
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ enabled: false, provider: 'openai', model: 'openai-a' })
  expect(model.getAttribute('aria-invalid')).toBe('true')
})
