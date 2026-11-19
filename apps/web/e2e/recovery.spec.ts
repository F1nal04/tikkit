import { expect, type Page, test } from '@playwright/test'
import { proxyApi, signIn } from './support'

const DESK = '30000000-0000-4000-8000-000000000101'
const json = (status: number, code: string) => ({
  status,
  contentType: 'application/json',
  body: JSON.stringify({ error: { code }, request_id: 'recovery' }),
})

function pathOf(url: string) {
  return new URL(url).pathname
}

function isSubmit(url: string, method: string) {
  const path = pathOf(url)
  return method === 'POST' && (path === '/api/tickets' || /\/api\/ticket-drafts\/[^/]+\/submit$/.test(path))
}

async function fillTicket(page: Page) {
  await page.goto('/tickets/new')
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Category').selectOption({ label: 'Hardware' })
  await page.getByLabel('Subject').fill('Printer jam')
  await page.getByLabel('Description').fill('Paper stops halfway.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'Low' })
}

test('a slow, failed, rate-limited, or offline ticket submit keeps the draft and can be retried', async ({ page }) => {
  await signIn(page, 'requester')
  await fillTicket(page)
  let posts = 0
  let mode: 'delay' | 'down' | 'limit' | 'offline' | 'pass' = 'delay'
  await page.route('**/api/**', async (route) => {
    if (mode === 'offline') return route.abort('internetdisconnected')
    if (!isSubmit(route.request().url(), route.request().method()) || mode === 'pass') return route.fallback()
    if (mode === 'delay') {
      posts += 1
      await new Promise((resolve) => setTimeout(resolve, 800))
      return route.fallback()
    }
    await route.fulfill(mode === 'down' ? json(503, 'unavailable') : json(429, 'rate_limited'))
  })
  const pending = page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.locator('form')).toHaveAttribute('aria-busy', 'true')
  await expect(page.getByLabel('Subject')).toHaveValue('Printer jam')
  await pending
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect(posts).toBe(1)

  mode = 'down'
  await fillTicket(page)
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.locator('#form-errors')).toBeFocused()
  await expect(page.locator('#form-errors')).toContainText('The service is unavailable.')
  await expect(page.getByLabel('Subject')).toHaveValue('Printer jam')
  mode = 'limit'
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.locator('#form-errors')).toContainText('Too many requests.')
  await expect(page.getByLabel('Description')).toHaveValue('Paper stops halfway.')
  mode = 'offline'
  await page.context().setOffline(true)
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.getByRole('alert').first()).toBeVisible()
  await expect(page.getByLabel('Subject')).toHaveValue('Printer jam')
  await page.context().setOffline(false)
  mode = 'pass'
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
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
  await again.getByLabel('Password').fill('correct horse battery')
  await again.getByRole('button', { name: 'Sign in' }).click()
  await expect(again).toHaveURL('/tickets/new')
  await again.close()

  await expect(page.getByText(/Draft saved at/)).toBeVisible({ timeout: 15_000 })
  await expect(page.getByLabel('Subject')).toHaveValue('Unsent after expiry')
  await expect(banner).toBeHidden()
})

