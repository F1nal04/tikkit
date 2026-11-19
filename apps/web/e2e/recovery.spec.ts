import { expect, type BrowserContext, type Page, test } from '@playwright/test'
import { PASSWORD, proxyApi, signIn } from './support'

const DESK = '30000000-0000-4000-8000-000000000101'
const UNAVAILABLE = 'The service is unavailable. Please try again later.'
const LIMITED = 'Too many requests. Please wait a minute and try again.'
const EXPIRED = 'Your session has expired. Please sign in again.'

const json = (status: number, code: string) => ({
  status,
  contentType: 'application/json',
  body: JSON.stringify({ error: { code, message: code }, request_id: 'recovery' }),
})

type Mode = 'pass' | 'slow' | '503' | '429' | '401' | 'offline'
type Hit = { key?: string; action?: string }

function pathOf(url: string) {
  return new URL(url).pathname
}

async function relogin(page: Page, context: BrowserContext, username: string, next: string) {
  await page.evaluate(() => fetch('/api/test/expire', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
    body: '{}',
  }))
  const again = await context.newPage()
  await proxyApi(again)
  await again.context().clearCookies({ name: 'lang' })
  await again.goto(`/login?next=${encodeURIComponent(next)}`)
  await again.getByLabel('Username').fill(username)
  await again.getByLabel('Password').fill(PASSWORD)
  await again.getByRole('button', { name: 'Sign in' }).click()
  await expect(again).toHaveURL(new RegExp(`${next.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`))
  await again.close()
}

/**
 * One route for the whole test. `match` selects the endpoint. A failure keeps the typed
 * input and shows that failure's text; the retry of the same input reuses the idempotency
 * key when the form sends one, and produces the success text once.
 */
function arm(page: Page, match: (url: string, method: string, body: Record<string, unknown> | null) => boolean) {
  const hits: Hit[] = []
  let mode: Mode = 'pass'
  const ready = page.route('**/api/**', async (route) => {
    if (mode === 'offline') return route.abort('internetdisconnected')
    const request = route.request()
    let body: Record<string, unknown> | null = null
    try {
      const raw = request.postData()
      body = raw ? JSON.parse(raw) as Record<string, unknown> : null
    } catch { body = null }
    if (!match(request.url(), request.method(), body)) return route.fallback()
    hits.push({
      key: typeof body?.idempotency_key === 'string' ? body.idempotency_key : undefined,
      action: typeof body?.action === 'string' ? body.action : undefined,
    })
    if (mode === 'pass') return route.fallback()
    if (mode === 'slow') {
      await new Promise((resolve) => setTimeout(resolve, 800))
      return route.fallback()
    }
    const status = mode === '503' ? 503 : mode === '429' ? 429 : 401
    const code = mode === '503' ? 'unavailable' : mode === '429' ? 'rate_limited' : 'unauthorized'
    return route.fulfill(json(status, code))
  })
  return {
    ready,
    hits,
    async set(next: Mode) {
      mode = next
      if (next === 'offline') await page.context().setOffline(true)
      else await page.context().setOffline(false)
    },
  }
}

async function expectFailure(page: Page, mode: Mode, value: ReturnType<Page['getByLabel']>, kept: string) {
  const summary = page.locator('#form-errors')
  await expect(summary).toContainText(mode === '429' ? LIMITED : mode === '401' ? EXPIRED : UNAVAILABLE)
  if (mode !== '429') await expect(summary).not.toContainText('Too many requests')
  await expect(value).toHaveValue(kept)
}

