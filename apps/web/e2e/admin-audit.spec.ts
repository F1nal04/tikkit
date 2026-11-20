import { readFile } from 'node:fs/promises'
import { expect, test, type Page } from '@playwright/test'
import { signIn } from './support'

/** Method and path of every API request the browser sends while `run` executes. */
async function apiRequests(page: Page, run: () => Promise<void>) {
  const seen: string[] = []
  const listener = (request: { url(): string; method(): string }) => {
    if (new URL(request.url()).pathname.startsWith('/api/')) seen.push(`${request.method()} ${new URL(request.url()).pathname}`)
  }
  page.on('request', listener)
  await run()
  page.off('request', listener)
  return seen
}

test('an Admin reads the audit log, follows a ticket and a request, and pages through it', async ({ page }) => {
  await signIn(page, 'admin')
  const rows = page.locator('tbody tr')
  const requests = await apiRequests(page, async () => {
    await page.getByRole('link', { name: 'Audit log' }).click()
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Audit log')
    await expect(page.getByRole('link', { name: 'Audit log' })).toHaveAttribute('aria-current', 'page')
    // Newest first, in pages.
    await expect(rows).toHaveCount(4)
    await expect(rows.first()).toContainText('organization_settings_updated')
    await page.getByRole('link', { name: 'Next page' }).click()
    await expect(rows).toHaveCount(2)
    await expect(rows.last()).toContainText('login_failed')
    await expect(rows.last().getByTestId('service')).toHaveText('Not signed in')
    await page.getByRole('link', { name: 'First page' }).click()
    await expect(rows).toHaveCount(4)
  })
  // Reading the log only reads.
  expect(requests.every((r) => r.startsWith('GET '))).toBe(true)

  // Who acted: the account, its role and team at the time; or a named service.
  const denied = page.locator('tr[data-action="ai_solved_change_denied"]')
  await expect(denied).toContainText('operator (Operator, Desk)')
  await expect(denied).toContainText('Ticket #301')
  await expect(page.locator('tr[data-action="ticket_ai_suggestion_ready"]').getByTestId('service')).toHaveText('System: AI suggestions')

  // Metadata is untrusted text: shown literally, never as markup.
  const solved = page.locator('tr[data-action="ticket_ai_solved"]')
  await solved.getByText('Show').click()
  await expect(solved.locator('pre')).toContainText('<img src=x onerror=\\"window.auditXss = true\\">')
  await expect(solved.locator('pre')).toContainText('"closure_reason": "requester_confirmed"')
  await expect(page.locator('tbody img, tbody script')).toHaveCount(0)
  expect(await page.evaluate(() => (window as unknown as { auditXss?: boolean }).auditXss)).toBeUndefined()

  // The ticket's timeline: every decision on it, including the refused attempt.
  await denied.getByRole('link', { name: 'Ticket #301' }).click()
  await expect(page).toHaveURL(/ticket_reference=301/)
  await expect(page.getByLabel('Ticket number')).toHaveValue('301')
  await expect(rows).toHaveCount(4)
  await expect(rows.locator('th code')).toHaveText(['ai_solved_change_denied', 'ticket_ai_solved', 'ticket_ai_suggestion_ready', 'ticket_submit'])
  await expect(rows.first().getByRole('link', { name: 'Open submission' })).toHaveAttribute('href', '/admin/submissions/60000000-0000-4000-8000-000000000301')

  // Filters are a plain GET form kept in the address bar.
  await page.getByLabel('Action').selectOption('ticket_ai_solved')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page).toHaveURL(/action=ticket_ai_solved/)
  await expect(rows).toHaveCount(1)
  // Everything one request did.
  await rows.first().getByRole('link', { name: '70000000-0000-4000-8000-000000000004' }).click()
  await expect(page).toHaveURL(/request_id=70000000-0000-4000-8000-000000000004/)
  await expect(page.getByLabel('Request ID')).toHaveValue('70000000-0000-4000-8000-000000000004')
  await expect(rows).toHaveCount(1)
  await page.getByRole('link', { name: 'Clear filters' }).click()
  await page.getByLabel('Actor').fill('requester')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(rows).toHaveCount(2)
  await page.getByLabel('Actor').fill('nobody')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page.getByText('No audit entries match these filters.')).toBeVisible()
  // There is nothing to edit or delete.
  await expect(page.getByRole('main').getByRole('button')).toHaveText(['Apply filters', 'Export as CSV'])
})

test('the export downloads the filtered log as neutralized CSV and is itself recorded', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/audit?action=organization_settings_updated')
  await expect(page.locator('tbody tr')).toHaveCount(1)
  const download = page.waitForEvent('download')
  const requests = await apiRequests(page, async () => {
    await page.getByRole('button', { name: 'Export as CSV' }).click()
    await expect(page.getByRole('status')).toHaveText('Exported 1 entries.')
  })
  expect(requests).toEqual(['POST /api/admin/audit/export'])
  const file = await download
  expect(file.suggestedFilename()).toBe('tikkit-audit-2026-11-18.csv')
  const csv = await readFile(await file.path(), 'utf8')
  const lines = csv.split('\r\n')
  expect(lines[0]).toBe('"id","time_utc","action","actor_username","resource_id"')
  // A value a spreadsheet would run as a formula is text in the file.
  expect(lines[1]).toBe('"6","2026-11-02T09:06:00Z","organization_settings_updated","admin","\'=HYPERLINK(""http://evil.example"")"')
  expect(lines).toHaveLength(3)

  await page.goto('/admin/audit?action=audit_exported')
  const exported = page.locator('tr[data-action="audit_exported"]')
  await expect(exported).toHaveCount(1)
  await expect(exported).toContainText('admin (Admin)')
  await exported.getByText('Show').click()
  await expect(exported.locator('pre')).toContainText('"action": "organization_settings_updated"')
  await expect(exported.locator('pre')).toContainText('"rows": 1')
})

test('German audit log at phone width', async ({ page }) => {
  await signIn(page, 'admin')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/audit?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Audit-Protokoll')
  await expect(page.getByRole('button', { name: 'Als CSV exportieren' })).toBeVisible()
  await expect(page.getByLabel('Akteur')).toBeVisible()
  await expect(page.locator('tr[data-action="ai_solved_change_denied"]')).toContainText('operator (Operator, Desk)')
  await page.goto('/admin/audit?lang=de&cursor=4')
  await expect(page.locator('tr[data-action="login_failed"]').getByTestId('service')).toHaveText('Nicht angemeldet')
  // Machine identifiers stay the same in every language.
  await expect(page.locator('tr[data-action="login_failed"] th code')).toHaveText('login_failed')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

for (const who of ['requester', 'operator', 'lead']) {
  test(`${who} gets neither the audit log nor its export`, async ({ page }) => {
    await signIn(page, who)
    await expect(page.getByRole('link', { name: 'Audit log' })).toHaveCount(0)
    for (const path of ['/admin/audit', '/admin/audit?action=login_failed']) {
      const response = await page.goto(path)
      expect(response?.status()).toBe(403)
      await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
      await expect(page.locator('table')).toHaveCount(0)
      await expect(page.getByText('login_failed')).toHaveCount(0)
    }
    // The API itself refuses the data, not only the page.
    const statuses = await page.evaluate(async () => {
      const headers = { 'x-requested-with': 'tikkit' }
      const search = await fetch('/api/admin/audit')
      const actions = await fetch('/api/admin/audit/actions')
      const exported = await fetch('/api/admin/audit/export', { method: 'POST', headers })
      return [search.status, actions.status, exported.status]
    })
    expect(statuses).toEqual([403, 403, 403])
  })
}
