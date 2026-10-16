// Generates apps/web/src/api/schema.gen.ts from apps/api/openapi.json.
//
//   node tools/api-codegen/generate.mjs           # write the file
//   node tools/api-codegen/generate.mjs --check   # fail if it is stale
//
// Run through `bun run generate:api` / `bun run check:api` in apps/web, which
// install this directory's pinned dependencies first.
import { readFile, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import openapiTS, { astToString } from 'openapi-typescript'

const specUrl = new URL('../../../api/openapi.json', import.meta.url)
const outUrl = new URL('../../src/api/schema.gen.ts', import.meta.url)
const check = process.argv.includes('--check')

const header = `// Generated from apps/api/openapi.json by openapi-typescript. Do not edit.
// Regenerate: bun run --cwd apps/web generate:api
`

const ast = await openapiTS(JSON.parse(await readFile(specUrl, 'utf8')), {
  alphabetize: true,
})
const generated = `${header}\n${astToString(ast)}`
const outPath = fileURLToPath(outUrl)

if (check) {
  const committed = await readFile(outPath, 'utf8').catch(() => '')
  if (committed !== generated) {
    console.error(
      `${outPath} is out of date with apps/api/openapi.json.\n` +
        'Run `bun run --cwd apps/web generate:api` and commit the result.',
    )
    process.exit(1)
  }
  console.log('API client schema is up to date.')
} else {
  await writeFile(outPath, generated)
  console.log(`Wrote ${outPath}`)
}