test('ticket submit keeps its input across slow, 5xx, 429, 401, and offline, then retries once', async ({ page, context }) => {
  test.setTimeout(120_000)
  await signIn(page, 'requester')
  const gate = arm(page, (url, method) => {
    const path = pathOf(url)
    return method === 'POST' && (path === '/api/tickets' || /\/api\/ticket-drafts\/[^/]+\/submit$/.test(path))
  })
  await gate.ready
  const fill = async () => {
    await page.goto('/tickets/new')
    await page.waitForLoadState('networkidle')
    await page.getByLabel('Category').selectOption({ label: 'Hardware' })
    await page.getByLabel('Subject').fill('Printer jam')
    await page.getByLabel('Description').fill('Paper stops halfway.')
    await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'Low' })
  }
  const subject = () => page.getByLabel('Subject')
  const submit = () => page.getByRole('button', { name: 'Submit request' })

  await fill()
  await gate.set('slow')
  const pending = submit().click()
  await expect(page.locator('form')).toHaveAttribute('aria-busy', 'true')
  await expect(subject()).toHaveValue('Printer jam')
  await pending
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect(gate.hits).toHaveLength(1)

  for (const mode of ['503', '429', '401', 'offline'] as const) {
    await fill()
    gate.hits.length = 0
    await gate.set(mode)
    await submit().click()
    await expectFailure(page, mode, subject(), 'Printer jam')
    await expect(page.getByLabel('Description')).toHaveValue('Paper stops halfway.')
    if (mode === '401') {
      await gate.set('pass')
      const before = gate.hits.length
      await page.waitForTimeout(400)
      expect(gate.hits.length).toBe(before)
    }
    const keep = page.getByRole('button', { name: 'Keep mine as a new draft' })
    if (await keep.count()) {
      await keep.click()
      await expect(keep).toBeHidden()
    }
    await gate.set('pass')
    await submit().click()
    await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
    const keys = gate.hits.map((hit) => hit.key).filter(Boolean)
    expect(keys.length).toBeGreaterThan(0)
    expect(new Set(keys).size).toBe(1)
  }
})

test('an expired session keeps the ticket draft and a new sign-in lets saving continue', async ({ page, context }) => {
  await signIn(page, 'requester')
  await page.goto('/tickets/new')
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Subject').fill('Unsent after expiry')
  await page.evaluate(() => fetch('/api/test/expire', { method: 'POST' }))
  await expect(page.getByRole('status').filter({ hasText: 'could not be saved' })).toBeVisible()
  const banner = page.getByRole('alert').filter({ has: page.getByRole('link', { name: /Sign in/ }) })
  await expect(banner).toBeVisible()
  await expect(banner.getByRole('link')).toHaveAttribute('href', '/login?next=%2Ftickets%2Fnew')
  await expect(page.getByLabel('Subject')).toHaveValue('Unsent after expiry')

  const again = await context.newPage()
  await proxyApi(again)
  await again.goto('/login?next=%2Ftickets%2Fnew')
  await again.getByLabel('Username').fill('requester')
  await again.getByLabel('Password').fill(PASSWORD)
  await again.getByRole('button', { name: 'Sign in' }).click()
  await expect(again).toHaveURL('/tickets/new')
  await again.close()

  await expect(page.getByText(/Draft saved at/)).toBeVisible({ timeout: 15_000 })
  await expect(page.getByLabel('Subject')).toHaveValue('Unsent after expiry')
  await expect(banner).toBeHidden()
})

