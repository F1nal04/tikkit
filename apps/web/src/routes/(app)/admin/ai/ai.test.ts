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
const limits = (over: object = {}) => ({
  window_hours: 24,
  org_request_limit: 500 as number | null,
  user_request_limit: 10 as number | null,
  org_token_limit: null as number | null,
  user_token_limit: null as number | null,
  max_concurrency: 2,
  concurrency_wait_seconds: 30,
  breaker_failure_threshold: 5,
  breaker_cooldown_seconds: 60,
  retention_days: 90,
  updated_at: '2026-01-01T00:00:00Z',
  ...over,
})
const counts = (over: object = {}) => ({
  requests: 0,
  succeeded: 0,
  failed: 0,
  refused: 0,
  images: 0,
  input_tokens: null as number | null,
  output_tokens: null as number | null,
  total_tokens: null as number | null,
  usage_unreported: 0,
  ...over,
})
const closed = { state: 'closed' as const, consecutive_failures: 0, open_until: null }
const usage = (over: object = {}) => ({
  days: 30,
  window: { window_hours: 24, requests: 12, request_limit: 500 as number | null, budget_tokens: 34567, estimated_tokens: 9000, token_limit: null as number | null, in_flight: 1, max_concurrency: 2 },
  totals: counts({ requests: 12, succeeded: 9, failed: 3, refused: 2, images: 4, input_tokens: 2400, output_tokens: 900, total_tokens: 1650, usage_unreported: 1 }),
  providers: [
    { provider: 'openai' as const, usage: counts({ requests: 10, succeeded: 9, failed: 1, input_tokens: 1200, output_tokens: 450, total_tokens: 1650 }), circuit: closed },
    { provider: 'gemini' as const, usage: counts(), circuit: { state: 'open' as const, consecutive_failures: 5, open_until: '2026-03-01T10:00:00Z' } },
    { provider: 'anthropic' as const, usage: counts({ requests: 2, succeeded: 2, input_tokens: 1200, output_tokens: 450 }), circuit: closed },
  ],
  outcomes: [
    { status: 'failed' as const, outcome: 'rate_limited', count: 3 },
    { status: 'refused' as const, outcome: 'budget_user_requests', count: 2 },
    { status: 'failed' as const, outcome: 'some_future_code', count: 1 },
  ],
  ...over,
})
const mount = (ai = settings(), locale: 'en' | 'de' = 'en', more: { limits?: ReturnType<typeof limits>; usage?: ReturnType<typeof usage> } = {}) =>
  render(Page, { props: { data: { locale, user: null, ai, limits: more.limits ?? limits(), usage: more.usage ?? usage() }, params: {} } })
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

const cells = (name: string) => [...screen.getByRole('row', { name: new RegExp(`^${name}`) }).querySelectorAll('td')].map((td) => td.textContent?.trim())

test('reports usage per provider and never shows an unreported token count as a number', () => {
  mount()
  // Calls, succeeded, failed, refused, screenshots, input, output, total, circuit.
  expect(cells('OpenAI')).toEqual(['10', '9', '1', '0', '0', '1,200', '450', '1,650', 'Closed'])
  // Anthropic reports no total: it is not summed locally.
  expect(cells('Anthropic')).toEqual(['2', '2', '0', '0', '0', '1,200', '450', 'not reported', 'Closed'])
  expect(cells('Google Gemini').slice(5)).toEqual(['not reported', 'not reported', 'not reported', expect.stringMatching(/^Open, calls paused until .*2026.* UTC$/)])
  expect(cells('All providers').slice(0, 5)).toEqual(['12', '9', '3', '2', '4'])
  const window = document.querySelector('[data-testid="ai-budget-window"]')!.textContent!.replace(/\s+/g, ' ')
  expect(window).toContain('Requests 12 of 500')
  expect(window).toContain('Tokens counted 34,567 of no limit (of which estimated by Tikkit: 9,000)')
  expect(window).toContain('Calls in progress 1 of 2')
  expect(screen.getByText('Successful calls without any token count from the provider: 1')).toBeTruthy()
  // Failure reasons are translated by code; an unknown code is shown as is.
  expect(cells('Provider rate limit or quota')).toEqual(['3'])
  expect(cells('Request budget of the requester used up')).toEqual(['2'])
  expect(cells('some_future_code')).toEqual(['1'])
  // No cost is shown anywhere, only the statement that there is none.
  expect(screen.getByText(/Tikkit has no price information and therefore shows no cost\./)).toBeTruthy()
  expect(document.body.textContent).not.toMatch(/[$€]/)
})

