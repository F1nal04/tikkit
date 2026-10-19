import { expect, test } from '@playwright/test'
import { DOWN } from '../playwright.config'

test('server-renders the sign-in page before any JavaScript runs', async ({ request }) => {
  const response = await request.get('/login', { headers: { 'accept-language': 'en' } })
  expect(response.headers()['cache-control']).toBe('private, no-store')
  const html = await response.text()
  expect(html).toContain('<html lang="en">')
  expect(html).toContain('<h1>Sign in</h1>')
  // No implementation details in the product UI.
  expect(html).not.toMatch(/SvelteKit|Rust/)
})

test('signed-out visitors are sent to sign-in and keep their destination', async ({ request }) => {
  const response = await request.get('/queue', { maxRedirects: 0 })
  expect(response.status()).toBe(303)
  expect(response.headers().location).toBe('/login?next=%2Fqueue')
  const home = await request.get('/', { maxRedirects: 0 })
  expect(home.headers().location).toBe('/login')
})

test('renders German from ?lang=de and keeps it across client navigation', async ({ page }) => {
  await page.goto('/login?lang=de')
  await expect(page.locator('html')).toHaveAttribute('lang', 'de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Anmelden')

  // A marker on window survives only if the router navigates without a full reload.
  await page.evaluate(() => Object.assign(window, { hydrated: true }))
  await page.getByRole('link', { name: 'Passwort vergessen?' }).click()
  await expect(page).toHaveURL('/forgot-password')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Passwort zurücksetzen')
  expect(await page.evaluate(() => 'hydrated' in window)).toBe(true)
})

test('uses Accept-Language when no language was chosen', async ({ browser }) => {
  const context = await browser.newContext({ locale: 'de-DE' })
  const page = await context.newPage()
  await page.goto('/login')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Anmelden')
  await context.close()
})

test('deep links survive a refresh', async ({ page }) => {
  await page.goto('/forgot-password')
  await page.reload()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Reset your password')
  await page.getByRole('link', { name: 'Back to sign in' }).click()
  await expect(page).toHaveURL('/login')
})

test('says the service is unavailable when the API is down', async ({ page }) => {
  await page.goto(`${DOWN}/login`)
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password').fill('whatever')
  await page.getByRole('button', { name: 'Sign in' }).click()
  await expect(page.getByRole('alert')).toHaveText('The service is unavailable. Please try again later.')
  await expect(page.getByLabel('Username')).toHaveValue('admin')
})
