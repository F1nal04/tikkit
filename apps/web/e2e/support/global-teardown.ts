import { dropDatabase, RUN_DATABASE_ENV } from './database.ts'

export default async function globalTeardown(): Promise<void> {
  const name = process.env[RUN_DATABASE_ENV]
  if (!name) return
  // WITH (FORCE) terminates the API's open connections; the API process is
  // stopped by Playwright only after global teardown.
  await dropDatabase(name)
  console.log(`[e2e] dropped database ${name}`)
}