test('the usage report is localized and reloads for another period', async () => {
  const fetch = vi.fn().mockResolvedValue(json(usage({ days: 7, outcomes: [], totals: counts() })))
  vi.stubGlobal('fetch', fetch)
  mount(settings(), 'de')
  expect(cells('Anthropic')[7]).toBe('nicht gemeldet')
  expect(cells('OpenAI')[5]).toBe('1.200')
  expect(cells('Ratenlimit oder Kontingent des Anbieters')).toEqual(['3'])
  await fireEvent.change(screen.getByLabelText('Zeitraum'), { target: { value: '7' } })
  await fireEvent.click(screen.getByRole('button', { name: 'Aktualisieren' }))
  expect(await screen.findByText('Keine fehlgeschlagenen oder abgelehnten Aufrufe in diesem Zeitraum.')).toBeTruthy()
  expect(fetch.mock.calls[0][0]).toBe('/api/settings/ai/usage?days=7')
})

test('saves limits as whole numbers, with an empty budget meaning no limit', async () => {
  const saved = limits({ user_request_limit: 3, org_request_limit: null, org_token_limit: 2000000 })
  const fetch = vi.fn().mockResolvedValueOnce(json(saved)).mockResolvedValueOnce(json(usage()))
  vi.stubGlobal('fetch', fetch)
  mount()
  expect((screen.getByLabelText('Requests per window, whole organization') as HTMLInputElement).value).toBe('500')
  expect((screen.getByLabelText('Tokens per window, per requester') as HTMLInputElement).value).toBe('')
  await fireEvent.input(screen.getByLabelText('Requests per window, per requester'), { target: { value: ' 3 ' } })
  await fireEvent.input(screen.getByLabelText('Requests per window, whole organization'), { target: { value: '' } })
  await fireEvent.input(screen.getByLabelText('Tokens per window, whole organization'), { target: { value: '2000000' } })
  await fireEvent.submit(screen.getByRole('button', { name: 'Save limits' }).closest('form')!)
  expect(await screen.findByText('Saved.')).toBeTruthy()
  const [url, init] = fetch.mock.calls[0]
  expect(url).toBe('/api/settings/ai/limits')
  expect(init.method).toBe('PUT')
  expect(JSON.parse(init.body)).toEqual({
    window_hours: 24,
    org_request_limit: null,
    user_request_limit: 3,
    org_token_limit: 2000000,
    user_token_limit: null,
    max_concurrency: 2,
    concurrency_wait_seconds: 30,
    breaker_failure_threshold: 5,
    breaker_cooldown_seconds: 60,
    retention_days: 90,
  })
  // The report is read again, because the budget window shows the limits.
  expect(fetch.mock.calls[1][0]).toBe('/api/settings/ai/usage?days=30')
})

test('rejected limits stay in the form with the reason on the field', async () => {
  const rejected = { error: { code: 'validation_failed', message: '', details: { fields: { max_concurrency: ['out_of_range'], retention_days: ['shorter_than_window'] } } }, request_id: 't' }
  const fetch = vi.fn().mockResolvedValue(json(rejected, 422))
  vi.stubGlobal('fetch', fetch)
  mount(settings(), 'de')
  const form = screen.getByRole('button', { name: 'Limits speichern' }).closest('form')!
  // Not a whole number, and a required value left empty: nothing is sent.
  await fireEvent.input(screen.getByLabelText('Gleichzeitig laufende Aufrufe'), { target: { value: '1.5' } })
  await fireEvent.input(screen.getByLabelText('Budgetfenster (Stunden)'), { target: { value: '' } })
  await fireEvent.submit(form)
  expect(await screen.findByText('Dieser Wert ist ungültig.')).toBeTruthy()
  expect(screen.getByLabelText('Budgetfenster (Stunden)').getAttribute('aria-invalid')).toBe('true')
  expect(fetch).not.toHaveBeenCalled()

  await fireEvent.input(screen.getByLabelText('Gleichzeitig laufende Aufrufe'), { target: { value: '999' } })
  await fireEvent.input(screen.getByLabelText('Budgetfenster (Stunden)'), { target: { value: '744' } })
  await fireEvent.submit(form)
  expect(await screen.findByText('Dieser Wert liegt außerhalb des erlaubten Bereichs.')).toBeTruthy()
  expect(screen.getByText('Einträge müssen mindestens so lange aufbewahrt werden wie das Budgetfenster.')).toBeTruthy()
  expect(JSON.parse(fetch.mock.calls[0][1].body).max_concurrency).toBe(999)
  expect((screen.getByLabelText('Gleichzeitig laufende Aufrufe') as HTMLInputElement).value).toBe('999')
})
