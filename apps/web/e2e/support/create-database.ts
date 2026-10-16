// Run by the API webServer command before the API starts:
// creates this run's disposable database (see database.ts).
import {
  createDatabase,
  dropStaleDatabases,
  runDatabaseName,
} from './database.ts'

const stale = await dropStaleDatabases()
for (const name of stale) console.log(`[e2e] dropped stale database ${name}`)

const name = runDatabaseName()
await createDatabase(name)
console.log(`[e2e] created database ${name}`)
