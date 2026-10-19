import { devices, expect, type Page, test } from '@playwright/test'
import { signIn } from './support'

const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64')
async function request(page: Page, path: string, body?: unknown) {
  return page.evaluate(async ({ path, body }) => {
    const r = await fetch(path, body === undefined ? {} : { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify(body) })
    const text = await r.text()
    let data
    try { data = JSON.parse(text) } catch { data = null }
    return { status: r.status, body: data }
  }, { path, body })
}
async function setup(page: Page, user = 'requester') {
  await signIn(page, user)
  return (await request(page, '/api/test/history', {})).body as Record<string, string>
}
async function detail(page: Page, id: string, language = '') {
  await page.goto(`/tickets/${id}${language}`)
  await page.waitForLoadState('networkidle')
}

test('own history pages and status/date filters stay bounded and include AI-solved records', async ({ page }) => {
  await setup(page)
  await page.goto('/')
  const table = page.getByRole('table', { name: 'My tickets' })
  await expect(table.getByRole('row')).toHaveCount(26)
  await expect(page.getByRole('link', { name: 'requester: AI solved final' })).toBeVisible()
  await page.getByRole('link', { name: 'Next page' }).click()
  await expect(page).toHaveURL(/cursor=/)
  await expect(table.getByRole('row')).toHaveCount(8)
  await page.getByRole('link', { name: 'First page' }).click()
  await page.getByLabel('Status', { exact: true }).selectOption('ai_solved')
  await page.getByLabel('Created from (UTC)').fill('2026-10-01')
  await page.getByLabel('Created through (UTC)').fill('2026-10-01')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(table.getByRole('row')).toHaveCount(2)
  await page.getByLabel('Created from (UTC)').fill('2027-01-01')
  await page.getByLabel('Created through (UTC)').fill('2027-01-02')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page.getByText('No tickets match.')).toBeVisible()
})

test('two requesters cannot read each other’s detail, replies, or screenshots', async ({ page, browser }) => {
  const a = await setup(page)
  const context = await browser.newContext()
  try {
    const second = await context.newPage()
    const b = await setup(second, 'requester2')
    await second.goto('/')
    await expect(second.getByRole('link', { name: 'requester2: AI solved final' })).toBeVisible()
    await expect(second.getByRole('link', { name: 'requester: AI solved final', exact: true })).toHaveCount(0)
    for (const path of [`/api/tickets/${a.queued}`, `/api/tickets/${a.queued}/messages`, `/api/screenshots/${a.screenshot}`, `/api/screenshots/${a.screenshot}/preview`, `/api/screenshots/${a.screenshot}/download`]) {
      expect((await request(second, path)).status).toBe(404)
    }
    expect((await request(page, `/api/tickets/${b.queued}`)).status).toBe(404)
    expect((await request(second, `/api/tickets/${a.queued}/replies`, { body: 'Intrusion', idempotency_key: crypto.randomUUID() })).status).toBe(404)
    await detail(second, a.queued)
    await expect(second.getByRole('main')).not.toContainText('Original problem')
    await expect(second.getByRole('main')).toContainText('not available')
  } finally { await context.close() }
})

test('frozen intake and public conversation escape text, page replies, and hide notes and note images', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.queued)
  await expect(page.getByText('General queue', { exact: true })).toBeVisible()
  await expect(page.getByText('Unassigned', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Original submission' })).toBeVisible()
  await expect(page.getByText('Original problem <script>window.intakeXss = true</script>')).toBeVisible()
  await expect(page.getByRole('main')).not.toContainText('PRIVATE NOTE MUST NEVER LEAK')
  expect(JSON.stringify((await request(page, `/api/tickets/${ids.queued}/messages`)).body)).not.toContain('PRIVATE NOTE')
  for (const suffix of ['', '/preview', '/download']) expect((await request(page, `/api/screenshots/${ids.privateShot}${suffix}`)).status).toBe(404)
  expect((await request(page, `/api/screenshots/${ids.screenshot}/download`)).status).toBe(200)
  await expect(page.getByRole('article')).toHaveCount(25)
  await page.getByRole('button', { name: 'Load more replies' }).click()
  await expect(page.getByRole('article')).toHaveCount(27)
  await expect(page.getByRole('button', { name: 'Load more replies' })).toHaveCount(0)
  expect(await page.evaluate('window.intakeXss || window.messageXss || false')).toBe(false)
  await expect(page.getByRole('button', { name: /Edit|Delete/ })).toHaveCount(0)
})

