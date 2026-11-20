import { expect, test } from '@playwright/test'
import { readFile } from 'node:fs/promises'
import { signIn } from './support'

const RANGE = 'from=2026-10-01&until=2026-10-31'

test('Admin can filter reporting totals, drill into submissions, and download the scoped CSV', async ({ page }) => {
  await signIn(page, 'admin')
  await page.getByRole('link', { name: 'Reports', exact: true }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Reports')
  await page.getByLabel('Submitted from (UTC)').fill('2026-10-01')
  await page.getByLabel('Submitted through (UTC)').fill('2026-10-31')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  const solved = page.getByRole('row', { name: 'Requester confirmed AI solved it 1' })
  await expect(solved).toBeVisible()
  await expect(page.getByText('33.3% (1 / 3)')).toBeVisible()
  await expect(page.getByRole('row', { name: 'Queue entry to first staff response 1 0.5 hr' })).toBeVisible()
  await solved.getByRole('link').click()
  await expect(page).toHaveURL(/ai_disposition=solved/)
  await expect(page.locator('tbody tr')).toHaveCount(1)
  await expect(page.locator('tbody tr')).toContainText('Solved by the AI suggestion')

  await page.goto(`/admin/reports?${RANGE}`)
  await page.getByRole('combobox', { name: 'Category', exact: true }).selectOption({ label: 'Hardware' })
  await page.getByRole('combobox', { name: 'Team', exact: true }).selectOption({ label: 'Desk' })
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page.getByRole('row', { name: 'Requester confirmed AI solved it 0' })).toBeVisible()
  await expect(page.getByRole('row', { name: 'AI did not solve it 1' })).toBeVisible()
  await expect(page.getByText('AI rates are unavailable with a team filter.')).toBeVisible()
  await expect(page.getByText('AI confirmation rate', { exact: false })).toHaveCount(0)
  await expect(page.getByText('AI deflection rate', { exact: false })).toHaveCount(0)
  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Download CSV' }).click()
  const file = await download
  expect(file.suggestedFilename()).toBe('ticket-report.csv')
  expect(await readFile((await file.path())!, 'utf8')).toBe('reference,ai_outcome\r\n305,not_solved\r\n306,ai_disabled')

  await page.getByRole('table', { name: 'Help desk workload' })
    .getByRole('link', { name: /Open.*operator/ }).click()
  await expect(page).toHaveURL(/entered_queue=true/)
  await expect(page).toHaveURL(/assignee_id=/)
  await expect(page.locator('tbody tr')).toHaveCount(1)
  await expect(page.locator('tbody tr')).toContainText('Escalated after the suggestion')
})

test('German report formats rates and dates at phone width and suppresses team-filtered rates', async ({ page }) => {
  await signIn(page, 'admin')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(`/admin/reports?${RANGE}&lang=de`)
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Berichte')
  await expect(page.getByText(/33,3\s% \(1 \/ 3\)/)).toBeVisible()
  await expect(page.getByRole('link', { name: '24.10.2026' })).toBeVisible()
  await expect(page.getByRole('table', { name: 'Arbeitslast im Support' }).getByText('Operator', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.goto(`/admin/reports?${RANGE}&team_id=50000000-0000-4000-8000-000000000001&lang=de`)
  await expect(page.getByText('Mit einem Teamfilter sind keine KI-Quoten verfügbar.')).toBeVisible()
  await expect(page.getByText('KI-Bestätigungsquote', { exact: false })).toHaveCount(0)
  await page.goto('/admin/reports?from=&until=&lang=de')
  await expect(page.locator('input[name="from"]')).toHaveValue(/^\d{4}-\d{2}-\d{2}$/)
  await expect(page.getByRole('button', { name: 'CSV herunterladen' })).toBeEnabled()
})

test('non-Admin cannot open reports or invoke either API action', async ({ page }) => {
  await signIn(page, 'operator')
  await expect(page.getByRole('link', { name: 'Reports', exact: true })).toHaveCount(0)
  expect((await page.goto('/admin/reports'))?.status()).toBe(403)
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
  expect(await page.evaluate(async () => {
    const report = await fetch('/api/admin/reports')
    const exportResponse = await fetch('/api/admin/reports/export', { method: 'POST', headers: { 'x-requested-with': 'tikkit' } })
    return [report.status, exportResponse.status]
  })).toEqual([403, 403])
})
