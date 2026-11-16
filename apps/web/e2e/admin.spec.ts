import { expect, test } from '@playwright/test'
import { signIn } from './support'

for (const locale of ['en', 'de']) {
  test(`admin policy form preserves values after rejection (${locale}, mobile)`, async ({ page }) => {
    await signIn(page, 'admin')
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(`/admin/settings?lang=${locale}`)
    await expect(page.getByRole('heading', { level: 1 })).toHaveText(locale === 'en' ? 'Organization settings' : 'Organisationseinstellungen')
    await expect(page.getByRole('link', { name: 'Example support' })).toBeVisible()
    await page.screenshot({ path: test.info().outputPath('settings.png'), fullPage: true })
    const name = page.getByLabel(locale === 'en' ? 'Organization name' : 'Organisationsname')
    await name.fill('Changed organization')
    await page.route('**/api/admin/settings', async (route) => {
      if (route.request().method() !== 'PUT') return route.fallback()
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

test('account administration distinguishes shared-email accounts and offers all five invitation roles', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/accounts')
  const role = page.locator('select[name="role"]')
  await expect(role).toHaveValue('user')
  expect(await role.locator('option').allTextContents()).toEqual(['User', 'Operator', 'Specialist', 'Team Lead', 'Admin'])
  await expect(page.getByRole('article', { name: 'admin', exact: true })).toBeVisible()
  await expect(page.getByRole('article', { name: 'operator', exact: true })).toBeVisible()
  await page.getByText('Edit account: admin', { exact: true }).click()
  await expect(page.getByRole('article', { name: 'admin', exact: true }).getByLabel('Team', { exact: true })).toHaveCount(0)
})

test('a non-admin deep link does not render management content', async ({ page }) => {
  await signIn(page, 'requester')
  const response = await page.goto('/admin/settings')
  expect(response?.status()).toBe(403)
  await expect(page.getByLabel('Organization name')).toHaveCount(0)
})

test('an Admin creates a role-specific invitation and receives the link once', async ({ page }) => {
  await signIn(page, 'admin')
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

test('outbound email settings render in the admin shell and send credentials only when typed', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/email?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Ausgehende E-Mail')
  await expect(page.getByRole('link', { name: 'Ausgehende E-Mail' })).toHaveAttribute('aria-current', 'page')
  let sent: Record<string, unknown> = {}
  await page.route('**/api/settings/smtp', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback()
    sent = route.request().postDataJSON()
    await route.fulfill({ status: 422, json: { error: { code: 'validation_failed', details: { fields: { password: ['required'] } } }, request_id: 'e2e' } })
  })
  await page.getByLabel('Host').fill('smtp.example.com')
  await page.getByLabel('Benutzername').fill('mailer')
  await page.getByRole('button', { name: 'Speichern', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  expect(sent).toMatchObject({ host: 'smtp.example.com', username: 'mailer', security: 'starttls', port: 587 })
  expect(sent).not.toHaveProperty('password')
  await expect(page.getByLabel('Passwort')).toHaveAttribute('aria-invalid', 'true')
})

test('AI settings render in the admin shell, keep keys write-only, and bind a key to its provider', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/ai?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('KI-Unterstützung')
  await expect(page.getByRole('link', { name: 'KI-Unterstützung' })).toHaveAttribute('aria-current', 'page')
  await expect(page.getByText('Die KI-Unterstützung ist aus. Neue Tickets gehen direkt an den Helpdesk.')).toBeVisible()
  // Exactly the three first-release providers; stored keys show only masked metadata.
  await expect(page.getByLabel('Anbieter').locator('option')).toHaveText(['OpenAI', 'Google Gemini', 'Anthropic'])
  await expect(page.getByText('Schlüssel gespeichert, endet auf …WXYZ')).toBeVisible()
  for (const input of await page.locator('input[type="password"]').all()) await expect(input).toHaveValue('')

  const calls: { url: string; body: unknown }[] = []
  await page.route('**/api/settings/ai/credentials/*', async (route) => {
    calls.push({ url: new URL(route.request().url()).pathname, body: route.request().postDataJSON() })
    await route.fulfill({ status: 422, json: { error: { code: 'validation_failed', details: { fields: { api_key: ['invalid'] } } }, request_id: 'e2e' } })
  })
  await page.getByLabel('API-Schlüssel (Anthropic)').fill('sk-ant-e2e-key')
  await page.locator('form[data-provider="anthropic"]').getByRole('button', { name: 'Schlüssel speichern' }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  expect(calls).toEqual([{ url: '/api/settings/ai/credentials/anthropic', body: { api_key: 'sk-ant-e2e-key' } }])
  await expect(page.getByLabel('API-Schlüssel (Anthropic)')).toHaveAttribute('aria-invalid', 'true')
  await expect(page.getByLabel('API-Schlüssel (OpenAI)')).toHaveAttribute('aria-invalid', 'false')

  let sent: Record<string, unknown> = {}
  await page.route('**/api/settings/ai', async (route) => {
    if (route.request().method() !== 'PUT') return route.fallback()
    sent = route.request().postDataJSON()
    await route.fulfill({ status: 422, json: { error: { code: 'validation_failed', details: { fields: { enabled: ['credential_required'] } } }, request_id: 'e2e' } })
  })
  await page.getByLabel('Anbieter').selectOption('gemini')
  await page.getByLabel('Modell').fill('some-model')
  await page.getByLabel('KI-Unterstützung aktivieren').check()
  await page.getByRole('button', { name: 'Speichern', exact: true }).click()
  await expect(page.getByText('Speichern Sie zuerst einen API-Schlüssel für diesen Anbieter.')).toBeVisible()
  expect(sent).toEqual({ enabled: true, provider: 'gemini', model: 'some-model' })
})

test('AI settings are not reachable for non-admins', async ({ page }) => {
  await signIn(page, 'operator')
  await page.goto('/admin/ai')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
  await expect(page.getByLabel('Provider')).toHaveCount(0)
})