test('staff reply, note, and proposal keep their text across 5xx, 429, 401, and offline', async ({ page, context }) => {
  test.setTimeout(180_000)
  await signIn(page, 'operator')
  await page.goto(`/queue/${DESK}`)
  await page.getByRole('button', { name: 'Claim ticket' }).click()
  await page.getByRole('button', { name: 'Start work' }).click()
  await expect(page.locator('.badge')).toHaveText('In progress')
  const gate = arm(page, (url, method) => {
    const path = pathOf(url)
    return method === 'POST' && /\/(replies|notes|staff-actions)$/.test(path)
  })
  await gate.ready

  const messages = [
    { button: 'Send public reply', success: 'Public reply sent.', note: false },
    { button: 'Add internal note', success: 'Internal note added.', note: true },
  ]
  for (const item of messages) {
    for (const mode of ['503', '429', '401', 'offline'] as const) {
      const text = `${item.button} ${mode}`
      const field = page.getByLabel('Message')
      if (item.note) await page.getByLabel('Internal note', { exact: true }).check()
      else await page.getByLabel('Public reply', { exact: true }).check()
      await field.fill(text)
      gate.hits.length = 0
      await gate.set(mode)
      await page.getByRole('button', { name: item.button }).click()
      await expectFailure(page, mode, field, text)
      if (mode === '401') {
        await gate.set('pass')
        const before = gate.hits.length
        await page.waitForTimeout(400)
        expect(gate.hits.length).toBe(before)
      }
      await gate.set('pass')
      await page.getByRole('button', { name: item.button }).click()
      await expect(page.getByText(item.success)).toBeVisible()
      await expect(page.getByText(text)).toHaveCount(1)
      const keys = gate.hits.map((hit) => hit.key).filter(Boolean)
      expect(gate.hits.length, JSON.stringify(gate.hits)).toBeGreaterThan(0)
      expect(new Set(keys).size, JSON.stringify(gate.hits)).toBe(1)
    }
  }

  const summary = page.getByLabel(/Resolution summary/)
  await summary.fill('Replaced the roller.')
  for (const mode of ['503', '429', '401', 'offline'] as const) {
    await gate.set(mode)
    await page.getByRole('button', { name: 'Propose resolution' }).click()
    await expectFailure(page, mode, summary, 'Replaced the roller.')
    if (mode === '401') {
      await gate.set('pass')
      const before = gate.hits.length
      await page.waitForTimeout(400)
      expect(gate.hits.length).toBe(before)
    }
  }
  gate.hits.length = 0
  await gate.set('pass')
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(page.getByText('Awaiting confirmation', { exact: true }).first()).toBeVisible()
})

test('requester reply, accept, and reject keep their input across 5xx, 401, and offline', async ({ page, context }) => {
  test.setTimeout(180_000)
  await signIn(page, 'requester')
  const history = (await page.evaluate(async () => {
    const response = await fetch('/api/test/history', { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: '{}' })
    return response.json()
  })) as { queued: string; pending: string }
  const gate = arm(page, (url, method, body) => {
    if (method !== 'POST') return false
    const path = pathOf(url)
    if (/\/replies$/.test(path)) return true
    return /\/actions$/.test(path) && (body?.action === 'accept_resolution' || body?.action === 'reject_resolution')
  })
  await gate.ready

  await page.goto(`/tickets/${history.queued}`)
  const message = page.getByLabel('Message')
  for (const mode of ['503', '401', 'offline'] as const) {
    const text = `Still jammed ${mode}`
    await message.fill(text)
    gate.hits.length = 0
    await gate.set(mode)
    await page.getByRole('button', { name: 'Send reply' }).click()
    await expectFailure(page, mode, message, text)
    if (mode === '401') {
      await gate.set('pass')
      const before = gate.hits.length
      await page.waitForTimeout(400)
      expect(gate.hits.length).toBe(before)
    }
    await gate.set('pass')
    await page.getByRole('button', { name: 'Send reply' }).click()
    await expect(page.getByText('Reply sent.')).toHaveCount(1)
    const keys = gate.hits.map((hit) => hit.key).filter(Boolean)
    expect(new Set(keys).size).toBe(1)
  }

  const decisions = [
    { name: 'Confirm resolved', status: 'closed', id: history.pending },
    { name: 'Still not solved', status: 'in_progress', id: '' },
  ] as const
  for (const decision of decisions) {
    const id = decision.id || ((await page.evaluate(async () => {
      const response = await fetch('/api/test/history', { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: '{}' })
      return response.json()
    })) as { pending: string }).pending
    await page.goto(`/tickets/${id}`)
    for (const mode of ['503', '401', 'offline'] as const) {
      await gate.set(mode)
      await page.getByRole('button', { name: decision.name }).click()
      await expect(page.locator('#form-errors')).toContainText(mode === '401' ? EXPIRED : UNAVAILABLE)
      if (mode === '401') {
        await gate.set('pass')
        const before = gate.hits.length
        await page.waitForTimeout(400)
        expect(gate.hits.length).toBe(before)
      }
    }
    await gate.set('pass')
    await page.getByRole('button', { name: decision.name }).click()
    await expect(page.locator(`.badge[data-status="${decision.status}"]`)).toBeVisible()
  }
})

