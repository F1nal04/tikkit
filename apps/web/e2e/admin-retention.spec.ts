import { readFile } from 'node:fs/promises'
import { expect, test } from '@playwright/test'
import { signIn } from './support'

test('an Admin sets periods, previews the deletion, and confirms a run by typing', async ({ page }) => {
  await signIn(page, 'admin')
  await page.getByRole('link', { name: 'Data retention' }).click()
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Data retention')
  await expect(page.getByRole('link', { name: 'Data retention' })).toHaveAttribute('aria-current', 'page')

  // Conservative defaults: every period empty, nothing to delete, no run offered.
  for (const label of ['Unsubmitted drafts', 'Screenshots', 'AI inputs and suggestions', 'Ticket content', 'Audit log entries']) {
    await expect(page.getByLabel(label, { exact: true })).toHaveValue('')
  }
  await expect(page.getByLabel('Apply these periods automatically once a day')).not.toBeChecked()
  await expect(page.getByText('No retention period is set, so there is nothing to delete.')).toBeVisible()
  await expect(page.getByText('No deletion has run yet.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Delete now' })).toHaveCount(0)

  // Bounds are the server's: audit entries are kept at least 30 days.
  await page.getByLabel('Audit log entries', { exact: true }).fill('7')
  await page.getByRole('button', { name: 'Save periods' }).click()
  await expect(page.getByText('This value is outside the allowed range.')).toBeVisible()
  await page.getByLabel('Audit log entries', { exact: true }).fill('')

  // A dry run of unsaved periods: exact counts, nothing changed.
  await page.getByLabel('Unsubmitted drafts', { exact: true }).fill('30')
  await page.getByLabel('Ticket content', { exact: true }).fill('365')
  await page.getByRole('button', { name: 'Preview deletion' }).click()
  const preview = page.getByTestId('preview')
  await expect(preview.locator('tr[data-count="drafts"] td')).toHaveText('4')
  await expect(preview.locator('tr[data-count="screenshots"] td')).toHaveText('7')
  await expect(preview.locator('tr[data-count="ai_content"] td')).toHaveText('2')
  await expect(preview.locator('tr[data-count="ticket_content"] td')).toHaveText('5')
  await expect(preview.locator('tr[data-count="audit_events"] td')).toHaveText('0')
  await expect(page.getByText('Save or discard your changes first.')).toBeVisible()

  // Automatic deletion is saved only with the typed confirmation of a preview of these periods.
  await page.getByLabel('Apply these periods automatically once a day').check()
  const save = page.getByRole('button', { name: 'Save periods' })
  await expect(save).toBeDisabled()
  await page.getByLabel('Type DELETE to confirm').fill('delete')
  await expect(save).toBeDisabled()
  // Editing a period clears the preview, and with it the confirmation.
  await page.getByLabel('Unsubmitted drafts', { exact: true }).fill('31')
  await expect(preview).toHaveCount(0)
  await expect(page.getByTestId('needs-preview')).toBeVisible()
  await expect(page.getByLabel('Type DELETE to confirm')).toHaveCount(0)
  await expect(save).toBeDisabled()
  await page.getByLabel('Unsubmitted drafts', { exact: true }).fill('30')
  await expect(preview).toHaveCount(0)
  await page.getByLabel('Apply these periods automatically once a day').uncheck()
  await save.click()
  await expect(page.getByRole('status')).toHaveText('Retention periods saved.')

  // The run: bound to a preview of the saved periods, and off until DELETE is typed.
  const run = page.getByRole('button', { name: 'Delete now' })
  await expect(run).toHaveCount(0)
  await expect(page.getByText('Preview the deletion first.')).toBeVisible()
  await page.getByRole('button', { name: 'Preview deletion' }).click()
  await expect(preview.locator('tr[data-count="ticket_content"] td')).toHaveText('5')
  await expect(run).toBeDisabled()
  await page.getByLabel('Type DELETE to confirm').fill('DELETE')
  await run.click()
  await expect(page.getByRole('status')).toHaveText('Deletion run started.')
  await expect(page.getByText('A deletion run is in progress.')).toBeVisible()
  const row = page.locator('tr[data-run]')
  await expect(row).toContainText('In progress')
  await expect(row).toContainText('admin')
  await expect(run).toHaveCount(0)
  await page.getByRole('button', { name: 'Refresh' }).click()
  await expect(page.locator('tr[data-run="completed"]')).toContainText('Completed')
  await expect(page.locator('tr[data-run="completed"] td').nth(3)).toHaveText('4')
  await expect(page.locator('tr[data-run="completed"] td').nth(6)).toHaveText('5')
  // The saved periods survive a reload.
  await page.reload()
  await expect(page.getByLabel('Ticket content', { exact: true })).toHaveValue('365')
})

