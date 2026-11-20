import { devices, expect, type Page, test } from '@playwright/test'
import { proxyApi, signIn } from './support'

const SOFTWARE = '00000000-0000-4000-9000-000000000001'
const WORD = '00000000-0000-4000-9000-000000000030'
const PROGRAM = '00000000-0000-4000-9000-000000000010'
const HIGH = '00000000-0000-4000-9000-000000000022'

async function request(page: Page, path: string, body?: unknown, method = 'POST') {
  return page.evaluate(async ({ path, body, method }) => {
    const r = await fetch(path, body === undefined ? {} : { method, headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify(body) })
    const text = await r.text()
    let data
    try { data = JSON.parse(text) } catch { data = null }
    return { status: r.status, body: data }
  }, { path, body, method })
}
type AiConfig = { ai_assistance: boolean; requester_skip_allowed: boolean }
const configure = (page: Page, config: Partial<AiConfig>) => request(page, '/api/test/ai', config)
const aiLog = async (page: Page) => (await request(page, '/api/test/ai-log')).body as { provider_calls: number; actions: string[] }
const tickets = async (page: Page) => (await request(page, '/api/test/tickets')).body as { id: string; status: string; reference: number }[]
const aiResult = (page: Page, id: string, extra: object = {}) => request(page, '/api/test/ai-result', { id, ...extra })
const ticketId = (page: Page) => new URL(page.url()).pathname.split('/').pop()!
const step = (page: Page) => page.locator('section.ai-step')
const badge = (page: Page, status: string) => page.locator(`.badge[data-status="${status}"]`)

/** Signed in as a fresh session with the Admin's AI configuration applied. */
async function start(page: Page, config: Partial<AiConfig> = { ai_assistance: true }) {
  await signIn(page, 'requester')
  await configure(page, config)
}
async function openForm(page: Page) {
  await page.goto('/tickets/new')
  // Wait for hydration so typed input is not reset by it.
  await page.waitForLoadState('networkidle')
}
async function fill(page: Page, category = 'Software') {
  await page.getByLabel('Category').selectOption({ label: category })
  if (category === 'Software') await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
}
const submit = (page: Page) => page.getByRole('button', { name: 'Submit request' }).click()
/** Submits a non-exempt request with AI on and lands on its ticket page in the waiting state. */
async function submitToAi(page: Page) {
  await openForm(page)
  await fill(page)
  await submit(page)
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}$/)
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeVisible()
  await page.waitForLoadState('networkidle')
  return ticketId(page)
}
async function toSuggestion(page: Page) {
  const id = await submitToAi(page)
  expect((await aiResult(page, id)).body).toEqual({ applied: true })
  await expect(page.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
  return id
}

test('AI switched on while the form is open: the disclosure is shown before anything is sent', async ({ page }) => {
  await start(page, {})
  await openForm(page)
  await fill(page)
  await expect(page.getByRole('heading', { name: 'AI assistance' })).toHaveCount(0)
  await configure(page, { ai_assistance: true })
  await submit(page)
  await expect(page.getByRole('heading', { name: 'AI assistance' })).toBeFocused()
  await expect(page.getByText('AI assistance was switched on while this form was open.')).toBeVisible()
  expect(await tickets(page)).toEqual([])
  expect((await aiLog(page)).provider_calls).toBe(0)
  await expect(page.getByLabel('Subject')).toHaveValue('Word crashes')
  await submit(page)
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}$/)
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeVisible()
})

test('AI off: no disclosure, no skip control, and the request goes to support without a provider call', async ({ page }) => {
  await start(page, {})
  await openForm(page)
  await fill(page)
  await expect(page.getByRole('heading', { name: 'AI assistance' })).toHaveCount(0)
  await expect(page.getByRole('main')).not.toContainText('AI provider')
  await expect(page.getByRole('checkbox')).toHaveCount(0)
  await submit(page)
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeFocused()
  await expect(page.getByText('It is waiting in the support queue.')).toBeVisible()
  expect((await aiLog(page)).provider_calls).toBe(0)
  await page.getByRole('link', { name: 'Open the ticket' }).click()
  await expect(badge(page, 'queued')).toBeVisible()
  await expect(step(page)).toHaveCount(0)
})

