import { expect, type Page, test } from '@playwright/test'
import { PASSWORD, proxyApi, signIn } from './support'

// Runs at desktop and phone size. On a phone the navigation sits behind the Menu button.
async function openMenu(page: Page) {
  const menu = page.getByRole('button', { name: /^(Menu|Menü)$/ })
  if (await menu.isVisible()) await menu.click()
}
const navLinks = (page: Page) => page.getByRole('navigation').getByRole('link')

test('a requester signs in and sees only requester navigation', async ({ page }) => {
  await signIn(page, 'requester')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('My tickets')
  await openMenu(page)
  await expect(navLinks(page)).toHaveText(['My tickets', 'New ticket'])
  await expect(page.getByText('Signed in as')).toContainText('requester')
  await expect(page.getByText('Role:')).toContainText('User')
  await page.getByRole('link', { name: 'New ticket' }).click()
  await expect(page).toHaveURL('/tickets/new')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('New ticket')
})

test('Operator and Specialist share the help-desk workspace', async ({ page }) => {
  await signIn(page, 'specialist')
  await openMenu(page)
  await expect(navLinks(page)).toHaveText(['My tickets', 'New ticket', 'General queue'])
  await expect(page.getByText('Role:')).toContainText('Specialist')
  await page.getByRole('link', { name: 'General queue' }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('General queue')
  await openMenu(page)
  await expect(page.getByRole('link', { name: 'General queue' })).toHaveAttribute('aria-current', 'page')
})

test('a Team Lead gets team tools, an Admin gets administration', async ({ page }) => {
  await signIn(page, 'lead')
  await openMenu(page)
  await expect(navLinks(page)).toHaveText(['My tickets', 'New ticket', 'General queue', 'My team'])
  await page.getByRole('button', { name: 'Sign out' }).click()
  await expect(page).toHaveURL('/login')

  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Password').fill('correct horse battery')
  await page.getByRole('button', { name: 'Sign in' }).click()
  await expect(page).not.toHaveURL(/\/login/)
  await openMenu(page)
  await expect(navLinks(page)).toHaveText(['My tickets', 'New ticket', 'General queue', 'Overview', 'Accounts'])
  await page.getByRole('link', { name: 'Accounts' }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Accounts')
  await expect(page.getByRole('table', { name: 'Accounts' }).getByRole('cell', { name: 'operator', exact: true })).toBeVisible()
})

test('denied pages fail safely inside the shell without leaking content', async ({ page }) => {
  await signIn(page, 'requester')
  for (const path of ['/admin/accounts', '/queue', '/team']) {
    const response = await page.goto(path)
    expect(response?.status()).toBe(403)
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
    await expect(page.getByRole('table')).toHaveCount(0)
    await expect(page.getByRole('main')).not.toContainText('Accounts')
  }
  const missing = await page.goto('/nowhere')
  expect(missing?.status()).toBe(404)
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Page not found')
  // The shell stays usable from the error page.
  await openMenu(page)
  await expect(navLinks(page)).toHaveText(['My tickets', 'New ticket'])
})

test('signing in returns to the page that was asked for', async ({ page }) => {
  await signIn(page, 'operator')
  await openMenu(page)
  await page.getByRole('button', { name: 'Sign out' }).click()
  await expect(page).toHaveURL('/login')
  await page.goto('/queue')
  await expect(page).toHaveURL('/login?next=%2Fqueue')
  await page.getByLabel('Username').fill('operator')
  await page.getByLabel('Password').fill('correct horse battery')
  await page.getByRole('button', { name: 'Sign in' }).click()
  await expect(page).toHaveURL('/queue')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('General queue')
})

test('language can be chosen before sign-in without losing typed input, and persists for the account', async ({ page }) => {
  await proxyApi(page)
  await page.goto('/login')
  await page.getByLabel('Username').fill('admin')
  await page.getByLabel('Language').selectOption('de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Anmelden')
  await expect(page.getByLabel('Benutzername')).toHaveValue('admin')

  await page.getByLabel('Passwort').fill(PASSWORD)
  await page.getByRole('button', { name: 'Anmelden' }).click()
  // The account's own preference (English until changed) applies once signed in.
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('My tickets')
  await openMenu(page)
  await page.getByLabel('Language').selectOption('de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Meine Tickets')
  await expect(page.getByText('Rolle:')).toContainText('Administrator')

  // Stored on the account, not only in a cookie.
  await page.context().clearCookies({ name: 'lang' })
  await page.reload()
  await expect(page.locator('html')).toHaveAttribute('lang', 'de')
})

test('an expired session keeps unsent input and offers a way back to sign-in', async ({ page }) => {
  await signIn(page, 'requester')
  await page.goto('/profile')
  await page.getByLabel('Email').fill('new-address@example.com')
  await page.evaluate(() => fetch('/api/test/expire', { method: 'POST' }))
  await page.getByRole('button', { name: 'Save' }).click()
  const banner = page.getByRole('alert').filter({ has: page.getByRole('link', { name: /Sign in/ }) })
  await expect(banner).toContainText('Your session has expired.')
  const signInAgain = banner.getByRole('link', { name: /Sign in/ })
  await expect(signInAgain).toHaveAttribute('href', '/login?next=%2Fprofile')
  await expect(signInAgain).toHaveAttribute('target', '_blank')
  await expect(page.getByLabel('Email')).toHaveValue('new-address@example.com')
})

test('keyboard users can skip straight to the content', async ({ page }) => {
  await signIn(page, 'requester')
  await page.goto('/')
  await page.keyboard.press('Tab')
  const skip = page.getByRole('link', { name: 'Skip to main content' })
  await expect(skip).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('main')).toBeFocused()
})
