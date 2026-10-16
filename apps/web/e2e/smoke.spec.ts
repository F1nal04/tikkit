import { expect, test } from '@playwright/test'

test('landing page renders and reports the API as healthy', async ({
  page,
}) => {
  await page.goto('/')

  await expect(page).toHaveTitle('Tikkit')
  await expect(page.getByRole('heading', { name: 'Tikkit' })).toBeVisible()

  // The browser reaches the Rust API through the Vite /api proxy.
  const status = page.getByRole('status')
  await expect(status).toHaveText('Rust API connected')
  await expect(status).toHaveAttribute('data-health', 'connected')
})

test('API health endpoint is reachable through the web origin', async ({
  request,
}) => {
  const response = await request.get('/api/health')
  expect(response.ok()).toBe(true)
  expect(await response.json()).toEqual({ status: 'ok' })
})
