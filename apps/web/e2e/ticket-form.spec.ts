import { devices, expect, type Page, test } from '@playwright/test'
import { signIn } from './support'

const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64')
const PROGRAM = '00000000-0000-4000-9000-000000000010'
const HIGH = '00000000-0000-4000-9000-000000000022'

async function open(page: Page) {
  await signIn(page, 'requester')
  await page.goto('/tickets/new')
  // Wait for hydration so typed input is not reset by it.
  await page.waitForLoadState('networkidle')
}

const submitButton = (page: Page) => page.getByRole('button', { name: 'Submit request' })
const sentTickets = (page: Page) => page.evaluate(() => fetch('/api/test/tickets').then((r) => r.json()))
const post = (page: Page, path: string, body: unknown) =>
  page.evaluate(([p, b]) => fetch(p as string, { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify(b) }), [path, body])

async function fillSoftwareTicket(page: Page) {
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await page.getByLabel('Program').selectOption({ label: 'Word' })
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'High' })
}

test('identifies the signed-in requester and states that the request is still a draft', async ({ page }) => {
  await open(page)
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('New ticket')
  await expect(page.getByText('Submitting as')).toContainText('requester')
  await expect(page.getByText('Draft, not saved yet. Nothing has been sent to support.')).toBeVisible()
})

test('category validation: errors are linked to fields and conditional fields are required only while visible', async ({ page }) => {
  await open(page)
  await submitButton(page).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Please correct the highlighted fields.' })).toBeVisible()
  await expect(page.getByLabel('Category')).toBeFocused()
  await expect(page.getByLabel('Category')).toHaveAttribute('aria-invalid', 'true')
  await expect(page.getByLabel('Category')).toHaveAccessibleDescription(/This field is required\./)
  await expect(page.getByLabel('Program')).toHaveCount(0)

  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await expect(page.getByLabel('Program')).toBeVisible()
  await page.getByLabel('Subject').fill('Word crashes')
  await page.getByLabel('Description').fill('It closes on startup.')
  await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'Low' })
  await submitButton(page).click()
  await expect(page.getByLabel('Program')).toBeFocused()
  await expect(page.getByLabel('Program')).toHaveAccessibleDescription(/Which program is affected\?.*This field is required\./)

  // Another category hides the field, and with it the requirement.
  await page.getByLabel('Category').selectOption({ label: 'Network' })
  await expect(page.getByLabel('Program')).toHaveCount(0)
  await expect(page.getByLabel('Office location')).toBeVisible()
  await submitButton(page).click()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
})

test('a free-text program name replaces the choice list', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  await page.getByLabel('Program').selectOption({ label: 'Other (type your own answer)' })
  await submitButton(page).click()
  await expect(page.getByLabel(/Your answer for/)).toBeFocused()
  await page.getByLabel(/Your answer for/).fill('Notepad++')
  await submitButton(page).click()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  const [ticket] = await sentTickets(page)
  expect(ticket.intake.answers[PROGRAM]).toEqual({ text: 'Notepad++' })
})

