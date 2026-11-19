import { expect, type Locator, type Page, test } from '@playwright/test'
import { PASSWORD, proxyApi, signIn } from './support'

const DESK = '30000000-0000-4000-8000-000000000101'

async function call(page: Page, path: string, body?: unknown) {
  return page.evaluate(async ({ path, body }) => {
    const response = await fetch(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify(body ?? {}),
    })
    const text = await response.text()
    try { return JSON.parse(text) } catch { return null }
  }, { path, body })
}

async function treeHas(page: Page, ...parts: string[]) {
  const tree = await page.getByRole('main').ariaSnapshot()
  for (const part of parts) expect(tree, tree).toContain(part)
}

/** Tab until `target` is focused, proving it is in the tab order. */
async function tabTo(page: Page, target: Locator) {
  for (let i = 0; i < 25; i++) {
    if (await target.evaluate((element) => element === document.activeElement).catch(() => false)) return
    await page.keyboard.press('Tab')
  }
  await expect(target).toBeFocused()
}

test('sign-in works from the keyboard and the accessibility tree names the form', async ({ page }) => {
  await proxyApi(page)
  await page.goto('/login')
  await page.keyboard.press('Tab')
  await expect(page.getByRole('link', { name: 'Skip to main content' })).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('main')).toBeFocused()
  await page.keyboard.press('Tab')
  await expect(page.getByLabel('Username')).toBeFocused()
  await page.keyboard.type('requester')
  await page.keyboard.press('Tab')
  await expect(page.getByLabel('Password')).toBeFocused()
  await page.keyboard.type(PASSWORD)
  await page.keyboard.press('Enter')
  await expect(page).not.toHaveURL(/\/login/)
  await treeHas(page, 'heading "My tickets" [level=1]')
})

test('a ticket can be submitted from the keyboard', async ({ page }) => {
  await signIn(page, 'requester')
  await call(page, '/api/test/ai', { ai_assistance: true })
  await page.goto('/tickets/new')
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Category').focus()
  await page.keyboard.press('s')
  await expect(page.getByLabel('Program')).toBeVisible()
  await tabTo(page, page.getByLabel('Program'))
  await page.keyboard.press('w')
  await tabTo(page, page.getByLabel('Subject'))
  await page.keyboard.type('Word crashes')
  await tabTo(page, page.getByLabel('Description'))
  await page.keyboard.type('It closes on startup.')
  await tabTo(page, page.getByLabel('Priority', { exact: true }))
  await page.keyboard.press('h')
  const submit = page.getByRole('button', { name: 'Submit request' })
  await tabTo(page, submit)
  await treeHas(page, 'heading "New ticket" [level=1]', 'combobox "Category"', 'combobox "Program"', 'textbox "Subject"', 'textbox "Description"', 'button "Submit request"')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}/)
  await treeHas(page, 'heading "Preparing an AI suggestion" [level=2]')
})

test('Solved and Still need help are keyboard decisions and do not fire twice', async ({ page }) => {
  await signIn(page, 'requester')
  await call(page, '/api/test/ai', { ai_assistance: true })
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
  const actions: string[] = []
  await page.route('**/api/tickets/*/actions', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON() as { action?: string }
    if (body.action === 'ai_solved' || body.action === 'ai_not_solved' || body.action === 'skip_ai') actions.push(body.action)
    await route.fallback()
  })
  await call(page, '/api/test/ai-result', { id, outcome: 'suggestion' })
  await page.reload()
  await expect(page.getByRole('heading', { name: 'AI-generated suggestion' })).toBeVisible()
  await expect(page.getByTestId('ai-suggestion')).toHaveAttribute('lang', 'en')
  await page.waitForTimeout(500)
  expect(actions).toEqual([])
  await treeHas(page, 'heading "AI-generated suggestion" [level=2]', 'button "Solved"', 'button "Still need help"')
  await page.getByRole('button', { name: 'Solved', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: 'Solved with the AI suggestion' })).toBeVisible()
  await expect(page.getByTestId('ai-announcement')).toHaveText(/Recorded as solved/)
  expect(actions).toEqual(['ai_solved'])

  await page.goto('/tickets/new')
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Still broken')
  await page.getByLabel('Description').fill('The steps did not help.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
  await page.getByRole('button', { name: 'Submit request' }).click()
  await expect(page).toHaveURL(/\/tickets\/[0-9a-f-]{36}/)
  const second = new URL(page.url()).pathname.split('/').pop()!
  await call(page, '/api/test/ai-result', { id: second, outcome: 'suggestion' })
  await page.reload()
  await page.getByRole('button', { name: 'Still need help' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: 'Sent to the support team' })).toBeVisible()
  await expect(page.getByTestId('ai-announcement')).toHaveText(/sent to the support team/)
  expect(actions).toEqual(['ai_solved', 'ai_not_solved'])
})