test('mandatory AI by default: disclosure before submission, no skip, and a waiting state without a bypass', async ({ page }) => {
  await start(page)
  await openForm(page)
  await fill(page)
  const disclosure = page.getByRole('region', { name: 'AI assistance' })
  await expect(disclosure).toContainText('are sent to the external AI provider your organization has configured')
  await expect(disclosure).toContainText('subject and description')
  await expect(disclosure).toContainText('such as the affected program')
  await expect(disclosure).toContainText('any attached screenshots')
  await expect(page.getByRole('checkbox')).toHaveCount(0)
  await expect(page.getByText(/Skip the AI suggestion/)).toHaveCount(0)

  await submit(page)
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}$/)
  const id = ticketId(page)
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeVisible()
  await expect(step(page)).toContainText('analyzing your description, the program and other details you entered, and any screenshots you attached')
  await expect(step(page)).toContainText('external AI provider')
  await expect(step(page)).toContainText('goes to the support team automatically')
  await expect(badge(page, 'ai_processing')).toBeVisible()
  // No way around the step: no button in it, no reply box, only the ordinary cancellation outside.
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(page.getByRole('main').getByRole('textbox')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /support/i })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Cancel ticket', exact: true })).toBeVisible()
  expect((await aiLog(page)).provider_calls).toBe(1)

  // Forged requests are refused by the API and change nothing.
  expect((await request(page, `/api/tickets/${id}/actions`, { action: 'skip_ai' })).status).toBe(403)
  expect((await request(page, `/api/tickets/${id}/actions`, { action: 'ai_not_solved' })).status).toBe(403)
  const forged = await request(page, '/api/tickets', {
    idempotency_key: crypto.randomUUID(), requester_skip: true,
    intake: { category_id: SOFTWARE, priority_id: HIGH, subject: 'Forged skip', description: 'Let me through', answers: { [PROGRAM]: { choice_id: WORD } }, screenshot_ids: [] },
  })
  expect(forged.status).toBe(403)
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, status: 'ai_processing' })])
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeVisible()
})

test('skip allowed by the Admin: a skip at submission sends nothing to the provider and shows the support receipt', async ({ page }) => {
  await start(page, { ai_assistance: true, requester_skip_allowed: true })
  await openForm(page)
  await fill(page)
  const skip = page.getByRole('checkbox', { name: 'Skip the AI suggestion and send this request straight to the support team' })
  await expect(skip).not.toBeChecked()
  await skip.check()
  await submit(page)
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeFocused()
  await expect(page.getByText('You skipped the AI suggestion. Nothing was sent to the AI provider.')).toBeVisible()
  expect(await aiLog(page)).toEqual({ provider_calls: 0, actions: [] })
  await page.getByRole('link', { name: 'Open the ticket' }).click()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await expect(step(page)).toContainText('You skipped the AI suggestion.')
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(badge(page, 'queued')).toBeVisible()
})

test('skip allowed by the Admin: the waiting state offers the skip and hands the same request to support', async ({ page }) => {
  await start(page, { ai_assistance: true, requester_skip_allowed: true })
  const id = await submitToAi(page)
  const [before] = await tickets(page)
  await expect(step(page).getByRole('button')).toHaveCount(1)
  await page.getByRole('button', { name: 'Skip the AI suggestion and send to support' }).dblclick()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeFocused()
  await expect(step(page)).toContainText('You skipped the AI suggestion.')
  await expect(step(page)).toContainText(`Your original request #${before.reference} was passed on unchanged`)
  await expect(page.getByTestId('ai-announcement')).toHaveText('Your request was sent to the support team.')
  await expect(badge(page, 'queued')).toBeVisible()
  await expect(page.getByText('General queue', { exact: true })).toBeVisible()
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, reference: before.reference, status: 'queued' })])
  // A late provider result is dropped; the receipt stays.
  expect((await aiResult(page, id)).body).toEqual({ applied: false })
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Solved', exact: true })).toHaveCount(0)
})

