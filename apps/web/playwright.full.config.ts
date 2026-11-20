import { defineConfig, devices } from '@playwright/test'

const externalOrigin = process.env.WORKFLOW_BASE_URL
if (externalOrigin && !process.env.WORKFLOW_COMPOSE_ENV) {
  throw new Error('Use ops/compose/workflows.sh for a disposable external stack with isolated provider traffic')
}
const baseURL = externalOrigin ? new URL(externalOrigin).origin : 'http://127.0.0.1:18500'

export default defineConfig({
  testDir: 'e2e-full',
  outputDir: 'test-results-full',
  reporter: [['list'], ['html', { open: 'never', outputFolder: 'playwright-report-full' }]],
  forbidOnly: !!process.env.CI,
  // Organization policy and first-admin setup are shared within one deployment.
  workers: 1,
  timeout: 240_000,
  use: {
    ...devices['Desktop Chrome'],
    baseURL,
    // The Compose launcher verifies Caddy's private CA before starting Chromium.
    ignoreHTTPSErrors: !!process.env.WORKFLOW_COMPOSE_ENV,
    actionTimeout: 8_000,
    navigationTimeout: 15_000,
    trace: 'retain-on-failure',
    video: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer: externalOrigin ? undefined : {
    command: 'node e2e-full/serve.mjs',
    // Playwright considers a 503 unavailable, so waits for both migrations and the API.
    url: `${baseURL}/api/ready`,
    timeout: 180_000,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 15_000 },
  },
})
