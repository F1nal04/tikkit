#!/usr/bin/env bash
# Check the complete browser JavaScript output (excluding server bundles).
set -euo pipefail
if [ "$#" != 1 ]; then
  echo "usage: $0 WEB_IMAGE" >&2
  exit 2
fi
docker run --rm --network none --read-only --entrypoint node "$1" -e '
  const { readdirSync, readFileSync } = require("node:fs")
  const { join } = require("node:path")
  const { gzipSync } = require("node:zlib")
  const root = "/app/apps/web/build/client"
  const scripts = readdirSync(root, { recursive: true }).filter(file => file.endsWith(".js"))
  if (!scripts.length) throw new Error("Missing client JavaScript")
  const sizes = scripts.map(file => readFileSync(join(root, file)))
  const gzip = sizes.reduce((sum, file) => sum + gzipSync(file, { level: 9 }).length, 0)
  const raw = sizes.reduce((sum, file) => sum + file.length, 0)
  const budget = 200 * 1024
  console.log(JSON.stringify({ files: scripts.length, raw_bytes: raw, gzip_bytes: gzip, budget_bytes: budget }))
  if (gzip > budget) process.exitCode = 1
'