test('an operator can claim, reply, and propose a resolution from the keyboard', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto(`/queue/${DESK}`)
  await page.getByRole('button', { name: 'Claim ticket' }).press('Enter')
  await expect(page.getByText('Unassigned → operator')).toBeVisible()
  await page.getByRole('button', { name: 'Start work' }).press('Enter')
  await expect(page.locator('.badge')).toHaveText('In progress')
  await page.getByLabel('Message').focus()
  await page.keyboard.type('Checked the cable.')
  await page.getByRole('button', { name: 'Send public reply' }).press('Enter')
  await expect(page.getByText('Public reply sent.')).toBeVisible()
  await expect(page.getByText('Checked the cable.')).toBeVisible()
  await page.getByLabel(/Resolution summary/).focus()
  await page.keyboard.type('Replaced the paper-feed roller.')
  await page.getByRole('button', { name: 'Propose resolution' }).press('Enter')
  await expect(page.locator('.badge')).toHaveText('Awaiting confirmation')
  await treeHas(page, 'heading "Team and assignment" [level=2]')
})

test('a requester can accept or reject a resolution from the keyboard', async ({ page }) => {
  await signIn(page, 'requester')
  const first = (await call(page, '/api/test/history', {})) as { pending: string }
  await page.goto(`/tickets/${first.pending}`)
  await treeHas(page, 'button "Confirm resolved"', 'button "Still not solved"')
  await page.getByRole('button', { name: 'Confirm resolved' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.locator('.badge[data-status="closed"]')).toBeVisible()

  const second = (await call(page, '/api/test/history', {})) as { pending: string }
  await page.goto(`/tickets/${second.pending}`)
  await page.getByRole('button', { name: 'Still not solved' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.locator('.badge[data-status="in_progress"]')).toBeVisible()
})

test('an administrator can save settings from the keyboard', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/settings')
  const name = page.getByLabel('Organization name')
  await name.focus()
  await page.keyboard.press('ControlOrMeta+a')
  await page.keyboard.press('Backspace')
  await page.keyboard.type('Renamed support')
  await page.getByRole('button', { name: 'Save', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('status').filter({ hasText: 'Saved.' })).toBeVisible()
  await expect(name).toHaveValue('Renamed support')
  await treeHas(page, 'heading "Organization settings" [level=1]', 'textbox "Organization name"', 'button "Save"')
})

test('the discard dialog traps focus, closes on Escape, and returns focus', async ({ page }) => {
  await signIn(page, 'requester')
  await page.goto('/tickets/new')
  await page.waitForLoadState('networkidle')
  await page.getByLabel('Subject').fill('Keep this draft')
  await expect(page.getByText(/Draft saved at/)).toBeVisible()
  await page.reload()
  const discard = page.getByRole('button', { name: /Discard draft/ })
  await expect(discard).toBeVisible()
  await discard.press('Enter')
  const dialog = page.getByRole('dialog', { name: 'Discard this draft?' })
  await expect(dialog).toBeVisible()
  await expect(page.getByTestId('dialog-announcement')).toHaveText('Dialog opened: Discard this draft?')
  await expect.poll(() => page.evaluate(() => document.querySelector('dialog')?.contains(document.activeElement) ?? false)).toBe(true)
  for (let i = 0; i < 6; i++) await page.keyboard.press('Tab')
  expect(await page.evaluate(() => document.querySelector('dialog')?.contains(document.activeElement) ?? false)).toBe(true)
  await page.keyboard.press('Escape')
  await expect(dialog).toBeHidden()
  await expect(discard).toBeFocused()
  await expect(page.getByTestId('dialog-announcement')).toHaveText('Dialog closed.')
})