test('skip switched off after the page loaded: the form and the waiting state explain the refusal and keep the request', async ({ page }) => {
  await start(page, { ai_assistance: true, requester_skip_allowed: true })
  await openForm(page)
  await fill(page)
  await page.getByRole('checkbox', { name: /Skip the AI suggestion/ }).check()
  await configure(page, { requester_skip_allowed: false })
  await submit(page)
  await expect(page.locator('#form-errors')).toContainText('Skipping the AI suggestion is not allowed by your organization.')
  expect(await tickets(page)).toEqual([])
  // The policy was reloaded: the control is gone, the input is kept.
  await expect(page.getByRole('checkbox')).toHaveCount(0)
  await expect(page.getByLabel('Subject')).toHaveValue('Word crashes')

  await configure(page, { requester_skip_allowed: true })
  await submit(page)
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}$/)
  const skip = page.getByRole('button', { name: 'Skip the AI suggestion and send to support' })
  await expect(skip).toBeVisible()
  await page.waitForLoadState('networkidle')
  await configure(page, { requester_skip_allowed: false })
  await skip.click()
  await expect(page.getByRole('alert').filter({ hasText: 'Skipping the AI suggestion is not allowed by your organization.' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeFocused()
  await expect(skip).toHaveCount(0)
  await expect(badge(page, 'ai_processing')).toBeVisible()
  expect(await tickets(page)).toEqual([expect.objectContaining({ status: 'ai_processing' })])
})

test('an exempt category bypasses AI: support receipt, no suggestion screen, no provider call', async ({ page }) => {
  await start(page, { ai_assistance: true, requester_skip_allowed: true })
  await openForm(page)
  await fill(page)
  await expect(page.getByRole('region', { name: 'AI assistance' })).toContainText('are sent to the external AI provider')
  await page.getByLabel('Category').selectOption({ label: 'Network' })
  const disclosure = page.getByRole('region', { name: 'AI assistance' })
  await expect(disclosure).toContainText('This category goes straight to the support team without AI assistance. Nothing is sent to the AI provider.')
  await expect(disclosure).not.toContainText('are sent to')
  await expect(page.getByRole('checkbox')).toHaveCount(0)
  await submit(page)
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeFocused()
  await expect(page.getByText('No AI suggestion was created: requests in this category go straight to the support team.')).toBeVisible()
  expect(await aiLog(page)).toEqual({ provider_calls: 0, actions: [] })
  await page.getByRole('link', { name: 'Open the ticket' }).click()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await expect(step(page)).toContainText('Requests in this category go straight to the support team.')
  await expect(page.getByTestId('ai-suggestion')).toHaveCount(0)
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(badge(page, 'queued')).toBeVisible()
})

test('success: the one suggestion arrives, is labelled and rendered as plain text, and only Solved records the AI resolution', async ({ page }) => {
  await start(page)
  const id = await submitToAi(page)
  await aiResult(page, id)
  // The open page picks the suggestion up by itself and tells assistive technology.
  await expect(page.getByRole('heading', { name: 'AI-generated suggestion' })).toBeFocused()
  await expect(page.getByTestId('ai-announcement')).toHaveText('The AI suggestion is ready. Review it and choose “Solved” or “Still need help”.')
  await expect(step(page)).toContainText('An AI wrote this suggestion from your request.')
  await expect(step(page)).toContainText('may be incomplete or wrong')
  await expect(step(page)).toContainText('may have misread them')
  await expect(badge(page, 'awaiting_ai_decision')).toBeVisible()

  const suggestion = page.getByTestId('ai-suggestion')
  await expect(suggestion).toHaveAttribute('lang', 'en')
  await expect(suggestion).toContainText('<img src=x onerror="window.aiXss = true"> **Reset** it: [click here](javascript:window.aiXss=true) or https://evil.example/reset')
  await expect(suggestion).toContainText('Your screenshot seems to show error 0x80070005.')
  await expect(suggestion.getByRole('listitem')).toHaveText(['Close Word. <script>window.aiXss = true</script>', 'Rename the folder "Normal.dotm".', 'Start Word again.'])
  await expect(suggestion.locator('img, a, script, strong, em')).toHaveCount(0)
  expect(await page.evaluate('window.aiXss || false')).toBe(false)

  // Exactly two outcomes: no chat, no follow-up, no second suggestion.
  await expect(step(page).getByRole('button')).toHaveText(['Solved', 'Still need help'])
  await expect(page.getByRole('main').getByRole('textbox')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /another|regenerate|retry|again|ask/i })).toHaveCount(0)

  // German interface: the stored text is shown as generated, not translated, not regenerated.
  await page.goto(`/tickets/${id}?lang=de`)
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { name: 'KI-generierter Vorschlag' })).toBeVisible()
  await expect(page.getByText('Der Vorschlag wird genau so angezeigt, wie er erstellt wurde: auf English.')).toBeVisible()
  await expect(page.getByTestId('ai-suggestion')).toHaveAttribute('lang', 'en')
  await expect(page.getByTestId('ai-suggestion')).toContainText('Word keeps a damaged profile.')
  await expect(step(page).getByRole('button')).toHaveText(['Gelöst', 'Ich brauche weiterhin Hilfe'])
  expect(await aiLog(page)).toEqual({ provider_calls: 1, actions: [] })

  await page.getByRole('button', { name: 'Gelöst', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Mit dem KI-Vorschlag gelöst' })).toBeFocused()
  await expect(badge(page, 'ai_solved')).toBeVisible()
  await expect(page.getByTestId('ai-suggestion')).toContainText('Word keeps a damaged profile.')
  // Final and read-only: nothing left to press or type.
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(page.getByRole('main').getByRole('textbox')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /stornieren|wiedereröffnen/i })).toHaveCount(0)
  expect(await aiLog(page)).toEqual({ provider_calls: 1, actions: ['ai_solved'] })
  expect((await request(page, `/api/tickets/${id}/actions`, { action: 'ai_not_solved' })).status).toBe(409)
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, status: 'ai_solved' })])
})

