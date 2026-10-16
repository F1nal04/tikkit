import postgres from 'postgres'

/**
 * Per-run database isolation for browser tests.
 *
 * playwright.config.ts picks one database name per run and stores it in
 * `process.env` so the runner, its workers, the web servers, and the global
 * teardown all agree on it. The database is created right before the API
 * starts (as part of the API webServer command, because Playwright launches
 * webServer before globalSetup) and dropped by globalTeardown.
 */

export const DEFAULT_ADMIN_URL =
  'postgres://tikkit:tikkit@127.0.0.1:5432/postgres'

/** Internal: the database name chosen for the current run. */
export const RUN_DATABASE_ENV = 'TIKKIT_E2E_DATABASE_NAME'

const NAME_PATTERN = /^tikkit_e2e_[a-z0-9_]+$/

export function adminUrl(): string {
  return process.env.E2E_DATABASE_URL || DEFAULT_ADMIN_URL
}

export function generateDatabaseName(): string {
  const random = Math.random().toString(36).slice(2, 10)
  return `tikkit_e2e_${Date.now()}_${random}`
}

function assertSafeName(name: string): void {
  // Only ever touch databases this tooling named.
  if (!NAME_PATTERN.test(name)) {
    throw new Error(`Refusing to manage unexpected database name "${name}"`)
  }
}

/** Connection URL for `name` on the same server as the admin URL. */
export function databaseUrl(name: string, base = adminUrl()): string {
  assertSafeName(name)
  const url = new URL(base)
  url.pathname = `/${name}`
  return url.toString()
}

function connectAdmin() {
  return postgres(adminUrl(), { max: 1, onnotice: () => {} })
}

export async function createDatabase(name: string): Promise<void> {
  assertSafeName(name)
  const sql = connectAdmin()
  try {
    await sql`CREATE DATABASE ${sql(name)}`
  } finally {
    await sql.end({ timeout: 5 })
  }
}

export async function dropDatabase(name: string): Promise<void> {
  assertSafeName(name)
  const sql = connectAdmin()
  try {
    await sql`DROP DATABASE IF EXISTS ${sql(name)} WITH (FORCE)`
  } finally {
    await sql.end({ timeout: 5 })
  }
}

/**
 * Drops databases left behind by runs that crashed before global teardown
 * (for example when a web server failed to start). Only names this tooling
 * generated and whose embedded timestamp is older than `maxAgeMs` qualify.
 */
export async function dropStaleDatabases(
  maxAgeMs = 6 * 60 * 60 * 1000,
): Promise<string[]> {
  const sql = connectAdmin()
  try {
    const rows = await sql<{ datname: string }[]>`
      SELECT datname FROM pg_database WHERE datname LIKE 'tikkit\_e2e\_%'
    `
    const cutoff = Date.now() - maxAgeMs
    const stale = rows
      .map((row) => row.datname)
      .filter((name) => {
        const match = /^tikkit_e2e_(\d+)_[a-z0-9]+$/.exec(name)
        return match !== null && Number(match[1]) < cutoff
      })
    for (const name of stale) {
      await sql`DROP DATABASE IF EXISTS ${sql(name)} WITH (FORCE)`
    }
    return stale
  } finally {
    await sql.end({ timeout: 5 })
  }
}

export function runDatabaseName(): string {
  const name = process.env[RUN_DATABASE_ENV]
  if (!name) throw new Error(`${RUN_DATABASE_ENV} is not set`)
  return name
}
