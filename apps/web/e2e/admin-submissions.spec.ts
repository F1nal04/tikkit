import { expect, test, type Page } from '@playwright/test'
import { signIn } from './support'

const SOLVED = '/admin/submissions/60000000-0000-4000-8000-000000000301'

/** Every request the browser sends to the API while `run` executes. */
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

test('an Admin finds mixed outcomes, filters them, and opens the AI-solved record read-only', async ({ page }) => {
  await signIn(page, 'admin')
  await page.getByRole('link', { name: 'Submissions' }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Submissions')
  await expect(page.getByRole('link', { name: 'Submissions' })).toHaveAttribute('aria-current', 'page')
  const rows = page.locator('tbody tr')
  await expect(rows).toHaveCount(6)
  const row = (outcome: string) => page.locator(`tr[data-ai-outcome="${outcome}"]`)
  await expect(row('solved')).toContainText('Solved by AI: confirmed by the requester')
  await expect(row('awaiting_decision')).toContainText('Unconfirmed: suggestion shown, no decision')
  await expect(row('awaiting_decision')).toContainText('Not resolved yet')
  await expect(row('cancelled')).toContainText('Abandoned: cancelled without a decision')
  await expect(row('failed')).toContainText('Failed: no suggestion, sent to the help desk')
  await expect(row('none')).toContainText('Resolved by the help desk, confirmed by the requester')

  // Filters are a plain GET form kept in the address bar.
  await page.getByLabel('AI outcome').selectOption('cancelled')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(page).toHaveURL(/ai_outcome=cancelled/)
  await expect(rows).toHaveCount(1)
  await expect(rows.first()).toContainText('Suggestion abandoned')
  await page.getByRole('link', { name: 'Clear filters' }).click()
  await page.getByLabel('Resolution source').selectOption('ai')
  await page.getByRole('button', { name: 'Apply filters' }).click()
  await expect(rows).toHaveCount(1)

  // Opening and reading the record sends nothing but reads.
  const requests = await apiRequests(page, async () => {
    await page.getByRole('link', { name: '#301 Solved by the AI suggestion' }).click()
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('#301 · Solved by the AI suggestion')
    await expect(page.getByTestId('ai-notice')).toContainText('explicitly confirmed')
    await expect(page.getByTestId('decision')).toHaveText('Solved')
    await expect(page.locator('#event-3')).toContainText('AI solution confirmed')
    await expect(page.locator('#event-3')).toContainText('requester (requester)')
    await expect(page.getByTestId('description')).toHaveText('Printer jams on page two.')
  })
  expect(requests.length).toBeGreaterThan(0)
  expect(requests.every((r) => r.startsWith('GET '))).toBe(true)
  // The stored suggestion is untrusted text: shown literally, never as markup.
  const suggestion = page.getByTestId('suggestion')
  await expect(suggestion).toContainText('<img src=x onerror="window.aiXss = true">')
  await expect(suggestion.locator('img, script, a')).toHaveCount(0)
  expect(await page.evaluate(() => (window as unknown as { aiXss?: boolean }).aiXss)).toBeUndefined()
  // No reopen, escalation, or any other action, and no way into operator work.
  await expect(page.getByRole('main').getByRole('button')).toHaveCount(0)
  await expect(page.getByRole('link', { name: 'Open in the help desk view' })).toHaveCount(0)
})

test('unconfirmed and failed attempts read as not solved; the page source holds no secrets', async ({ page }) => {
  await signIn(page, 'admin')
  for (const [reference, text] of [
    ['302', 'Unconfirmed: the requester has not decided'],
    ['303', 'Abandoned: the requester cancelled the ticket'],
    ['304', 'Failed: no suggestion was created'],
  ]) {
    const response = await page.goto(`/admin/submissions/60000000-0000-4000-8000-000000000${reference}`)
    expect(response?.status()).toBe(200)
    await expect(page.getByTestId('ai-notice')).toContainText(text)
    await expect(page.getByTestId('decision')).toHaveText('No decision')
    await expect(page.getByText('Confirmation event')).toHaveCount(0)
    // Server-rendered HTML and its serialized data, then the hydrated document.
    for (const source of [await response!.text(), await page.content()]) {
      for (const secret of ['WXYZ', 'key_hint', 'ciphertext', 'api_key', 'credential_set_at']) expect(source).not.toContain(secret)
    }
  }
  await expect(page.getByText('Took too long')).toBeVisible()
  await expect(page.getByRole('link', { name: 'Open in the help desk view' })).toHaveAttribute('href', '/queue/60000000-0000-4000-8000-000000000304')
})

test('German submission view at phone width', async ({ page }) => {
  await signIn(page, 'admin')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/submissions?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Einreichungen')
  await expect(page.locator('tr[data-ai-outcome="cancelled"]')).toContainText('Abgebrochen: ohne Entscheidung storniert')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.goto(`${SOLVED}?lang=de`)
  await expect(page.getByTestId('resolution')).toHaveText('Von der KI gelöst, vom Antragsteller bestätigt')
  await expect(page.getByTestId('ai-notice')).toContainText('Keine Rolle kann ihn erneut öffnen oder eskalieren')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

for (const who of ['requester', 'operator', 'lead']) {
  test(`${who} gets neither the submission list nor a record`, async ({ page }) => {
    await signIn(page, who)
    await expect(page.getByRole('link', { name: 'Submissions' })).toHaveCount(0)
    for (const path of ['/admin/submissions', '/admin/submissions?ai_outcome=solved', SOLVED]) {
      const response = await page.goto(path)
      expect(response?.status()).toBe(403)
      await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
      await expect(page.getByText('Solved by the AI suggestion')).toHaveCount(0)
      await expect(page.locator('table')).toHaveCount(0)
    }
    // The API itself refuses the dataset, not only the page.
    expect(await page.evaluate(() => fetch('/api/admin/submissions').then((r) => r.status))).toBe(403)
  })
}
