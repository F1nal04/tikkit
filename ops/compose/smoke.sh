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

status() {
  curl --silent --show-error --max-time 10 --cacert "$ca" \
    -o "$scratch/response" -w '%{http_code}' "$@"
}

security_headers() {
  tr -d '\r' < "$1" > "$scratch/headers-lf"
  grep -Fxiq 'strict-transport-security: max-age=31536000' "$scratch/headers-lf"
  grep -Fxiq 'x-content-type-options: nosniff' "$scratch/headers-lf"
  grep -Eiq "^content-security-policy: .*frame-ancestors 'none'" "$scratch/headers-lf"
}

probe "$origin/api/health" | jq -e '.status == "ok"'
probe "$origin/api/ready" | jq -e '.status == "ready" and .checks.database == "ok" and .checks.migrations == "ok"'
test "$(status -H 'Host: foreign.example.test' "$origin/setup")" = 421
# A correct hostname and wrong port is still a different authority.
host=${origin#https://}
test "$(status -H "Host: ${host%%:*}:1" "$origin/setup")" = 421

probe -D "$scratch/headers" -H 'X-Request-ID: 00000000-0000-4000-8000-000000000046' \
  "$origin/api/setup" | jq -e '.setup_required == true'
security_headers "$scratch/headers"
grep -Eiq '^x-request-id: [0-9a-f-]{36}$' "$scratch/headers-lf"
if grep -qi '00000000-0000-4000-8000-000000000046' "$scratch/headers-lf"; then
  echo 'Proxy accepted a client-supplied request ID' >&2
  exit 1
fi

probe -D "$scratch/headers" "$origin/setup?lang=en" > "$scratch/page"
grep -Fq '<h1' "$scratch/page"
security_headers "$scratch/headers"
grep -Eiq "^content-security-policy: .*script-src [^;]*'nonce-" "$scratch/headers-lf"
# Check a fingerprinted client module referenced by the SSR page.
asset=$(grep -Eo '/_app/immutable/[^"[:space:]<>]+\.js' "$scratch/page" | sed -n '1p')
test -n "$asset"
probe -D "$scratch/headers" -o /dev/null "$origin$asset"
security_headers "$scratch/headers"

# A forwarded-protocol forgery must not make adapter-node see an HTTP origin.
# With the right origin, the request reaches the route's method check instead.
test "$(status -H "Origin: $origin" -H 'X-Forwarded-Proto: http' \
  --data 'unused=1' "$origin/login")" = 405
test "$(status -D "$scratch/headers" -H 'Origin: https://foreign.example.test' \
  --data 'unused=1' "$origin/login")" = 403
security_headers "$scratch/headers"
test "$(status -D "$scratch/headers" -H 'Content-Length: 10485761' \
  -H "Origin: $origin" -H 'Sec-Fetch-Site: same-origin' -H 'X-Requested-With: tikkit' \
  --data-binary '' "$origin/api/screenshots")" = 413
security_headers "$scratch/headers"

for path in /metrics /api/metrics; do
  test "$(status "$origin$path")" = 404
done

# Use real browser metadata and a disposable account. The optional password
# lets a local Playwright drill reuse the account after smoke completes.
password=${COMPOSE_SMOKE_ADMIN_PASSWORD:-$(openssl rand -hex 24)}
jq -n --arg password "$password" \
  '{username:"compose-admin",$password,email:"compose-admin@example.test",language:"en"}' > "$scratch/setup"
jq 'del(.email,.language)' "$scratch/setup" > "$scratch/login"
post() {
  local request_origin=$1 fetch_site=$2 body=$3
  shift 3
  status -H "Origin: $request_origin" -H "Sec-Fetch-Site: $fetch_site" \
    -H 'X-Requested-With: tikkit' -H 'Content-Type: application/json' \
    --data-binary "@$body" "$@"
}

test "$(post "$origin" same-origin "$scratch/setup" "$origin/api/setup")" = 200
test "$(post https://foreign.example.test same-origin "$scratch/login" "$origin/api/auth/login")" = 403
jq -e '.error.code == "csrf_rejected"' "$scratch/response"
test "$(post "$origin" cross-site "$scratch/login" "$origin/api/auth/login")" = 403
jq -e '.error.code == "csrf_rejected"' "$scratch/response"
test "$(post "$origin" same-origin "$scratch/login" -c "$scratch/cookies" "$origin/api/auth/login")" = 200
jq -e '.username == "compose-admin"' "$scratch/response"
probe -b "$scratch/cookies" "$origin/api/admin/retention" | jq -e '
  .settings.automatic == false and (.settings.policy | all(.[]; . == null))
  and .limits.audit_min_days == 30 and (.runs | length == 0)'

jq '.username = "compose-negative-login"' "$scratch/login" > "$scratch/bad-login"
test "$(post "$origin" same-origin "$scratch/bad-login" "$origin/api/auth/login")" = 401
for suffix in 1 2 3 4; do
  test "$(post "$origin" same-origin "$scratch/bad-login" \
    -H "X-Forwarded-For: 192.0.2.$suffix" "$origin/api/auth/login")" = 401
done
test "$(post "$origin" same-origin "$scratch/bad-login" \
  -H 'X-Forwarded-For: 192.0.2.5' "$origin/api/auth/login")" = 429
jq -e '.error.code == "rate_limited"' "$scratch/response"

db() {
  docker compose exec -T postgres psql -U postgres -d tikkit -v ON_ERROR_STOP=1 -Atq "$@"
}
test "$(db -c "SELECT count(*) = 5 AND count(metadata->>'ip') = 5
        AND bool_and((metadata->>'ip')::inet = (
          SELECT (metadata->>'ip')::inet FROM audit_events WHERE event_type = 'login_succeeded'
        ))
        AND bool_and(NOT (metadata->>'ip')::inet <<= '172.30.0.0/24'::cidr)
        FROM audit_events WHERE event_type = 'login_failed'")" = t

# Check both durable stores across a replacement of every container.
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
