#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
. ops/load/stack.sh
mode=${1:-profile}
case "$mode" in profile|recovery|frontend|smoke|report-diagnostic) ;; *) echo "usage: $0 [profile|recovery|frontend|smoke|report-diagnostic]" >&2; exit 2;; esac
: "${LOAD_API_IMAGE:?set LOAD_API_IMAGE to the production API image}"
: "${LOAD_WEB_IMAGE:?set LOAD_WEB_IMAGE to the production web image}"
export LOAD_WORK
LOAD_WORK=$(mktemp -d "${TMPDIR:-/tmp}/tikkit-load.XXXXXX")
LOAD_WORK=$(cd "$LOAD_WORK" && pwd -P)
owned=0 locked=0
cleanup() {
  rc=$?
  trap - EXIT
  if [ "$locked" = 1 ]; then rmdir /tmp/tikkit-e2e.lock; fi
  if [ "$owned" = 1 ]; then
    if ! stack down --volumes --remove-orphans >&2; then rc=1; fi
    release_project "$project"
  fi
  rm -rf "$LOAD_WORK"
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export LOAD_BASE=https://localhost:18744
if [ -z "${LOAD_SMTP+x}" ]; then
  case "$mode" in
    profile|recovery) LOAD_SMTP=1 ;;
    *) LOAD_SMTP=0 ;;
  esac
fi
export LOAD_SMTP
mkdir "$LOAD_WORK"/{tls,control}
umask 077
cat > "$LOAD_WORK/stack.env" <<ENV
PUBLIC_HOST=localhost:18744
PROXY_BIND=127.0.0.1
HTTP_PORT=18780
HTTPS_PORT=18744
CADDY_GLOBAL_OPTIONS=local_certs
TIKKIT_API_IMAGE=$LOAD_API_IMAGE
TIKKIT_WEB_IMAGE=$LOAD_WEB_IMAGE
POSTGRES_PASSWORD=$(openssl rand -hex 32)
TIKKIT_DB_PASSWORD=$(openssl rand -hex 32)
SESSION_SECRET=$(openssl rand -base64 32)
ENCRYPTION_KEY=$(openssl rand -base64 32)
METRICS_PORT=9091
WORKFLOW_TLS_DIR=$LOAD_WORK/tls
WORKFLOW_UID=$(id -u)
WORKFLOW_GID=$(id -g)
LOAD_CONTROL_DIR=$LOAD_WORK/control
ENV
(
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 -subj '/CN=Tikkit load CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -keyout "$LOAD_WORK/ca.key" -out "$LOAD_WORK/tls/stub-ca.crt"
  openssl req -new -newkey rsa:2048 -nodes -subj '/CN=api.openai.com' \
    -keyout "$LOAD_WORK/tls/stub.key" -out "$LOAD_WORK/stub.csr"
  printf 'subjectAltName=DNS:api.openai.com,DNS:smtp.load.test\nextendedKeyUsage=serverAuth\n' > "$LOAD_WORK/stub.ext"
  openssl x509 -req -in "$LOAD_WORK/stub.csr" -CA "$LOAD_WORK/tls/stub-ca.crt" -CAkey "$LOAD_WORK/ca.key" \
    -CAcreateserial -days 2 -sha256 -extfile "$LOAD_WORK/stub.ext" -out "$LOAD_WORK/tls/stub.crt"
) >"$LOAD_WORK/cert.log" 2>&1 || { cat "$LOAD_WORK/cert.log" >&2; exit 1; }
chmod 644 "$LOAD_WORK/tls/stub-ca.crt"
export LOAD_PROVIDER_MS=${LOAD_PROVIDER_MS:-250}
if [[ ! "$LOAD_PROVIDER_MS" =~ ^[0-9]{1,5}$ ]] || [ "$LOAD_PROVIDER_MS" -gt 30000 ]; then
  echo 'LOAD_PROVIDER_MS must be an integer in 0–30000' >&2
  exit 2
fi
printf '%s\n' "$LOAD_PROVIDER_MS" > "$LOAD_WORK/control/provider"
printf 'ready\n' > "$LOAD_WORK/control/smtp"
existing=$(project_occupied "$LOAD_WORK/stack.env" "$project" "${files[@]}")
if [ -n "$existing" ] || ! reserve_project "$project"; then
  printf 'load project already occupied: %s\n%s\n' "$project" "$existing" >&2
  exit 1
fi
owned=1
stack up -d --wait --wait-timeout 120 postgres >&2
stack run --rm --no-deps api migrate >&2
stack up -d --wait --wait-timeout 120 >&2
stack cp proxy:/data/caddy/pki/authorities/local/root.crt "$LOAD_WORK/portal-ca.crt" >&2
for service in api provider smtp; do
  for net in $(docker inspect --format '{{range $name, $_ := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$(stack ps -q "$service")"); do
    test "$(docker network inspect --format '{{.Internal}}' "$net")" = true
  done
done
export NODE_EXTRA_CA_CERTS="$LOAD_WORK/portal-ca.crt"
until mkdir /tmp/tikkit-e2e.lock 2>/dev/null; do sleep 10; done
locked=1
timeout 120 bun ops/load/seed.mjs "$mode"
case "$mode" in
  profile|smoke) timeout 760 bun ops/load/profile.mjs "$mode" ;;
  recovery) timeout 760 bun ops/load/recovery.mjs ;;
  frontend) timeout 760 bun ops/load/frontend.mjs ;;
  report-diagnostic) timeout 760 bun ops/load/report-diagnostic.mjs ;;
esac