test('Still need help keeps the original submission and shows the support receipt', async ({ page }) => {
  await start(page)
  const id = await toSuggestion(page)
  const [before] = await tickets(page)
  await page.getByRole('button', { name: 'Still need help' }).click()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeFocused()
  await expect(step(page)).toContainText('You told us the AI suggestion did not solve the problem.')
  await expect(step(page)).toContainText(`Your original request #${before.reference} was passed on unchanged, with its description, details, and screenshots.`)
  await expect(page.getByRole('heading', { name: 'The AI-generated suggestion you tried' })).toBeVisible()
  await expect(page.getByTestId('ai-suggestion')).toContainText('Word keeps a damaged profile.')
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(badge(page, 'queued')).toBeVisible()
  // Same ticket, same reference, same intake; now an ordinary support request.
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, reference: before.reference, status: 'queued' })])
  await expect(page.getByRole('heading', { name: new RegExp(`#${before.reference} · Word crashes`) })).toBeVisible()
  await expect(page.getByText('It closes on startup.')).toBeVisible()
  await expect(page.getByRole('form', { name: 'Public reply' })).toBeVisible()
  expect(await aiLog(page)).toEqual({ provider_calls: 1, actions: ['ai_not_solved'] })
})

test('automatic failure hands off without trapping the requester, and a late result does not bring the suggestion back', async ({ page }) => {
  await start(page)
  const id = await submitToAi(page)
  await aiResult(page, id, { outcome: 'timeout' })
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeFocused()
  await expect(step(page)).toContainText('Preparing an AI suggestion took too long, so your request was passed on without one.')
  await expect(page.getByTestId('ai-announcement')).toHaveText('Your request was sent to the support team.')
  await expect(badge(page, 'queued')).toBeVisible()
  await expect(step(page).getByRole('button')).toHaveCount(0)
  await expect(page.getByRole('form', { name: 'Public reply' })).toBeVisible()

  // The state is settled: the page stops asking the server.
  let polls = 0
  page.on('request', (r) => { if (r.url().endsWith('/activity')) polls++ })
  // The worker drops a late result; even one that reached the attempt must not reopen the choice.
  expect((await aiResult(page, id)).body).toEqual({ applied: false })
  await aiResult(page, id, { force: true })
  await page.waitForTimeout(4000)
  expect(polls).toBe(0)
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await expect(step(page)).toContainText('took too long')
  await expect(page.getByRole('button', { name: 'Solved', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Still need help' })).toHaveCount(0)
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, status: 'queued' })])
  expect((await aiLog(page)).actions).toEqual([])

  // The other failure reasons read differently.
  const failed = await submitToAi(page)
  await aiResult(page, failed, { outcome: 'ai_failed' })
  await expect(step(page)).toContainText('No AI suggestion could be prepared, so your request was passed on without one.')
  const unavailable = await submitToAi(page)
  await aiResult(page, unavailable, { outcome: 'ai_unavailable' })
  await expect(step(page)).toContainText('AI assistance was not available, so your request was passed on without a suggestion.')
})

