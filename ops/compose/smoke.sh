#!/usr/bin/env bash
# Run against a clean stack using the temporary Caddy CA (see docs/deployment.md).
set -euo pipefail

origin=${1:?usage: smoke.sh https://host:port caddy-root.crt}
ca=${2:?pass the Caddy root CA}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
probe() {
  curl --silent --show-error --fail --retry 20 --retry-connrefused --retry-delay 1 \
    --max-time 5 --cacert "$ca" "$@"
}

probe "$origin/api/health" | jq -e '.status == "ok"'
probe "$origin/api/ready" | jq -e '.status == "ready" and .checks.database == "ok" and .checks.migrations == "ok"'
probe -D "$scratch/headers" -H 'X-Request-ID: 00000000-0000-4000-8000-000000000046' \
  "$origin/api/setup" | jq -e '.setup_required == true'
tr -d '\r' < "$scratch/headers" | grep -Eiq '^x-request-id: [0-9a-f-]{36}$'
if grep -qi '00000000-0000-4000-8000-000000000046' "$scratch/headers"; then
  echo 'Proxy accepted a client-supplied request ID' >&2
  exit 1
fi

probe "$origin/setup?lang=en" > "$scratch/page"
grep -Fq '<h1' "$scratch/page"
# Check a fingerprinted client module referenced by the SSR page.
asset=$(grep -Eo '/_app/immutable/[^"[:space:]<>]+\.js' "$scratch/page" | sed -n '1p')
test -n "$asset"
probe -o /dev/null "$origin$asset"

for path in /metrics /api/metrics; do
  test "$(curl --silent --show-error --max-time 5 --cacert "$ca" -o /dev/null -w '%{http_code}' "$origin$path")" = 404
done