test('a confirmation in a stale tab deletes nothing: the policy another Admin saved was never previewed', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/retention')
  await page.getByLabel('Ticket content', { exact: true }).fill('365')
  await page.getByRole('button', { name: 'Save periods' }).click()
  await expect(page.getByRole('status')).toHaveText('Retention periods saved.')
  await page.getByRole('button', { name: 'Preview deletion' }).click()
  await expect(page.getByTestId('preview')).toBeVisible()

  // Meanwhile another Admin shortens the period to one day.
  await page.evaluate(() => fetch('/api/admin/retention/test-other-admin', { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify({ ticket_content_days: 1 }) }))

  await page.getByLabel('Type DELETE to confirm').fill('DELETE')
  await page.getByRole('button', { name: 'Delete now' }).click()
  await expect(page.getByRole('alert')).toContainText('The retention periods were changed after you previewed them')
  await expect(page.getByRole('alert')).toContainText('Nothing was deleted or saved.')
  // No run started; the page shows what is stored now and asks for a new preview.
  await expect(page.locator('tr[data-run]')).toHaveCount(0)
  await expect(page.getByLabel('Ticket content', { exact: true })).toHaveValue('1')
  await expect(page.getByTestId('preview')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Delete now' })).toHaveCount(0)
  await expect(page.getByText('Preview the deletion first.')).toBeVisible()

  // The same for switching automatic deletion on from a stale preview.
  await page.getByRole('button', { name: 'Preview deletion' }).click()
  await page.getByLabel('Apply these periods automatically once a day').check()
  await page.getByLabel('Type DELETE to confirm').first().fill('DELETE')
  await page.evaluate(() => fetch('/api/admin/retention/test-other-admin', { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify({ ticket_content_days: 7 }) }))
  await page.getByRole('button', { name: 'Save periods' }).click()
  await expect(page.getByRole('alert')).toContainText('The retention periods were changed after you previewed them')
  await expect(page.getByLabel('Ticket content', { exact: true })).toHaveValue('7')
  await expect(page.getByLabel('Apply these periods automatically once a day')).not.toBeChecked()

  // In German too.
  await page.goto('/admin/retention?lang=de')
  await page.getByRole('button', { name: 'Löschvorschau' }).click()
  await page.evaluate(() => fetch('/api/admin/retention/test-other-admin', { method: 'POST', headers: { 'content-type': 'application/json', 'x-requested-with': 'tikkit' }, body: JSON.stringify({ ticket_content_days: 3 }) }))
  await page.getByLabel('Zur Bestätigung DELETE eingeben').fill('DELETE')
  await page.getByRole('button', { name: 'Jetzt löschen' }).click()
  await expect(page.getByRole('alert')).toContainText('Die Aufbewahrungsfristen wurden nach Ihrer Vorschau geändert')
})