test('a lost reply, note, or proposal stays on the page and retries once', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto(`/queue/${DESK}`)
  await page.getByRole('button', { name: 'Claim ticket' }).click()
  await page.getByRole('button', { name: 'Start work' }).click()
  await expect(page.locator('.badge')).toHaveText('In progress')

  let replies = 0
  await page.route('**/api/tickets/*/replies', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    replies += 1
    if (replies === 1) return route.fulfill(json(503, 'unavailable'))
    await route.fallback()
  })
  await page.getByLabel('Message').fill('The tray was empty.')
  await page.getByRole('button', { name: 'Send public reply' }).click()
  await expect(page.locator('#form-errors')).toContainText('The service is unavailable.')
  await expect(page.getByLabel('Message')).toHaveValue('The tray was empty.')
  await page.getByRole('button', { name: 'Send public reply' }).click()
  await expect(page.getByText('Reply sent.')).toBeVisible()
  expect(replies).toBe(2)

  let notes = 0
  await page.route('**/api/tickets/*/notes', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    notes += 1
    if (notes === 1) return route.fulfill(json(503, 'unavailable'))
    await route.fallback()
  })
  await page.getByLabel('Internal note', { exact: true }).check()
  await page.getByLabel('Message').fill('Order a replacement roller.')
  await page.getByRole('button', { name: 'Add internal note' }).click()
  await expect(page.locator('#form-errors')).toContainText('The service is unavailable.')
  await expect(page.getByLabel('Message')).toHaveValue('Order a replacement roller.')
  await page.getByRole('button', { name: 'Add internal note' }).click()
  await expect(page.getByText('Internal note added.')).toBeVisible()
  expect(notes).toBe(2)

  let proposals = 0
  await page.route('**/api/tickets/*/staff-actions', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    proposals += 1
    if (proposals === 1) return route.fulfill(json(500, 'unavailable'))
    await route.fallback()
  })
  await page.getByLabel(/Resolution summary/).fill('Replaced the roller.')
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(page.locator('#form-errors')).toContainText('The service is unavailable.')
  await expect(page.getByLabel(/Resolution summary/)).toHaveValue('Replaced the roller.')
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(page.getByText('Awaiting confirmation', { exact: true }).first()).toBeVisible()
  expect(proposals).toBe(2)
})

test('an administrator settings save survives a 500 and an expired session without losing input', async ({ page, context }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/settings')
  const name = page.getByLabel('Organization name')
  await name.fill('Kept across the error')
  await page.route('**/api/admin/settings', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback()
    await route.fulfill(json(503, 'unavailable'))
  })
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.locator('#form-errors')).toBeFocused()
  await expect(page.locator('#form-errors')).toContainText('The service is unavailable.')
  await expect(name).toHaveValue('Kept across the error')
  await page.unroute('**/api/admin/settings')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Saved.' })).toBeVisible()

  await name.fill('Kept across sign-in')
  await page.evaluate(() => fetch('/api/test/expire', { method: 'POST' }))
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  const banner = page.getByRole('alert').filter({ has: page.getByRole('link', { name: /Sign in/ }) })
  await expect(banner.getByRole('link')).toHaveAttribute('href', '/login?next=%2Fadmin%2Fsettings')
  await expect(name).toHaveValue('Kept across sign-in')
  const again = await context.newPage()
  await proxyApi(again)
  await again.goto('/login?next=%2Fadmin%2Fsettings')
  await again.getByLabel('Username').fill('admin')
  await again.getByLabel('Password').fill('correct horse battery')
  await again.getByRole('button', { name: 'Sign in' }).click()
  await expect(again).toHaveURL('/admin/settings')
  await again.close()
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Saved.' })).toBeVisible()
  await expect(name).toHaveValue('Kept across sign-in')
})

test('an AI decision is not sent until a click, and a failed click does not send another', async ({ page }) => {
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
  await page.evaluate((id) => fetch('/api/test/ai-result', {
    method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
    body: JSON.stringify({ id, outcome: 'suggestion' }),
  }), id)
  let decisions = 0
  await page.route('**/api/tickets/*/actions', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as { action?: string }
    if (body.action !== 'ai_solved' && body.action !== 'ai_not_solved') return route.fallback()
    decisions += 1
    if (decisions === 1) return route.fulfill(json(503, 'unavailable'))
    await route.fallback()
  })
  await page.reload()
  await expect(page.getByRole('button', { name: 'Solved', exact: true })).toBeVisible()
  await page.waitForTimeout(600)
  expect(decisions).toBe(0)
  await page.getByRole('button', { name: 'Solved', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('could not confirm that your decision was saved')
  await expect(page.getByRole('button', { name: 'Solved', exact: true })).toBeEnabled()
  await page.waitForTimeout(600)
  expect(decisions).toBe(1)
  await page.getByRole('button', { name: 'Solved', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Solved with the AI suggestion' })).toBeVisible()
  expect(decisions).toBe(2)
})
