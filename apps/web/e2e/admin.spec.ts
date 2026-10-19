import { expect, test } from '@playwright/test'

for (const locale of ['en', 'de']) {
  test(`admin policy form preserves values after rejection (${locale}, mobile)`, async ({ page, context }) => {
    await context.addCookies([{ name: 'tikkit_session', value: 'e2e_admin', domain: '127.0.0.1', path: '/' }])
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(`/admin/settings?lang=${locale}`)
    await expect(page.getByRole('heading', { level: 1 })).toHaveText(locale === 'en' ? 'Organization settings' : 'Organisationseinstellungen')
    await expect(page.getByRole('link', { name: 'Example support' })).toBeVisible()
    await page.screenshot({ path: test.info().outputPath('settings.png'), fullPage: true })
    const name = page.getByLabel(locale === 'en' ? 'Organization name' : 'Organisationsname')
    await name.fill('Changed organization')
    await page.route('**/api/admin/settings', async (route) => {
      if (route.request().method() !== 'PUT') return route.continue()
      expect(route.request().postDataJSON().name).toBe('Changed organization')
      expect(route.request().headers()['x-requested-with']).toBe('tikkit')
      await route.fulfill({ status: 422, json: { error: { code: 'validation_failed', details: { fields: { timezone: ['invalid'] } } }, request_id: 'e2e' } })
    })
    await page.getByRole('button', { name: locale === 'en' ? 'Save' : 'Speichern', exact: true }).click()
    await expect(page.getByRole('alert')).toBeVisible()
    await expect(name).toHaveValue('Changed organization')
    await expect(page.getByLabel(locale === 'en' ? 'Timezone' : 'Zeitzone')).toHaveAttribute('aria-invalid', 'true')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  })
}

test('account administration distinguishes shared-email accounts and offers all five invitation roles', async ({ page, context }) => {
  await context.addCookies([{ name: 'tikkit_session', value: 'e2e_admin', domain: '127.0.0.1', path: '/' }])
  await page.goto('/admin/accounts')
  const role = page.locator('select[name="role"]')
  await expect(role).toHaveValue('user')
  expect(await role.locator('option').allTextContents()).toEqual(['User', 'Operator', 'Specialist', 'Team Lead', 'Admin'])
  await expect(page.getByRole('article', { name: 'admin', exact: true })).toBeVisible()
  await expect(page.getByRole('article', { name: 'operator', exact: true })).toBeVisible()
  await page.getByText('Edit account: admin', { exact: true }).click()
  await expect(page.getByRole('article', { name: 'admin', exact: true }).getByLabel('Team', { exact: true })).toHaveCount(0)
})

test('a non-admin deep link does not render management content', async ({ page, context }) => {
  await context.addCookies([{ name: 'tikkit_session', value: 'e2e_user', domain: '127.0.0.1', path: '/' }])
  const response = await page.goto('/admin/settings')
  expect(response?.status()).toBe(403)
  await expect(page.getByLabel('Organization name')).toHaveCount(0)
})

test('an Admin creates a role-specific invitation and receives the link once', async ({ page, context }) => {
  await context.addCookies([{ name: 'tikkit_session', value: 'e2e_admin', domain: '127.0.0.1', path: '/' }])
  await page.goto('/admin/accounts')
  await page.route('**/api/**', async (route) => {
    if (route.request().method() === 'GET') return route.fulfill({ json: { items: [], next_cursor: null } })
    expect(new URL(route.request().url()).pathname).toBe('/api/invitations')
    expect(route.request().postDataJSON()).toMatchObject({ role: 'specialist', language: 'en', expires_in_hours: 168 })
    await route.fulfill({ status: 201, json: { url: 'http://localhost/invite/new-single-use-token', emailed: false, invitation: {} } })
  })
  await page.locator('select[name="role"]').selectOption('specialist')
  await page.getByRole('button', { name: 'Create invitation' }).click()
  await expect(page.getByLabel('Invitation link (shown once)')).toHaveValue('http://localhost/invite/new-single-use-token')
  await expect(page.getByText('No email was sent. Copy the link and share it yourself.')).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
})