test('a full no-AI submission shows the receipt with the chosen priority and sends no hidden answers', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  // Change the category: the program answer is hidden, remembered, and not sent.
  await page.getByLabel('Category').selectOption({ label: 'Network' })
  await expect(page.getByText(/will not be sent: Program/)).toBeVisible()
  await expect(page.getByText('goes straight to the support team without AI assistance')).toBeVisible()
  await page.getByLabel('Category').selectOption({ label: 'Software' })
  await expect(page.getByLabel('Program')).toHaveValue(/.+/)
  await page.getByLabel('Category').selectOption({ label: 'Network' })
  await submitButton(page).click()

  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeFocused()
  await expect(page.getByText('Your request #1001 was sent to support.')).toBeVisible()
  await expect(page.getByText('Queued', { exact: true })).toBeVisible()
  await expect(page.getByRole('term').filter({ hasText: 'Priority' })).toBeVisible()
  await expect(page.getByText('High', { exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Open the ticket' })).toHaveAttribute('href', /\/tickets\/.+/)
  const tickets = await sentTickets(page)
  expect(tickets).toHaveLength(1)
  expect(tickets[0].intake).toMatchObject({ category_id: '00000000-0000-4000-9000-000000000002', priority_id: HIGH, subject: 'Word crashes' })
  expect(tickets[0].intake.answers).toEqual({})
})

test('the German interface shows the same form', async ({ page }) => {
  await open(page)
  await page.goto('/tickets/new?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Neues Ticket')
  await expect(page.getByLabel('Kategorie')).toBeVisible()
  await page.getByRole('button', { name: 'Anfrage absenden' }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Bitte korrigieren Sie die markierten Felder.' })).toBeVisible()
  await expect(page.getByText('Dieses Feld ist erforderlich.').first()).toBeVisible()
})

test('draft recovery: autosaves, resumes after a reload, and keeps text when the category was archived', async ({ page }) => {
  await open(page)
  await page.getByLabel('Subject').fill('Printer jams')
  await page.getByLabel('Description').fill('Paper stuck in tray 2.')
  await expect(page.getByText(/Draft saved at .* It has not been sent to support yet\./)).toBeVisible()

  await page.reload()
  await page.waitForLoadState('networkidle')
  await expect(page.getByLabel('Subject')).toHaveValue('')
  await expect(page.getByRole('heading', { name: 'Saved drafts' })).toBeVisible()
  await page.getByRole('button', { name: 'Resume: Printer jams' }).click()
  await expect(page.getByLabel('Subject')).toHaveValue('Printer jams')
  await expect(page.getByLabel('Description')).toHaveValue('Paper stuck in tray 2.')

  // A draft whose category and a chosen answer no longer exist keeps everything else.
  await post(page, '/api/test/draft', {
    intake: {
      category_id: '00000000-0000-4000-9000-0000000000ff',
      priority_id: HIGH,
      subject: 'Old category',
      description: 'Still here.',
      answers: { [PROGRAM]: { text: 'Lotus Notes' } },
    },
  })
  await page.reload()
  await page.waitForLoadState('networkidle')
  await page.getByRole('button', { name: 'Resume: Old category' }).click()
  await expect(page.getByText('The category you chose earlier is no longer available.')).toBeVisible()
  await expect(page.getByText('Program: Lotus Notes')).toBeVisible()
  await expect(page.getByLabel('Description')).toHaveValue('Still here.')
  await expect(page.getByLabel('Priority', { exact: true })).toHaveValue(HIGH)
  await expect(page.getByLabel('Category')).toHaveValue('')
})

test('a draft changed elsewhere is never overwritten silently', async ({ page }) => {
  await open(page)
  await page.getByLabel('Subject').fill('First version')
  await expect(page.getByText(/Draft saved at/)).toBeVisible()
  await post(page, '/api/test/bump-drafts', {})

  await page.getByLabel('Description').fill('Typed after the other tab saved.')
  await expect(page.getByText('This draft was changed or removed somewhere else.')).toBeVisible()
  await expect(page.getByLabel('Description')).toHaveValue('Typed after the other tab saved.')

  // Loading the saved version replaces the form with it, on the user's say-so.
  await page.getByRole('button', { name: 'Load the saved version' }).click()
  await expect(page.getByLabel('Subject')).toHaveValue('First version (other tab)')
  await expect(page.getByText('This draft was changed or removed somewhere else.')).toHaveCount(0)

  // Keeping their own version saves it as a new draft.
  await page.getByLabel('Description').fill('Second attempt.')
  await expect(page.getByText(/Draft saved at/)).toBeVisible()
  await post(page, '/api/test/remove-drafts', {})
  await page.getByLabel('Description').fill('Third attempt.')
  await expect(page.getByText('This draft was changed or removed somewhere else.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Load the saved version' })).toHaveCount(0)
  await page.getByRole('button', { name: 'Keep mine as a new draft' }).click()
  await expect(page.getByText(/Draft saved at/)).toBeVisible()
  await expect(page.getByLabel('Description')).toHaveValue('Third attempt.')
})

test('discarding a saved draft asks first', async ({ page }) => {
  await open(page)
  await post(page, '/api/test/draft', { intake: { subject: 'Throw away', description: '', answers: {}, screenshot_ids: [] } })
  await page.reload()
  await page.waitForLoadState('networkidle')
  await page.getByRole('button', { name: 'Discard draft: Throw away' }).click()
  await expect(page.getByRole('dialog', { name: 'Discard this draft?' })).toBeVisible()
  await page.getByRole('dialog').getByRole('button', { name: 'Discard draft' }).click()
  await expect(page.getByRole('heading', { name: 'Saved drafts' })).toHaveCount(0)
})

test('double submission creates one ticket', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  await submitButton(page).dblclick()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect(await sentTickets(page)).toHaveLength(1)
})

test('pressing Enter twice while a submission is running creates one ticket', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  await page.getByLabel('Subject').focus()
  await page.keyboard.press('Enter')
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect(await sentTickets(page)).toHaveLength(1)
})