test('append-only reply uploads screenshots and displays the committed message without duplicates', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.pending)
  await expect(page.getByText('Desk', { exact: true })).toBeVisible()
  await expect(page.getByText('Olga Support', { exact: true })).toBeVisible()
  await page.getByLabel('Message', { exact: true }).fill('Still broken. <img src=x onerror="window.replyXss=true">')
  await page.getByLabel('Add screenshots').setInputFiles({ name: 'reply.png', mimeType: 'image/png', buffer: PNG })
  await expect(page.getByRole('img', { name: 'reply.png' })).toBeVisible()
  await page.getByRole('button', { name: 'Send reply' }).dblclick()
  await expect(page.getByText('Reply sent.', { exact: true })).toBeVisible()
  await expect(page.getByRole('article')).toHaveCount(1)
  await expect(page.getByRole('article')).toContainText('Still broken. <img src=x onerror="window.replyXss=true">')
  await expect(page.getByRole('article').getByRole('img', { name: 'Screenshot 1' })).toBeVisible()
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('')
  // A public reply does not confirm or reset the pending proposal.
  await expect(page.getByRole('button', { name: 'Confirm resolved' })).toBeVisible()
  expect(await page.evaluate('window.replyXss || false')).toBe(false)
})

test('resolution rejection keeps the support workflow; confirmation records explicit human closure', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.pending)
  await expect(page.getByText(/Automatic closure deadline:/)).toBeVisible()
  await expect(page.getByText('Installed a new driver. <img src=x onerror=alert(1)>').first()).toBeVisible()
  await page.getByRole('button', { name: 'Still not solved' }).click()
  await expect(page.locator('.badge[data-status="in_progress"]')).toBeVisible()
  await expect(page.getByText('Desk', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toHaveCount(0)
  const next = (await request(page, '/api/test/history', {})).body
  await detail(page, next.pending)
  await page.getByRole('button', { name: 'Confirm resolved' }).click()
  await expect(page.locator('.badge[data-status="closed"]')).toBeVisible()
  await expect(page.getByText('Confirmed by the requester')).toBeVisible()
  await expect(page.getByRole('form', { name: 'Public reply' })).toHaveCount(0)
  await detail(page, next.timeout)
  await expect(page.getByText('Closed automatically: no response before the confirmation deadline')).toBeVisible()
  await expect(page.getByText('Confirmed by the requester')).toHaveCount(0)
})

test('stale proposal decisions show conflict and preserve the unsent public reply', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.pending)
  await page.getByLabel('Message', { exact: true }).fill('Please keep this context.')
  expect((await request(page, `/api/tickets/${ids.pending}/actions`, { action: 'reject_resolution', proposal_version: 2 })).status).toBe(200)
  await page.getByRole('button', { name: 'Confirm resolved' }).click()
  await expect(page.getByRole('alert')).toContainText('The ticket or organization policy changed.')
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Please keep this context.')
  await expect(page.locator('.badge[data-status="in_progress"]')).toBeVisible()
  await expect(page.getByText('Ticket updated.', { exact: true })).toHaveCount(0)
})

test('cancellation requires a reason, retains it after policy changes, and records successful cancellation', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.queued)
  await page.getByRole('button', { name: 'Cancel ticket', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm cancellation' }).click()
  await expect(page.getByLabel('Cancellation reason')).toHaveAttribute('aria-invalid', 'true')
  await page.getByLabel('Cancellation reason').fill('No longer needed.')
  await request(page, '/api/test/policy', { requester_cancellation_policy: 'disabled' })
  await page.getByRole('button', { name: 'Confirm cancellation' }).click()
  await expect(page.getByRole('alert')).toContainText('The ticket or organization policy changed.')
  await expect(page.getByLabel('Cancellation reason')).toHaveValue('No longer needed.')
  await expect(page.getByRole('button', { name: 'Confirm cancellation' })).toBeDisabled()
  await expect(page.locator('.badge[data-status="queued"]')).toBeVisible()
  await request(page, '/api/test/policy', { requester_cancellation_policy: 'before_work_starts' })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm cancellation' }).click()
  await expect(page.locator('.badge[data-status="cancelled"]')).toBeVisible()
  await expect(page.getByText('Cancellation reason: No longer needed.')).toBeVisible()
  await detail(page, ids.pending)
  await expect(page.getByRole('button', { name: 'Cancel ticket', exact: true })).toHaveCount(0)
})

