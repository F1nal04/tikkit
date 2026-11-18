#!/usr/bin/env bash
# Run against a disposable stack with COMPOSE_FILE, COMPOSE_ENV_FILES and
# COMPOSE_PROJECT_NAME set. Pass the temporary Caddy CA.
set -euo pipefail

origin=${1:?usage: smoke.sh https://host:port caddy-root.crt}
ca=${2:?pass the Caddy root CA}
: "${COMPOSE_PROJECT_NAME:?set the disposable Compose project name}"
: "${COMPOSE_FILE:?set the production Compose file}"
: "${COMPOSE_ENV_FILES:?set the disposable Compose env file}"
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
tr -d '\r' < "$scratch/headers" > "$scratch/headers-lf"
grep -Eiq '^x-request-id: [0-9a-f-]{36}$' "$scratch/headers-lf"
grep -Fxiq 'strict-transport-security: max-age=31536000' "$scratch/headers-lf"
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

# Check both durable stores across a replacement of every container.
db() {
  docker compose exec -T postgres psql -U postgres -d tikkit -v ON_ERROR_STOP=1 -Atq "$@"
}
db -c "CREATE TABLE compose_smoke (marker text); INSERT INTO compose_smoke VALUES ('compose-smoke');"
docker compose exec -T api sh -ec 'printf "%s\n" compose-smoke > "$SCREENSHOT_DIR/.compose-smoke"'
docker compose up -d --wait --wait-timeout 120 --force-recreate
test "$(db -c 'SELECT marker FROM compose_smoke')" = compose-smoke
test "$(docker compose exec -T api sh -ec 'cat "$SCREENSHOT_DIR/.compose-smoke"')" = compose-smoke
db -c 'DROP TABLE compose_smoke'
docker compose exec -T api sh -ec 'rm "$SCREENSHOT_DIR/.compose-smoke"'

# Keep the API running to verify how readiness reports a newer schema.
unknown=9223372036854775807
db -c "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
       VALUES ($unknown, 'Compose smoke only', true, decode('00', 'hex'), 0)"
if docker compose run --rm -T --no-deps api migrate > "$scratch/migrate" 2>&1; then
  echo 'The API accepted an unknown migration' >&2
  exit 1
fi
grep -F '"error_kind":"version_missing"' "$scratch/migrate"
status=$(curl -sS --max-time 10 --cacert "$ca" -o "$scratch/ready" \
  -w '%{http_code}' "$origin/api/ready")
test "$status" = 503
jq -e '.status == "unavailable" and .checks.database == "ok" and .checks.migrations == "incompatible"' "$scratch/ready"
db -c "DELETE FROM _sqlx_migrations WHERE version = $unknown"
probe "$origin/api/ready" | jq -e '.status == "ready"'