test('a resumed draft submits through the draft and leaves the list', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  await expect(page.getByText(/Draft saved at/)).toBeVisible()
  await submitButton(page).click()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect(await sentTickets(page)).toHaveLength(1)
  await page.getByRole('button', { name: 'Submit another request' }).click()
  await expect(page.getByLabel('Subject')).toHaveValue('')
  await expect(page.getByRole('heading', { name: 'Saved drafts' })).toHaveCount(0)
})

test('screenshots: select, preview, remove, and keep the form when an upload fails', async ({ page }) => {
  await open(page)
  await page.getByLabel('Description').fill('Screenshot attached.')
  await page.getByLabel('Add screenshots').setInputFiles({ name: 'error.png', mimeType: 'image/png', buffer: PNG })
  const preview = page.getByRole('img', { name: 'error.png' })
  await expect(preview).toBeVisible()
  await expect.poll(() => preview.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBe(1)

  await page.getByLabel('Add screenshots').setInputFiles({ name: 'corrupt.png', mimeType: 'image/png', buffer: Buffer.from('nope') })
  await expect(page.getByRole('alert').filter({ hasText: 'corrupt.png: This image is damaged or cannot be read.' })).toBeVisible()
  await expect(page.getByLabel('Description')).toHaveValue('Screenshot attached.')
  await expect(preview).toBeVisible()

  await page.getByLabel('Add screenshots').setInputFiles({ name: 'notes.gif', mimeType: 'image/gif', buffer: PNG })
  await expect(page.getByRole('alert').filter({ hasText: 'Only PNG, JPEG, and WebP images can be attached.' })).toBeVisible()

  await page.getByRole('button', { name: 'Remove: error.png' }).click()
  await expect(preview).toHaveCount(0)
})

test('the screenshot reference is submitted with the ticket', async ({ page }) => {
  await open(page)
  await fillSoftwareTicket(page)
  await page.getByLabel('Add screenshots').setInputFiles({ name: 'error.png', mimeType: 'image/png', buffer: PNG })
  await expect(page.getByRole('img', { name: 'error.png' })).toBeVisible()
  await submitButton(page).click()
  await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  expect((await sentTickets(page))[0].intake.screenshot_ids).toHaveLength(1)
})

test.describe('on a phone', () => {
  const { defaultBrowserType: _ignored, ...phone } = devices['Pixel 7']
  test.use(phone)

  test('the form fits the screen, can be filled by touch, and submits', async ({ page }) => {
    await open(page)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
    for (const label of ['Subject', 'Description']) {
      // 16px+ text avoids the zoom-on-focus of mobile browsers.
      const size = await page.getByLabel(label).evaluate((el) => parseFloat(getComputedStyle(el).fontSize))
      expect(size).toBeGreaterThanOrEqual(16)
    }
    await page.getByLabel('Subject').tap()
    await page.getByLabel('Subject').fill('Cannot connect')
    await page.getByLabel('Description').tap()
    await page.getByLabel('Description').fill('Wi-Fi drops every few minutes.')
    await page.getByLabel('Category').selectOption({ label: 'Network' })
    await page.getByLabel('Priority', { exact: true }).selectOption({ label: 'Normal' })
    const button = submitButton(page)
    await button.scrollIntoViewIfNeeded()
    const box = await button.boundingBox()
    expect(box!.height).toBeGreaterThanOrEqual(44)
    expect(box!.width).toBeGreaterThan(200)
    await button.tap()
    await expect(page.getByRole('heading', { name: 'Request sent' })).toBeVisible()
  })
})
