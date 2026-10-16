import { expect, test } from '@playwright/test'
import { DOWN } from '../playwright.config'

test('server-renders the landing page before any JavaScript runs', async ({ request }) => {
  const response = await request.get('/', { headers: { 'accept-language': 'en' } })
  expect(response.headers()['cache-control']).toBe('private, no-store')
  const html = await response.text()
  expect(html).toContain('<html lang="en">')
  expect(html).toContain('A clean starting point.')
  expect(html).toContain('Rust API connected')
})

test('renders German from ?lang=de and keeps it across client navigation', async ({ page }) => {
  await page.goto('/?lang=de')
  await expect(page.locator('html')).toHaveAttribute('lang', 'de')
  await expect(page.getByRole('status')).toHaveText('Rust-API verbunden')

  // A marker on window survives only if the router navigates without a full reload.
  await page.evaluate(() => Object.assign(window, { hydrated: true }))
  await page.getByRole('link', { name: 'Über Tikkit' }).click()
  await expect(page).toHaveURL('/about')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Über Tikkit')
  expect(await page.evaluate(() => 'hydrated' in window)).toBe(true)
})

test('uses Accept-Language when no language was chosen', async ({ browser }) => {
  const context = await browser.newContext({ locale: 'de-DE' })
  const page = await context.newPage()
  await page.goto('/')
  await expect(page.getByText('Ein sauberer Ausgangspunkt.')).toBeVisible()
  await context.close()
})

test('deep links survive a refresh', async ({ page }) => {
  await page.goto('/about')
  await page.reload()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('About Tikkit')
  await page.getByRole('link', { name: 'Home' }).click()
  await expect(page).toHaveURL('/')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Tikkit')
})

test('shows the unavailable state when the API is down', async ({ page }) => {
  await page.goto(`${DOWN}/`)
  await expect(page.getByRole('status')).toHaveText('Rust API unavailable')
})
