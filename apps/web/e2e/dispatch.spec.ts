import { expect, type Page, test } from '@playwright/test'
import { signIn } from './support'

const fact = (page: Page, term: RegExp) => page.getByRole('term').filter({ hasText: term }).locator('xpath=following-sibling::dd')
const open = async (page: Page, path: string) => {
  await page.goto(path)
  await page.waitForLoadState('networkidle')
}

test('a general-queue ticket is dispatched, claimed, and worked in the team without a duplicate', async ({ page }) => {
  await signIn(page, 'operator')
  await open(page, '/queue')
  await page.getByRole('link', { name: 'General ticket 28', exact: true }).click()
  const url = /\/queue\/30000000-0000-4000-8000-000000000028$/
  await expect(page).toHaveURL(url)
  await page.waitForLoadState('networkidle')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('#28 · General ticket 28')
  await expect(page.getByText(/in the general queue. It has to be sent to a team/)).toBeVisible()
  // Nothing picks a team: the operator chooses one, and nobody is assigned by dispatch.
  await expect(page.getByLabel('Destination team')).toHaveValue('')
  await page.getByLabel('Destination team').selectOption({ label: 'Desk' })
  await page.getByRole('button', { name: 'Send to team' }).click()
  await expect(page.getByText('Ticket updated.')).toBeVisible()
  await expect(fact(page, /^Team$/)).toHaveText('Desk')
  await expect(fact(page, /^Assigned to$/)).toHaveText('Unassigned')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('#28 · General ticket 28')
  await expect(page).toHaveURL(url)

  await page.getByRole('button', { name: 'Claim ticket' }).click()
  await expect(fact(page, /^Assigned to$/)).toHaveText('operator')
  await page.getByRole('button', { name: 'Start work' }).click()
  await expect(page.locator('.badge')).toHaveText('In progress')
  const history = page.getByRole('region', { name: 'Status history' })
  await expect(page.getByText('Team and assignment history')).toBeVisible()
  await expect(history.getByText(/Sent to team: General queue \(not dispatched\) → Desk/)).toBeVisible()
  await expect(history.getByText(/Claimed: Unassigned → operator/)).toBeVisible()

  // The same ticket moved: gone from general intake, once in the team queue.
  await open(page, '/queue?view=team')
  await expect(page.getByRole('link', { name: 'General ticket 28', exact: true })).toHaveCount(1)
  await open(page, '/queue?view=general')
  await expect(page.getByRole('link', { name: 'General ticket 27', exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: 'General ticket 28', exact: true })).toHaveCount(0)
})

test('a dispatcher outside the destination team gets a receipt, not the ticket', async ({ page }) => {
  await signIn(page, 'specialist')
  await open(page, '/queue')
  await page.getByRole('link', { name: 'General ticket 27', exact: true }).click()
  await expect(page).toHaveURL(/\/queue\/30000000-0000-4000-8000-000000000027$/)
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Destination team').selectOption({ label: 'Desk' })
  await page.getByRole('button', { name: 'Send to team' }).click()
  await expect(page.getByText('Ticket #27 was sent to Desk. You are not a member of that team, so you can no longer open it.')).toBeVisible()
  await expect(page.getByText('Original submission')).toHaveCount(0)
  await page.reload()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Page not found')
})

test('the team lead reassigns and unassigns a member claim', async ({ page }) => {
  await signIn(page, 'lead')
  await open(page, '/queue/30000000-0000-4000-8000-000000000102?lang=de')
  await expect(fact(page, /^Zugewiesen an$/)).toHaveText('operator')
  await page.getByLabel('Zuweisen an').selectOption({ label: 'lead' })
  await page.getByRole('button', { name: 'Zuweisen', exact: true }).click()
  await expect(fact(page, /^Zugewiesen an$/)).toHaveText('lead')
  await page.getByRole('button', { name: 'Zuweisung entfernen' }).click()
  await expect(fact(page, /^Zugewiesen an$/)).toHaveText('Nicht zugewiesen')
  await expect(page.getByRole('button', { name: 'Ticket übernehmen' })).toBeVisible()
})
