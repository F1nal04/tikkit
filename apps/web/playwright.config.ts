import { fileURLToPath } from 'node:url'
import { defineConfig, devices } from '@playwright/test'
import {
  databaseUrl,
  generateDatabaseName,
  RUN_DATABASE_ENV,
} from './e2e/support/database.ts'

const isCI = !!process.env.CI
const repoRoot = fileURLToPath(new URL('../..', import.meta.url))

// One disposable database per run. The config is evaluated by the runner and
// again by every worker; workers inherit the runner's environment, so the
// name is generated once and shared. Playwright starts webServer *before*
// globalSetup, so the database is created by the API webServer command
// (e2e/support/create-database.ts) and dropped in globalTeardown.
const runDatabase = (process.env[RUN_DATABASE_ENV] ??= generateDatabaseName())

const webUrl = 'http://localhost:3000'
const apiAddr = '127.0.0.1:3001'
const chromiumPath = process.env.PW_CHROMIUM_PATH

export default defineConfig({
  testDir: './e2e',
  outputDir: './test-results',
  fullyParallel: true,
  forbidOnly: isCI,
  retries: isCI ? 1 : 0,
  workers: isCI ? 1 : undefined,
  reporter: [
    ['list'],
    ['html', { outputFolder: './playwright-report', open: 'never' }],
  ],
  globalTeardown: './e2e/support/global-teardown.ts',
  use: {
    baseURL: webUrl,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        ...(chromiumPath
          ? { launchOptions: { executablePath: chromiumPath } }
          : {}),
      },
    },
  ],
  webServer: [
    {
      name: 'api',
      command:
        'bun apps/web/e2e/support/create-database.ts && cargo run --locked -p tikkit-api',
      cwd: repoRoot,
      url: `http://${apiAddr}/api/health`,
      env: {
        API_ADDR: apiAddr,
        DATABASE_URL: databaseUrl(runDatabase),
        TIKKIT_ENV: 'development',
        [RUN_DATABASE_ENV]: runDatabase,
      },
      reuseExistingServer: !isCI,
      // The first run compiles the API from scratch.
      timeout: 300_000,
      stdout: 'pipe',
      stderr: 'pipe',
    },
    {
      name: 'web',
      command: 'bun run --cwd apps/web dev',
      cwd: repoRoot,
      url: webUrl,
      reuseExistingServer: !isCI,
      timeout: 120_000,
    },
  ],
})
