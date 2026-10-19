import { expect, type Page, test } from '@playwright/test'
import { signIn } from './support'

const ids = {
  deskUnclaimed: '30000000-0000-4000-8000-000000000101',
  deskClaimed: '30000000-0000-4000-8000-000000000102',
  claimedByLead: '30000000-0000-4000-8000-000000000104',
  closed: '30000000-0000-4000-8000-000000000105',
  general: '30000000-0000-4000-8000-000000000001',
  aiSolved: '30000000-0000-4000-8000-000000000201',
}
async function post(page: Page, path: string, body: unknown) {
  return page.evaluate(async ({ path, body }) => {
    const r = await fetch(path, { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify(body) })
    return r.status
  }, { path, body })
}
const open = async (page: Page, id: string, query = '') => {
  await page.goto(`/queue/${id}${query}`)
  await page.waitForLoadState('networkidle')
}
const status = (page: Page) => page.locator('.badge')

test('an operator works an unclaimed team ticket: work, wait, reply, note, priority, propose', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/queue?view=team')
  await page.getByRole('link', { name: 'Desk unclaimed' }).click()
  await expect(page).toHaveURL(new RegExp(`/queue/${ids.deskUnclaimed}`))
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { level: 1 })).toContainText('#101 · Desk unclaimed')

  // Frozen intake, requester identity, team, and the existing conversation with its note marked staff-only.
  await expect(page.getByText('Original submission')).toBeVisible()
  await expect(page.getByText('Printer jams').first()).toBeVisible()
  await expect(page.getByText('Program')).toBeVisible()
  await expect(page.getByText('(requester)')).toBeVisible()
  await expect(page.getByText('Desk', { exact: true })).toBeVisible()
  await expect(page.getByText('Public reply · Visible to the requester')).toBeVisible()
  await expect(page.getByText('Internal note · Only support staff can see this')).toBeVisible()

  await page.getByRole('button', { name: 'Start work' }).click()
  await expect(page.getByText('Ticket updated.')).toBeVisible()
  await expect(status(page)).toHaveText('In progress')

  // The composer states who will see the message before sending.
  await expect(page.getByText('Visible to the requester. The requester will see this reply')).toBeVisible()
  await page.getByLabel('Message').fill('Which program jams?')
  await page.getByRole('button', { name: 'Send public reply' }).click()
  await expect(page.getByText('Public reply sent.')).toBeVisible()
  await expect(page.getByText('Which program jams?')).toBeVisible()
  await page.getByLabel('Internal note', { exact: true }).check()
  await expect(page.getByText('Only support staff can see this. Internal notes')).toBeVisible()
  await page.getByLabel('Message').fill('Waiting on the requester, then swap the roller.')
  await page.getByRole('button', { name: 'Add internal note' }).click()
  await expect(page.getByText('Internal note added.')).toBeVisible()
  await expect(page.getByText('Waiting on the requester, then swap the roller.')).toBeVisible()

  await page.getByRole('button', { name: 'Wait for requester' }).click()
  await expect(status(page)).toHaveText('Waiting for requester')
  await page.getByRole('button', { name: 'Resume work' }).click()
  await expect(status(page)).toHaveText('In progress')

  await page.getByLabel(/Resolution summary/).fill('Unsent resolution draft')
  await page.getByLabel('New priority').selectOption({ label: 'Urgent' })
  await page.getByRole('button', { name: 'Save priority' }).click()
  await expect(page.getByText('Normal → Urgent')).toBeVisible()
  await expect(page.getByRole('term').filter({ hasText: 'Current priority' }).locator('xpath=following-sibling::dd')).toHaveText('Urgent')

  await expect(page.getByLabel(/Resolution summary/)).toHaveValue('Unsent resolution draft')
  await page.getByLabel(/Resolution summary/).fill('')

  // A proposal needs a summary; it waits for the requester and cannot close the ticket.
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(page.getByLabel(/Resolution summary/)).toHaveAttribute('aria-invalid', 'true')
  await page.getByLabel(/Resolution summary/).fill('Replaced the paper-feed roller.')
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(status(page)).toHaveText('Awaiting confirmation')
  await expect(page.getByText('Awaiting requester confirmation. The ticket is not closed.')).toBeVisible()
  await expect(page.getByText('Replaced the paper-feed roller.').first()).toBeVisible()
  await expect(page.getByRole('button', { name: /close/i })).toHaveCount(0)
  await page.getByLabel(/Resolution summary/).fill('Replaced the roller and cleaned the path.')
  await page.getByRole('button', { name: 'Replace proposal' }).click()
  await expect(page.getByText('Proposal 2')).toBeVisible()
  await expect(page.getByRole('main')).toContainText('By: operator')
})

