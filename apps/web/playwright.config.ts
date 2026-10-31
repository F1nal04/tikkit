import { defineConfig, devices } from '@playwright/test'

// Serves the production build (`bun run build` first; `bun run test:e2e` does it).
// The app on WEB talks to a stub API; the one on DOWN points at a closed port.
export const WEB = 'http://127.0.0.1:4173'
export const DOWN = 'http://127.0.0.1:4174'
export const STUB = 'http://127.0.0.1:4175'

const web = (url: string, apiOrigin: string) => ({
  command: 'node build/index.js',
  url,
  env: { HOST: '127.0.0.1', PORT: new URL(url).port, API_ORIGIN: apiOrigin },
  reuseExistingServer: !process.env.CI,
})

export default defineConfig({
  testDir: 'e2e',
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: WEB,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    // Layout and navigation at phone size.
    { name: 'mobile', use: { ...devices['Pixel 7'] }, testMatch: 'shell.spec.ts' },
  ],
  webServer: [
    {
      command: 'node e2e/stub-api.mjs',
      url: `${STUB}/api/health`,
      env: { PORT: new URL(STUB).port },
      reuseExistingServer: !process.env.CI,
    },
    web(WEB, STUB),
    web(DOWN, 'http://127.0.0.1:9'),
  ],
})
