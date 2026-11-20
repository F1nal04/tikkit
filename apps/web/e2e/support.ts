import type { Page } from '@playwright/test'
import { expect } from '@playwright/test'
import { STUB } from '../playwright.config'

export const PASSWORD = 'correct horse battery'

/**
 * Browser fetches to `/api` go to the stub API, as a reverse proxy would in production
 * (the test web server does not proxy). Cookies travel in both directions.
 */
export async function proxyApi(page: Page) {
  await page.route('**/api/**', async (route) => {
    const { pathname, search } = new URL(route.request().url())
    try {
      const response = await route.fetch({
        url: `${STUB}${pathname}${search}`,
        headers: await route.request().allHeaders(),
      })
      await route.fulfill({ response })
    } catch {
      // A background poll still in flight when the test ends would otherwise fail it.
      // Anything else surfaces in the page as a failed request.
      await route.abort().catch(() => {})
    }
  })
}

export async function signIn(page: Page, username: string, language = 'English') {
  await proxyApi(page)
  await page.goto('/login')
  await page.getByLabel(language === 'English' ? 'Username' : 'Benutzername').fill(username)
  await page.getByLabel(language === 'English' ? 'Password' : 'Passwort').fill(PASSWORD)
  await page.getByRole('button', { name: language === 'English' ? 'Sign in' : 'Anmelden' }).click()
  // On a phone the navigation is collapsed, so wait for the page to leave sign-in instead.
  await expect(page).not.toHaveURL(/\/login/)
  await expect(page.getByRole('main')).toBeVisible()
}