test('claimed, general-queue, and AI records are read-only; other teams see nothing', async ({ page, browser }) => {
  await signIn(page, 'operator')
  // Claimed by the lead: the operator can read but not write.
  await open(page, ids.claimedByLead)
  await expect(page.getByText(/assigned to someone else/)).toBeVisible()
  await expect(page.getByRole('textbox')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /Send|Add internal|Propose|Save priority|Start work|Wait for/ })).toHaveCount(0)
  await expect(page.getByText('Same model failed last month.')).toBeVisible()
  // Shared intake is dispatch-only.
  await open(page, ids.general)
  await expect(page.getByText(/in the general queue. It has to be sent to a team/)).toBeVisible()
  await expect(page.getByRole('textbox')).toHaveCount(0)
  // AI-solved records are invisible to operators, even by direct link.
  await open(page, ids.aiSolved)
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Page not found')
  // A non-member cannot see a dispatched ticket.
  const context = await browser.newContext()
  try {
    const other = await context.newPage()
    await signIn(other, 'specialist')
    await open(other, ids.deskUnclaimed)
    await expect(other.getByRole('heading', { level: 1 })).toHaveText('Page not found')
  } finally {
    await context.close()
  }
  // The lead (who owns that claim) and Admins get the write controls.
  await page.getByRole('button', { name: 'Sign out' }).click()
  await signIn(page, 'lead')
  await open(page, ids.claimedByLead)
  await expect(page.getByRole('button', { name: 'Wait for requester' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Send public reply' })).toBeVisible()
})

test('a stale page gets a recoverable conflict and keeps the unsent proposal', async ({ page }) => {
  await signIn(page, 'operator')
  await open(page, ids.deskClaimed)
  await expect(status(page)).toHaveText('In progress')
  await page.getByLabel(/Resolution summary/).fill('Replaced the roller.')
  // Someone else replies after this page loaded.
  expect(await post(page, '/api/test/concurrent', { id: ids.deskClaimed })).toBe(204)
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(page.getByRole('alert')).toContainText('changed since you loaded it')
  await expect(status(page)).toHaveText('In progress')
  await expect(page.getByText('Ticket updated.')).toHaveCount(0)
  // The refresh shows the newer reply; the text is intact and resubmitting now works.
  await expect(page.getByText('Newer reply from the requester')).toBeVisible()
  await expect(page.getByLabel(/Resolution summary/)).toHaveValue('Replaced the roller.')
  await page.getByRole('button', { name: 'Propose resolution' }).click()
  await expect(status(page)).toHaveText('Awaiting confirmation')
})

test('a claim taken away mid-session turns the page read-only safely', async ({ page }) => {
  await signIn(page, 'operator')
  await open(page, ids.deskClaimed)
  await page.getByLabel('Message').fill('Draft answer')
  await page.getByLabel(/Resolution summary/).fill('Draft proposal')
  expect(await post(page, '/api/test/concurrent', { id: ids.deskClaimed, assignee: 'lead' })).toBe(204)
  await page.getByRole('button', { name: 'Send public reply' }).click()
  // The operator is no longer in charge only if not the assignee; the stub moved the claim to the lead.
  await expect(page.getByRole('alert')).toContainText('changed since you loaded it')
  await expect(page.getByText(/assigned to someone else/)).toBeVisible()
  await expect(page.getByLabel('Message')).toHaveValue('Draft answer')
  await expect(page.getByLabel(/Resolution summary/)).toHaveValue('Draft proposal')
  await expect(page.getByLabel(/Resolution summary/)).toHaveAttribute('readonly', '')
  await expect(page.getByRole('button', { name: 'Send public reply' })).toBeDisabled()
  await expect(page.getByText('Draft answer')).toHaveCount(0)
})

test('reopening follows the Admin toggle, including after it is revoked on a loaded page', async ({ page }) => {
  await signIn(page, 'operator')
  await open(page, ids.closed)
  await expect(status(page)).toHaveText('Closed')
  await expect(page.getByText('This ticket is closed and cannot be changed.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toHaveCount(0)

  expect(await post(page, '/api/test/policy', { allow_reopening: true })).toBe(204)
  await open(page, ids.closed)
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toBeVisible()
  expect(await post(page, '/api/test/policy', { allow_reopening: false })).toBe(204)
  await page.getByRole('button', { name: 'Reopen ticket' }).click()
  await expect(page.getByRole('alert')).toContainText('changed since you loaded it')
  await expect(page.getByRole('button', { name: 'Reopen ticket' })).toHaveCount(0)
  await expect(status(page)).toHaveText('Closed')

  expect(await post(page, '/api/test/policy', { allow_reopening: true })).toBe(204)
  await open(page, ids.closed)
  await page.getByRole('button', { name: 'Reopen ticket' }).click()
  await expect(status(page)).toHaveText('In progress')
  await expect(page.getByRole('main')).toContainText('Reopened')
})

test('German page labels and requester-denied access', async ({ page, browser }) => {
  await signIn(page, 'operator')
  await open(page, ids.deskUnclaimed, '?lang=de')
  await expect(page.getByRole('button', { name: 'Bearbeitung beginnen' })).toBeVisible()
  await expect(page.getByText('Interne Notiz · Nur für Support-Mitarbeitende sichtbar')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Öffentliche Antwort senden' })).toBeVisible()

  const context = await browser.newContext()
  try {
    const requester = await context.newPage()
    await signIn(requester, 'requester')
    await requester.goto(`/queue/${ids.deskUnclaimed}`)
    await expect(requester.getByRole('heading', { level: 1 })).toHaveText('Access denied')
  } finally {
    await context.close()
  }
})
