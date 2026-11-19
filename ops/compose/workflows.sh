#!/usr/bin/env bash
# Run the full Playwright scenario through the production images and Caddy.
set -euo pipefail
cd "$(dirname "$0")/../.."
. ops/backup/hostlib.sh

project=tikkit-m4-48
base=https://localhost:18544
files=(-f docker-compose.prod.example.yml -f ops/compose/workflows.yml)
stack() { dc --env-file "$WORKFLOW_COMPOSE_ENV" -p "$project" "${files[@]}" "$@"; }
ready() {
  curl -fsS --retry 10 --retry-connrefused --retry-delay 1 --max-time 5 \
    --cacert "$(dirname "$WORKFLOW_COMPOSE_ENV")/portal-ca.crt" "$base/api/ready" >/dev/null
}

if [ "${1:-}" = recreate ]; then
  : "${WORKFLOW_COMPOSE_ENV:?run via the Compose workflow test}"
  docker network inspect --format '{{index .Labels "tikkit.reservation"}}' "$project-reserved" | grep -Fxq "$project"
  before_api=$(stack ps -q api)
  before_web=$(stack ps -q web)
  stack up -d --force-recreate --no-deps --wait --wait-timeout 120 api web
  test "$before_api" != "$(stack ps -q api)"
  test "$before_web" != "$(stack ps -q web)"
  ready
  exit
fi

: "${WORKFLOW_API_IMAGE:?set WORKFLOW_API_IMAGE to the production API image}"
: "${WORKFLOW_WEB_IMAGE:?set WORKFLOW_WEB_IMAGE to the production web image}"
work=$(mktemp -d "${TMPDIR:-/tmp}/tikkit-workflows.XXXXXX")
# Docker Desktop needs a canonical host source path for bind mounts.
work=$(cd "$work" && pwd -P)
export WORKFLOW_COMPOSE_ENV="$work/stack.env"
owned=0 locked=0
cleanup() {
  rc=$?
  trap - EXIT
  if [ "$owned" = 1 ]; then
    if [ "$rc" != 0 ]; then stack logs --no-color --tail 100 >&2 || true; fi
    if ! stack down --volumes --remove-orphans; then rc=1; fi
    release_project "$project"
  fi
  if [ "$locked" = 1 ]; then rmdir /tmp/tikkit-e2e.lock; fi
  rm -rf "$work"
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The secrets file stays host-only. The API and the provider each get only the
# cert files they need. The provider runs with the UID that owns the key.
mkdir "$work/tls"
(
  umask 077
  cat > "$WORKFLOW_COMPOSE_ENV" <<ENV
PUBLIC_HOST=localhost:18544
PROXY_BIND=127.0.0.1
HTTP_PORT=18580
HTTPS_PORT=18544
CADDY_GLOBAL_OPTIONS=local_certs
TIKKIT_API_IMAGE=$WORKFLOW_API_IMAGE
TIKKIT_WEB_IMAGE=$WORKFLOW_WEB_IMAGE
POSTGRES_PASSWORD=$(openssl rand -hex 32)
TIKKIT_DB_PASSWORD=$(openssl rand -hex 32)
SESSION_SECRET=$(openssl rand -base64 32)
ENCRYPTION_KEY=$(openssl rand -base64 32)
WORKFLOW_TLS_DIR=$work/tls
WORKFLOW_UID=$(id -u)
WORKFLOW_GID=$(id -g)
ENV
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
    -subj '/CN=Tikkit workflow CA' -addext 'basicConstraints=critical,CA:TRUE' \
    -keyout "$work/ca.key" -out "$work/tls/stub-ca.crt" >"$work/cert.log" 2>&1
  openssl req -new -newkey rsa:2048 -nodes -subj '/CN=api.openai.com' \
    -keyout "$work/tls/stub.key" -out "$work/stub.csr" >>"$work/cert.log" 2>&1
  printf 'subjectAltName=DNS:api.openai.com\nextendedKeyUsage=serverAuth\n' > "$work/stub.ext"
  openssl x509 -req -in "$work/stub.csr" -CA "$work/tls/stub-ca.crt" -CAkey "$work/ca.key" \
    -CAcreateserial -days 2 -sha256 -extfile "$work/stub.ext" -out "$work/tls/stub.crt" >>"$work/cert.log" 2>&1
) || { cat "$work/cert.log" >&2; exit 1; }
chmod 644 "$work/tls/stub-ca.crt"

existing=$(project_occupied "$WORKFLOW_COMPOSE_ENV" "$project" "${files[@]}")
if [ -n "$existing" ] || ! reserve_project "$project"; then
  printf 'workflow project is already in use: %s\n%s\n' "$project" "$existing" >&2
  exit 1
fi
owned=1

stack up -d --wait --wait-timeout 120 postgres
stack run --rm --no-deps api migrate
stack up -d --wait --wait-timeout 120
stack cp proxy:/data/caddy/pki/authorities/local/root.crt "$work/portal-ca.crt"
ready
curl -fsS --cacert "$work/portal-ca.crt" "$base/setup" | grep -q 'Set up Tikkit'
# The API and its provider peer must only have internal networks.
for service in api provider; do
  for network in $(docker inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$(stack ps -q "$service")"); do
    test "$(docker network inspect --format '{{.Internal}}' "$network")" = true
  done
done

until mkdir /tmp/tikkit-e2e.lock 2>/dev/null; do sleep 10; done
locked=1
CI=1 WORKFLOW_BASE_URL="$base" bun run --cwd apps/web playwright test --config=playwright.full.config.ts
