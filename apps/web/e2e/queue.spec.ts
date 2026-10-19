import { expect, test } from '@playwright/test'
import { signIn } from './support'

const rows = (page: import('@playwright/test').Page) => page.getByRole('table').getByRole('row')

test('an operator works through the general queue: columns, counts, filters, and pages', async ({ page }) => {
  await signIn(page, 'operator')
  await page.getByRole('link', { name: 'General queue' }).first().click()
  await expect(page).toHaveURL('/queue')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('General queue')

  // Open work only: AI-solved, still-in-AI, and finished tickets are absent.
  const table = page.getByRole('table', { name: 'General queue: Tickets' })
  await expect(table.getByRole('columnheader')).toHaveText([
    'Reference', 'Subject', 'Category', 'Requester', 'Priority', 'Age', 'Assignee', 'Status',
  ])
  await expect(rows(page)).toHaveCount(1 + 25)
  await expect(page.getByText('AI solved record')).toHaveCount(0)
  await expect(page.getByText('Still with the AI')).toHaveCount(0)
  await expect(page.getByText('Desk unclaimed')).toHaveCount(0)
  await expect(table.getByRole('row').nth(1)).toContainText('#28')
  await expect(table.getByRole('row').nth(1)).toContainText('Unassigned')
  await expect(table.getByRole('row').nth(1)).toContainText('Queued')

  const tabs = page.getByRole('navigation', { name: 'Queues' })
  await expect(tabs.getByRole('link', { name: /^General queue/ })).toHaveAttribute('aria-current', 'page')
  await expect(tabs.getByRole('link', { name: /^General queue/ })).toContainText('28')
  await expect(tabs.getByRole('link', { name: /^Team queue/ })).toContainText('2')

  // Stable pagination: the second page continues where the first ended.
  await page.getByRole('link', { name: 'Next page' }).click()
  await expect(page).toHaveURL(/cursor=25/)
  await expect(rows(page)).toHaveCount(1 + 3)
  await expect(table.getByRole('row').nth(1)).toContainText('#3')
  await expect(page.getByRole('link', { name: 'Next page' })).toHaveCount(0)
  await page.getByRole('link', { name: 'First page' }).click()
  await expect(rows(page)).toHaveCount(1 + 25)

  // Filter and sort through the URL, no client state.
  await page.getByLabel('Priority').selectOption({ label: 'Urgent' })
  await page.getByLabel('Sort by').selectOption('oldest')
  await page.getByRole('button', { name: 'Apply' }).click()
  await expect(page).toHaveURL(/priority_id=.*sort=oldest/)
  await expect(rows(page)).toHaveCount(1 + 5)
  await expect(table.getByRole('row').nth(1)).toContainText('#5')
  await page.getByRole('link', { name: 'Clear filters' }).click()
  await expect(rows(page)).toHaveCount(1 + 25)
})

test('the team queue shows dispatched work and the finished-status filter', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/queue?view=team')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Team queue')
  await expect(page.getByRole('table').getByRole('columnheader', { name: 'Team' })).toBeVisible()
  await expect(rows(page)).toHaveCount(1 + 2)
  await expect(page.getByRole('table')).toContainText('Desk unclaimed')
  await expect(page.getByRole('table')).toContainText('Desk claimed')
  await expect(page.getByRole('table')).not.toContainText('General ticket')
  await page.getByLabel('Status').selectOption('closed')
  await page.getByRole('button', { name: 'Apply' }).click()
  await expect(rows(page)).toHaveCount(1 + 1)
  await expect(page.getByRole('table')).toContainText('Desk finished')
  await page.goto('/queue?view=team&unassigned=true')
  await expect(rows(page)).toHaveCount(1 + 1)
  await expect(page.getByRole('table')).toContainText('Desk unclaimed')
})

test('a specialist without a team gets an empty team queue; Admins get the overview', async ({ page }) => {
  await signIn(page, 'specialist')
  await page.goto('/queue?view=team')
  await expect(page.getByText('No tickets match in your team queue.')).toBeVisible()
  await expect(page.getByRole('table')).toHaveCount(0)
  await page.getByRole('button', { name: 'Sign out' }).click()

  await signIn(page, 'admin')
  await page.goto('/queue')
  const tabs = page.getByRole('navigation', { name: 'Queues' })
  await expect(tabs.getByRole('link')).toHaveText([/^General queue/, /^All teams/, /^Everything/])
  await tabs.getByRole('link', { name: /^Everything/ }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Everything')
  await expect(page.getByRole('table')).toContainText('Desk claimed')
  await expect(page.getByRole('table')).not.toContainText('AI solved record')
})

test('requesters are denied the queue', async ({ page }) => {
  await signIn(page, 'requester')
  await page.goto('/queue')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
})

test('shows German and recovers when the API is unavailable', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/queue')
  await page.getByLabel('Language', { exact: true }).selectOption('de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Allgemeine Warteschlange')
  await expect(page.getByRole('columnheader', { name: 'Referenz' })).toBeVisible()
  await expect(page.getByText('Nicht zugewiesen').first()).toBeVisible()

  // The queue request fails once on a client navigation, then the retry succeeds.
  let failing = true
  await page.route(
    (url) => url.pathname === '/api/queue',
    (route) => (failing ? route.abort() : route.fallback()),
  )
  await page.getByRole('link', { name: /^Team-Warteschlange/ }).click()
  await expect(page.getByRole('alert')).toHaveText('Der Dienst ist nicht erreichbar. Bitte versuchen Sie es später erneut.')
  failing = false
  await page.getByRole('button', { name: 'Erneut versuchen' }).click()
  await expect(page.getByRole('table')).toContainText('Desk unclaimed')
})