test('an AI decision is not sent again after re-login until the button is pressed', async ({ page, context }) => {
  test.setTimeout(120_000)
  await signIn(page, 'requester')
  await page.evaluate(() => fetch('/api/test/ai', {
    method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
    body: JSON.stringify({ ai_assistance: true }),
  }))
  await page.goto('/tickets/new')
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}/)
  const id = new URL(page.url()).pathname.split('/').pop()!
  await page.evaluate((ticket) => fetch('/api/test/ai-result', {
    method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
    body: JSON.stringify({ id: ticket, outcome: 'suggestion' }),
  }), id)
  const gate = arm(page, (url, method, body) => method === 'POST' && /\/actions$/.test(pathOf(url)) && (body?.action === 'ai_solved' || body?.action === 'ai_not_solved'))
  await gate.ready
  await page.reload()
  const solved = page.getByRole('button', { name: 'Solved', exact: true })
  await expect(solved).toBeVisible()
  await page.waitForTimeout(600)
  expect(gate.hits).toHaveLength(0)

  for (const mode of ['503', '429', '401', 'offline'] as const) {
    const before = gate.hits.length
    await gate.set(mode)
    await solved.click()
    const decisionAlert = page.getByRole('alert').filter({ hasText: 'could not confirm' })
    await expect(decisionAlert).toContainText(mode === '429' ? 'Too many requests' : mode === '401' ? 'session has expired' : 'The service is unavailable')
    await page.waitForTimeout(500)
    const sent = gate.hits.length
    await page.waitForTimeout(400)
    expect(gate.hits.length).toBe(sent)
    void before
    await gate.set('pass')
    if (mode === '401') {
      await page.waitForTimeout(400)
      expect(gate.hits.length).toBe(sent)
    }
  }
  const beforeSuccess = gate.hits.length
  await solved.click()
  await expect(page.getByRole('heading', { name: 'Solved with the AI suggestion' })).toBeVisible()
  expect(gate.hits.length).toBe(beforeSuccess + 1)
})

test('organization settings and outbound email keep typed values across 5xx and 401', async ({ page, context }) => {
  test.setTimeout(120_000)
  await signIn(page, 'admin')
  const gate = arm(page, (url, method) => {
    const path = pathOf(url)
    return (method === 'PUT' && path === '/api/admin/settings') || (method === 'PUT' && path === '/api/settings/smtp')
  })
  await gate.ready

  await page.goto('/admin/settings')
  const name = page.getByLabel('Organization name')
  for (const mode of ['503', '401'] as const) {
    await name.fill('Kept across the error')
    await gate.set(mode)
    await page.getByRole('button', { name: 'Save', exact: true }).click()
    await expectFailure(page, mode, name, 'Kept across the error')
    if (mode === '401') {
      await gate.set('pass')
      await relogin(page, context, 'admin', '/admin/settings')
    }
    await gate.set('pass')
    await page.getByRole('button', { name: 'Save', exact: true }).click()
    await expect(page.getByRole('status').filter({ hasText: 'Saved.' })).toBeVisible()
    await expect(name).toHaveValue('Kept across the error')
  }

  let allowSmtp = false
  await page.route('**/api/settings/smtp', (route) => {
    if (!allowSmtp || route.request().method() !== 'PUT') return route.fallback()
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ connection: { host: 'smtp.example.com', port: 587, security: 'starttls', sender_name: 'Desk', sender_address: 'desk@example.com', enabled: true, last_test_at: null, last_test_error: null } }),
    })
  })
  await page.goto('/admin/email')
  const host = page.getByLabel('Host')
  await host.fill('smtp.example.com')
  await page.getByLabel('Sender address').fill('desk@example.com')
  await gate.set('503')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.locator('#form-errors')).toContainText(UNAVAILABLE)
  await expect(host).toHaveValue('smtp.example.com')
  allowSmtp = true
  await gate.set('pass')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Saved.' })).toBeVisible()
  await expect(host).toHaveValue('smtp.example.com')
})