test('refresh, leaving, and coming back restore the persisted step without deciding anything', async ({ page, context }) => {
  await start(page)
  const decisions: string[] = []
  context.on('request', (r) => { if (r.method() === 'POST' && r.url().endsWith('/actions')) decisions.push(r.url()) })
  const id = await submitToAi(page)
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Preparing an AI suggestion' })).toBeVisible()

  // Leave while it is processing; polling ends with the page.
  await page.getByRole('link', { name: 'Back to my tickets' }).click()
  await expect(page.getByRole('table', { name: 'My tickets' })).toBeVisible()
  let polls = 0
  page.on('request', (r) => { if (r.url().endsWith('/activity')) polls++ })
  await aiResult(page, id)
  await page.waitForTimeout(4000)
  expect(polls).toBe(0)

  // Coming back shows the stored suggestion; viewing and refreshing it decides nothing.
  await page.reload()
  await page.getByRole('link', { name: 'Word crashes' }).click()
  await expect(page.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
  await page.reload()
  await expect(page.getByTestId('ai-suggestion')).toContainText('Word keeps a damaged profile.')
  await page.getByRole('link', { name: 'Back to my tickets' }).click()
  await expect(page.getByRole('table', { name: 'My tickets' })).toBeVisible()

  // Closing the tab and opening the ticket later: still undecided, still the same suggestion.
  const later = await context.newPage()
  await proxyApi(later)
  await page.close()
  await later.goto(`/tickets/${id}`)
  await expect(later.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
  await expect(later.locator('section.ai-step').getByRole('button')).toHaveText(['Solved', 'Still need help'])
  expect(decisions).toEqual([])
  expect(await aiLog(later)).toEqual({ provider_calls: 1, actions: [] })
  expect(await tickets(later)).toEqual([expect.objectContaining({ id, status: 'awaiting_ai_decision' })])
})

test('a double click decides once, and a stale tab learns the committed state instead of overriding it', async ({ page, context }) => {
  await start(page)
  const id = await toSuggestion(page)
  // A second tab on the same suggestion that does not notice the change by itself.
  const stale = await context.newPage()
  await proxyApi(stale)
  let unaware = true
  await stale.route('**/api/tickets/*/activity', (route) => (unaware ? route.abort() : route.fallback()))
  stale.on('request', (r) => { if (r.url().endsWith('/actions')) unaware = false })
  await stale.goto(`/tickets/${id}`)
  await expect(stale.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
  await stale.waitForLoadState('networkidle')

  await page.getByRole('button', { name: 'Still need help' }).dblclick()
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  expect((await aiLog(page)).actions).toEqual(['ai_not_solved'])
  // A repeat of the committed decision is safe.
  expect(await request(page, `/api/tickets/${id}/actions`, { action: 'ai_not_solved' })).toEqual({ status: 200, body: { status: 'queued' } })

  await stale.getByRole('button', { name: 'Solved', exact: true }).click()
  await expect(stale.getByRole('alert')).toContainText('This request was already decided or changed somewhere else')
  await expect(stale.getByRole('alert')).toContainText('current state: Queued.')
  await expect(stale.getByRole('heading', { name: 'Sent to the support team' })).toBeFocused()
  await expect(stale.locator('.badge[data-status="queued"]')).toBeVisible()
  await expect(stale.getByRole('button', { name: 'Solved', exact: true })).toHaveCount(0)
  expect(await tickets(page)).toEqual([expect.objectContaining({ id, status: 'queued' })])
})

test('cancelling with a reason stays possible during the AI step and is not a handoff', async ({ page }) => {
  await start(page)
  const id = await submitToAi(page)
  await page.getByRole('button', { name: 'Cancel ticket', exact: true }).click()
  await page.getByLabel('Cancellation reason').fill('Fixed itself.')
  await page.getByRole('button', { name: 'Confirm cancellation' }).click()
  await expect(badge(page, 'cancelled')).toBeVisible()
  await expect(step(page)).toContainText('You cancelled this request during AI assistance. It was not sent to the support team.')
  await expect(step(page).getByRole('button')).toHaveCount(0)
  expect((await aiResult(page, id)).body).toEqual({ applied: false })
  expect((await aiLog(page)).actions).toEqual([])
})

test.describe('AI step on a phone', () => {
  const { defaultBrowserType: _ignored, ...phone } = devices['Pixel 7']
  test.use(phone)
  const fits = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)

  test('German disclosure, waiting state, suggestion, and receipt fit the screen', async ({ page }) => {
    await signIn(page, 'requester')
    await configure(page, { ai_assistance: true, requester_skip_allowed: true })
    await request(page, '/api/auth/me', { language: 'de' }, 'PATCH')
    await openForm(page)
    await page.getByLabel('Kategorie').selectOption({ label: 'Software' })
    await page.getByLabel('Program').selectOption({ label: 'Word' })
    await page.getByLabel('Betreff').fill('Word stürzt ab')
    await page.getByLabel('Beschreibung').fill('Es schließt sich beim Start.')
    await page.getByLabel('Priorität', { exact: true }).selectOption({ label: 'High' })
    const disclosure = page.getByRole('region', { name: 'KI-Unterstützung' })
    await expect(disclosure).toContainText('an den externen KI-Anbieter gesendet, den Ihre Organisation eingerichtet hat')
    await expect(page.getByRole('checkbox', { name: 'KI-Vorschlag überspringen und diese Anfrage direkt an das Support-Team senden' })).toBeVisible()
    expect(await fits(page)).toBe(true)

    await page.getByRole('button', { name: 'Anfrage absenden' }).click()
    await expect(page.getByRole('heading', { name: 'KI-Vorschlag wird vorbereitet' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'KI-Vorschlag überspringen und an den Support senden' })).toBeVisible()
    await page.waitForLoadState('networkidle')
    expect(await fits(page)).toBe(true)

    const id = ticketId(page)
    await aiResult(page, id, { suggestion: { explanation: 'Das Profil von Word ist beschädigt.', steps: ['Word schließen.', 'Den Ordner umbenennen.'] } })
    await expect(page.getByRole('heading', { name: 'KI-generierter Vorschlag' })).toBeVisible()
    await expect(page.getByTestId('ai-suggestion')).toHaveAttribute('lang', 'de')
    await expect(page.locator('section.ai-step').getByRole('button')).toHaveText(['Gelöst', 'Ich brauche weiterhin Hilfe'])
    expect(await fits(page)).toBe(true)

    await page.getByRole('button', { name: 'Ich brauche weiterhin Hilfe' }).click()
    await expect(page.getByRole('heading', { name: 'An das Support-Team gesendet' })).toBeFocused()
    await expect(page.locator('section.ai-step')).toContainText('Du hast angegeben, dass der KI-Vorschlag das Problem nicht gelöst hat.')
    expect(await fits(page)).toBe(true)
  })
})
