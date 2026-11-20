import { expect, test } from '@playwright/test'
import { signIn } from './support'

// The Admin banner for restore mode (docs/backup-restore.md), under the production
// build's nonce CSP: it is plain text in the page, so it needs no inline script or style.

async function setRestoreMode(page: import('@playwright/test').Page, on: boolean) {
  await page.evaluate(async (value) => {
    await fetch('/api/test/restore-mode', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' },
      body: JSON.stringify({ on: value }),
    })
  }, on)
}

async function trackViolations(page: import('@playwright/test').Page) {
  const violations: string[] = []
  page.on('console', (message) => {
    if (/Content Security Policy/i.test(message.text())) violations.push(message.text())
  })
  await page.addInitScript(() => {
    document.addEventListener('securitypolicyviolation', (event) => {
      const list = ((window as unknown as { cspViolations?: string[] }).cspViolations ??= [])
      list.push(`${event.violatedDirective} ${event.blockedURI}`)
    })
  })
  return async () => [
    ...violations,
    ...(await page.evaluate(() => (window as unknown as { cspViolations?: string[] }).cspViolations ?? [])),
  ]
}

test('an Admin sees the English banner while restore mode is on, and not after', async ({ page }) => {
  const violations = await trackViolations(page)
  await signIn(page, 'admin')
  await expect(page.getByText('Restore mode is on', { exact: false })).toHaveCount(0)
  await setRestoreMode(page, true)
  await page.goto('/?lang=en')
  const banner = page.getByRole('status').filter({ hasText: 'Restore mode is on' })
  await expect(banner).toBeVisible()
  await expect(banner).toContainText('No email is sent')
  await setRestoreMode(page, false)
  await page.goto('/?lang=en')
  await expect(page.getByText('Restore mode is on', { exact: false })).toHaveCount(0)
  expect(await violations()).toEqual([])
})

test('the banner is in German for a German page', async ({ page }) => {
  const violations = await trackViolations(page)
  await signIn(page, 'admin')
  await setRestoreMode(page, true)
  await page.goto('/?lang=de')
  const banner = page.getByRole('status').filter({ hasText: 'Wiederherstellungsmodus' })
  await expect(banner).toBeVisible()
  await expect(banner).toContainText('keine E-Mails')
  expect(await violations()).toEqual([])
})

test('a requester never sees the Admin banner', async ({ page }) => {
  await signIn(page, 'requester')
  await setRestoreMode(page, true)
  await page.goto('/?lang=en')
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible()
  await expect(page.getByText('Restore mode is on', { exact: false })).toHaveCount(0)
})