test('an Admin exports and anonymizes one account after typing its username', async ({ page }) => {
  await signIn(page, 'admin')
  await page.goto('/admin/retention')
  await page.getByLabel('Username', { exact: true }).fill('nobody')
  await page.getByRole('button', { name: 'Find account' }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(page.getByTestId('account')).toHaveCount(0)

  // An active account can be exported but not anonymized.
  await page.getByLabel('Username', { exact: true }).fill('Requester')
  await page.getByRole('button', { name: 'Find account' }).click()
  const account = page.getByTestId('account')
  await expect(account).toContainText('requester (User, active)')
  await expect(account).toContainText('Only a deactivated account can be anonymized.')
  await expect(page.getByRole('button', { name: 'Remove identity permanently' })).toHaveCount(0)
  const exportButton = page.getByRole('button', { name: 'Export personal data' })
  await expect(exportButton).toBeDisabled()
  await page.getByLabel('Type the username to confirm').fill('someone-else')
  await exportButton.click()
  await expect(page.getByText('This is not the username of this account.')).toBeVisible()
  await page.getByLabel('Type the username to confirm').fill('requester')
  const download = page.waitForEvent('download')
  await exportButton.click()
  const file = await download
  expect(file.suggestedFilename()).toBe('personal-data-40000000-0000-4000-8000-000000000001.json')
  const document = JSON.parse(await readFile(await file.path(), 'utf8'))
  expect(document.format).toBe('tikkit.personal_data.v1')
  expect(document.account.username).toBe('requester')
  await expect(page.getByRole('status')).toHaveText('Personal data exported.')

  // A deactivated account: identity removal needs its username typed.
  await page.getByLabel('Username', { exact: true }).fill('former.user')
  await page.getByRole('button', { name: 'Find account' }).click()
  await expect(account).toContainText('former.user (Operator, deactivated)')
  const remove = page.getByRole('button', { name: 'Remove identity permanently' })
  await expect(remove).toBeDisabled()
  await page.getByLabel('Type the username to confirm').fill('former')
  await remove.click()
  await expect(page.getByText('This is not the username of this account.')).toBeVisible()
  await page.getByLabel('Type the username to confirm').fill('former.user')
  await page.getByLabel("Also remove the content of this account's finished tickets now").check()
  await remove.click()
  await expect(page.getByRole('status')).toHaveText('The identity was removed.')
  await expect(page.getByTestId('anonymized')).toHaveText('The account is now called anonymized-40000000000040008000000000000009.')
  // The old name no longer finds anyone.
  await page.getByRole('button', { name: 'Find account' }).click()
  await expect(page.getByTestId('account')).toHaveCount(0)
  await page.getByLabel('Username', { exact: true }).fill('anonymized-40000000000040008000000000000009')
  await page.getByRole('button', { name: 'Find account' }).click()
  await expect(account).toContainText('This account has already been anonymized.')
})

test('German retention page at phone width', async ({ page }) => {
  await signIn(page, 'admin')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/admin/retention?lang=de')
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Datenaufbewahrung')
  await page.getByLabel('Ticketinhalte', { exact: true }).fill('90')
  await page.getByRole('button', { name: 'Löschvorschau' }).click()
  await expect(page.getByTestId('preview').locator('tr[data-count="ticket_content"]')).toContainText('Tickets (Inhalt)')
  await page.getByRole('button', { name: 'Fristen speichern' }).click()
  await expect(page.getByRole('status')).toHaveText('Aufbewahrungsfristen gespeichert.')
  await expect(page.getByText('Sehen Sie sich zuerst die Löschvorschau an.')).toBeVisible()
  await page.getByRole('button', { name: 'Löschvorschau' }).click()
  await expect(page.getByLabel('Zur Bestätigung DELETE eingeben')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Jetzt löschen' })).toBeDisabled()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

for (const who of ['requester', 'operator', 'lead']) {
  test(`${who} gets neither the retention page nor its API`, async ({ page }) => {
    await signIn(page, who)
    await expect(page.getByRole('link', { name: 'Data retention' })).toHaveCount(0)
    const response = await page.goto('/admin/retention')
    expect(response?.status()).toBe(403)
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Access denied')
    const statuses = await page.evaluate(async () => {
      const headers = { 'x-requested-with': 'tikkit', 'content-type': 'application/json' }
      const post = (path: string, body: unknown) => fetch(path, { method: 'POST', headers, body: JSON.stringify(body) }).then((r) => r.status)
      const id = '40000000-0000-4000-8000-000000000001'
      return [
        (await fetch('/api/admin/retention')).status,
        await post('/api/admin/retention/preview', {}),
        await post('/api/admin/retention/runs', { confirm: 'DELETE', preview_token: 'x', as_of: '2026-11-18T09:00:00Z' }),
        (await fetch('/api/admin/personal-data?username=requester')).status,
        await post(`/api/admin/accounts/${id}/personal-data/export`, { username: 'requester' }),
        await post(`/api/admin/accounts/${id}/anonymize`, { username: 'requester' }),
      ]
    })
    expect(statuses).toEqual([403, 403, 403, 403, 403, 403])
  })
}