test('reopening defaults off, honors current policy changes, then reopens only a human ticket', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.closed)
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toHaveCount(0)
  expect((await request(page, `/api/tickets/${ids.closed}/actions`, { action: 'reopen' })).status).toBe(403)
  await request(page, '/api/test/policy', { allow_reopening: true })
  await page.reload()
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toBeVisible()
  await request(page, '/api/test/policy', { allow_reopening: false })
  await page.getByRole('button', { name: 'Reopen ticket' }).click()
  await expect(page.getByRole('alert')).toContainText('The ticket or organization policy changed.')
  await expect(page.locator('.badge[data-status="closed"]')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toHaveCount(0)
  await request(page, '/api/test/policy', { allow_reopening: true })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await page.getByRole('button', { name: 'Reopen ticket' }).click()
  await expect(page.locator('.badge[data-status="in_progress"]')).toBeVisible()
  await expect(page.getByRole('form', { name: 'Public reply' })).toBeVisible()
})

test('AI-solved history is read-only, marks the suggestion unavailable, and rejects direct reopen/escalation', async ({ page }) => {
  const ids = await setup(page)
  await request(page, '/api/test/policy', { allow_reopening: true })
  await detail(page, ids.ai)
  await expect(page.getByText('The AI suggestion text is unavailable for this record.')).toBeVisible()
  await expect(page.getByText('AI solution confirmed', { exact: true })).toBeVisible()
  await expect(page.getByRole('main').getByRole('textbox')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /Send reply|Reopen|Cancel ticket|Still not solved|Confirm resolved/ })).toHaveCount(0)
  const before = await request(page, `/api/tickets/${ids.ai}`)
  for (const action of ['reopen', 'cancel', 'reject_resolution', 'ai_not_solved']) {
    expect((await request(page, `/api/tickets/${ids.ai}/actions`, { action, reason: 'Escalate', proposal_version: 1 })).status).toBeGreaterThanOrEqual(400)
  }
  expect((await request(page, `/api/tickets/${ids.ai}/replies`, { body: 'Escalate', idempotency_key: crypto.randomUUID() })).status).toBe(409)
  expect((await request(page, `/api/tickets/${ids.ai}`)).body).toEqual(before.body)
})

test('requester history and detail offer accessible unavailable states and recover', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.pending)
  let failing = true
  await page.route((url) => url.pathname === '/api/tickets', (route) => failing ? route.abort() : route.fallback())
  await page.getByRole('link', { name: 'Back to my tickets' }).click()
  await expect(page.getByRole('alert')).toContainText('The service is unavailable.')
  failing = false
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await expect(page.getByRole('table')).toBeVisible()
  await page.route((url) => url.pathname === `/api/tickets/${ids.queued}`, (route) => failing ? route.abort() : route.fallback())
  failing = true
  await page.getByRole('link', { name: 'requester: Waiting in general queue' }).click()
  await expect(page.getByRole('alert')).toContainText('The service is unavailable.')
  failing = false
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await expect(page.getByRole('heading', { name: /Waiting in general queue/ })).toBeVisible()
})

test('refresh shows reassignment and transfer without losing an unsent reply', async ({ page }) => {
  const ids = await setup(page)
  await detail(page, ids.pending)
  await page.getByLabel('Message', { exact: true }).fill('Please keep my context.')
  await request(page, '/api/test/assignment', { id: ids.pending, assignment: { team_name: 'Network', assignee_display_name: 'Nora Support' } })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await expect(page.getByText('Network', { exact: true })).toBeVisible()
  await expect(page.getByText('Nora Support', { exact: true })).toBeVisible()
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Please keep my context.')
})

test.describe('requester on a phone', () => {
  const { defaultBrowserType: _ignored, ...phone } = devices['Pixel 7']
  test.use(phone)
  test('German detail fits mobile and keeps unsent input when language changes', async ({ page }) => {
    const ids = await setup(page)
    await detail(page, ids.pending, '?lang=de')
    await expect(page.getByRole('heading', { name: 'Ursprüngliche Anfrage' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Lösung bestätigen' })).toBeVisible()
    await page.getByLabel('Nachricht', { exact: true }).fill('Noch nicht gelöst.')
    await page.getByRole('button', { name: 'Menü', exact: true }).click()
    await page.getByLabel('Sprache', { exact: true }).selectOption('en')
    await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Noch nicht gelöst.')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
})
