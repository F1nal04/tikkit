#!/usr/bin/env bash
# Repeatable disaster-recovery drill (docs/backup-restore.md#restore-drill).
#
# Starts a disposable production-shaped Compose project, seeds mixed ticket
# outcomes with screenshots, takes an encrypted backup with the real sidecar,
# restores it into a second, isolated project and verifies the copy. It then
# simulates a missing backup key, a missing screenshot file and a missing or
# wrong application ENCRYPTION_KEY.
#
#   DRILL_API_IMAGE=... DRILL_WEB_IMAGE=... ops/backup/drill.sh
#
# Needs docker (Compose v2.33.1+), jq, curl and openssl. Run from anywhere.
# Environment (all optional except the two images):
#   DRILL_SOURCE_PROJECT   default tikkit-drill            (source stack)
#   DRILL_RESTORE_PROJECT  default tikkit-drill-restore     (isolated restore stack)
#       Both names must start with tikkit-drill or tikkit-ci-drill, differ from each other,
#       and own NO existing container, volume or network, or the drill aborts untouched.
#       Pass --i-know-these-projects-are-disposable to use other names that are also free.
#   DRILL_PORT_BASE        default 18444 (uses +0/+1 HTTPS, +36/+37 HTTP)
#   DRILL_MODE             full (default) | reduced (no wrong-key and missing-file restores)
#   DRILL_WORKDIR          scratch directory (default: a new temp dir, removed unless DRILL_KEEP=1)
#   DRILL_BULK_MB          default 0; adds about this much random data to the database and to the
#                          screenshot volume (a throughput measurement for RPO/RTO sizing)
#   DRILL_KEEP=1           leave both projects and the work directory in place
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=hostlib.sh
. ops/backup/hostlib.sh

ALLOW_NAMES=0
for arg in "$@"; do
  case $arg in
    --i-know-these-projects-are-disposable) ALLOW_NAMES=1 ;;
    *) echo "drill: unknown argument $arg" >&2; exit 2 ;;
  esac
done
SRC=${DRILL_SOURCE_PROJECT:-tikkit-drill}
DST=${DRILL_RESTORE_PROJECT:-tikkit-drill-restore}
PORT=${DRILL_PORT_BASE:-18444}
SRC_HTTPS=$PORT SRC_HTTP=$((PORT + 36)) DST_HTTPS=$((PORT + 1)) DST_HTTP=$((PORT + 37))
API_IMAGE=${DRILL_API_IMAGE:?set DRILL_API_IMAGE to the API image under test}
WEB_IMAGE=${DRILL_WEB_IMAGE:?set DRILL_WEB_IMAGE to the web image under test}
BACKUP_IMAGE=${DRILL_BACKUP_IMAGE:-tikkit-drill-backup:local}
MODE=${DRILL_MODE:-full}
KEEP=${DRILL_KEEP:-0}
BULK_MB=${DRILL_BULK_MB:-0}
WORK_CREATED=0
if [ -z "${DRILL_WORKDIR:-}" ]; then WORK_CREATED=1; fi
WORK=${DRILL_WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/tikkit-drill.XXXXXX")}
PASSED=0
TIMES=$WORK/durations.txt
mkdir -p "$WORK"/{jars,ids,ca,offsite,capture}
: > "$TIMES"

say() { echo "drill: $*" >&2; }
die() { echo "drill: FAIL $*" >&2; exit 1; }
pass() { PASSED=$((PASSED + 1)); say "ok   $*"; }
check() { local what=$1; shift; if "$@"; then pass "$what"; else die "$what"; fi; }
mark() { printf '%s=%s\n' "$1" "$2" >> "$TIMES"; say "$1 = $2"; }
timed() { local name=$1 start; shift; start=$(date +%s); "$@"; mark "$name" "$(($(date +%s) - start))s"; }

# --- environment files ----------------------------------------------------------

command -v jq > /dev/null && command -v curl > /dev/null && command -v openssl > /dev/null \
  || die "docker, jq, curl and openssl are required"
PW=$(openssl rand -hex 12)
ENCRYPTION_KEY_VALUE=$(openssl rand -base64 32)

write_env() { # <file> <https-port> <http-port> <encryption key>
  umask 077
  cat > "$1" <<ENV
PUBLIC_ORIGIN=https://localhost:$2
TIKKIT_WEB_IMAGE=$WEB_IMAGE
TIKKIT_API_IMAGE=$API_IMAGE
TIKKIT_BACKUP_IMAGE=$BACKUP_IMAGE
POSTGRES_PASSWORD=$(openssl rand -hex 32)
TIKKIT_DB_PASSWORD=$(openssl rand -hex 32)
SESSION_SECRET=$(openssl rand -base64 32)
ENCRYPTION_KEY=$4
PROXY_BIND=127.0.0.1
HTTP_PORT=$3
HTTPS_PORT=$2
CADDY_GLOBAL_OPTIONS=local_certs
BACKUP_RETENTION_DAYS=14
BACKUP_MIN_KEEP=2
BACKUP_AGE_RECIPIENTS=age1placeholder
METRICS_PORT=9091
ENV
}

# --- compose helpers -------------------------------------------------------------

BASE_FILES=(-f docker-compose.prod.example.yml -f docker-compose.backup.example.yml)
DST_FILES=(-f docker-compose.prod.example.yml -f docker-compose.restore.example.yml)
src() { dc --env-file "$WORK/source.env" -p "$SRC" "${BASE_FILES[@]}" "$@"; }
src_seed() { dc --env-file "$WORK/source.env" -p "$SRC" "${BASE_FILES[@]}" -f docker-compose.restore.example.yml "$@"; }
dst() { dc --env-file "${DST_ENV:-$WORK/restore.env}" -p "$DST" "${DST_FILES[@]}" "$@"; }
sql() { local stack=$1; shift; "$stack" exec -T postgres psql -U postgres -d tikkit -v ON_ERROR_STOP=1 -Atq "$@"; }

# Until the two project names are proven free, the only cleanup is the scratch directory.
early_cleanup() {
  local rc=$?
  [ "$WORK_CREATED" != 1 ] || rm -rf "$WORK"
  exit "$rc"
}
trap early_cleanup EXIT

cleanup() {
  local rc=$?
  # Reached only after both names were free at the start: whatever carries these
  # names now was created by this run.
  if [ "$KEEP" != 1 ]; then
    src down --volumes --remove-orphans > /dev/null 2>&1 || true
    dst down --volumes --remove-orphans > /dev/null 2>&1 || true
    docker rm -f "$SRC-sched-test" > /dev/null 2>&1 || true
    docker volume rm "$SRC-sched-test" "${DST}_screenshots-data" > /dev/null 2>&1 || true
    release_project "$SRC"
    rm -rf "$WORK"
  else
    say "left in place: projects $SRC and $DST, work directory $WORK"
  fi
  [ "$rc" = 0 ] || say "drill FAILED (exit $rc)"
  exit "$rc"
}

# --- HTTP helpers (through the proxy with the project's local CA) ---------------

BASE_SRC=https://localhost:$SRC_HTTPS
BASE_DST=https://localhost:$DST_HTTPS
HTTP_CODE=
api_raw() { # <jar> <base> <method> <path> [json]  -> body on stdout, code in $WORK/.code
  local jar=$1 base=$2 method=$3 path=$4 body=${5:-} out args
  out=$(mktemp "$WORK/.out.XXXXXX")
  args=(-sS --cacert "$WORK/ca/${base##*:}.crt" -b "$jar" -c "$jar" -X "$method"
    -H 'X-Requested-With: tikkit' -H "Origin: $base" -o "$out" -w '%{http_code}')
  [ -z "$body" ] || args+=(-H 'content-type: application/json' --data "$body")
  curl "${args[@]}" "$base$path" > "$WORK/.code"
  cat "$out"
  rm -f "$out"
}
api() { # success required
  local out code
  out=$(api_raw "$@")
  code=$(cat "$WORK/.code")
  [ "${code:0:1}" = 2 ] || { echo "drill: $3 $4 -> $code: $out" >&2; return 1; }
  printf '%s' "$out"
}
code_of() { api_raw "$@" > /dev/null; cat "$WORK/.code"; }
jar_of() { echo "$WORK/jars/$1-$2"; } # stack user

login() { # <stack-name> <base> <user>
  local jar; jar=$(jar_of "$1" "$3")
  api "$jar" "$2" POST /api/auth/login "$(jq -n --arg u "$3" --arg p "$PW" '{username:$u,password:$p}')" > /dev/null
}

wait_ready() { # <base>
  curl -sS --fail --retry 60 --retry-connrefused --retry-delay 2 --max-time 5 --cacert "$WORK/ca/${1##*:}.crt" "$1/api/ready" > /dev/null
}

fetch_ca() { # <stack fn> <port>
  "$1" cp proxy:/data/caddy/pki/authorities/local/root.crt "$WORK/ca/$2.crt" > /dev/null
}

# --- seeding ----------------------------------------------------------------------

PNG0=iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mM4IScHAAK2AQUKW6YGAAAAAElFTkSuQmCC
PNG1=iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mOQOyEHAAIMAQUTReXtAAAAAElFTkSuQmCC
PNG2=iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mOQkzsBAAFiAQUv5nCNAAAAAElFTkSuQmCC
PNG3=iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mM4cUIOAAQKAa9+0HUUAAAAAElFTkSuQmCC

upload() { # <user> <base64 png> <name> -> screenshot id
  local jar out; jar=$(jar_of src "$1")
  printf '%s' "$2" | base64 -d > "$WORK/.png"
  out=$(curl -sS --fail --cacert "$WORK/ca/$SRC_HTTPS.crt" -b "$jar" -c "$jar" -X POST \
    -H 'X-Requested-With: tikkit' -H "Origin: $BASE_SRC" -H 'content-type: image/png' \
    --data-binary @"$WORK/.png" "$BASE_SRC/api/screenshots?filename=$3.png")
  jq -r .id <<< "$out"
}

submit() { # <user> <subject> [screenshot ids...] -> ticket id (stored in ids/<subject slug>)
  local user=$1 subject=$2 shots body
  shift 2
  shots=$(printf '%s\n' "$@" | jq -R . | jq -s 'map(select(. != ""))')
  body=$(jq -n --arg k "$(uuidgen | tr 'A-Z' 'a-z')" --arg c "$(cat "$WORK/ids/category")" --arg p "$(cat "$WORK/ids/priority")" \
    --arg s "$subject" --argjson shots "$shots" \
    '{idempotency_key:$k, intake:{category_id:$c, priority_id:$p, subject:$s, description:("Drill ticket: " + $s + ". The printer stops after the first page."), answers:{}, screenshot_ids:$shots}, requester_skip:false}')
  api "$(jar_of src "$user")" "$BASE_SRC" POST /api/tickets "$body" | jq -r .id
}

updated_at() { api "$(jar_of src "$1")" "$BASE_SRC" GET "/api/tickets/$2" | jq -r .updated_at; }

assign() { # <user> <ticket> <json fields without expected_updated_at>
  api "$(jar_of src "$1")" "$BASE_SRC" POST "/api/tickets/$2/assignment" \
    "$(jq -n --arg t "$(updated_at "$1" "$2")" --argjson f "$3" '$f + {expected_updated_at:$t}')" > /dev/null
}
staff() { # <user> <ticket> <json fields>
  api "$(jar_of src "$1")" "$BASE_SRC" POST "/api/tickets/$2/staff-actions" \
    "$(jq -n --arg t "$(updated_at "$1" "$2")" --argjson f "$3" '$f + {expected_updated_at:$t}')" > /dev/null
}
requester() { # <user> <ticket> <json>
  api "$(jar_of src "$1")" "$BASE_SRC" POST "/api/tickets/$2/actions" "$3"
}
status_of() { api "$(jar_of src "$1")" "$BASE_SRC" GET "/api/tickets/$2" | jq -r .status; }

invite() { # <user> <role>
  local jar token
  jar=$(jar_of src admin)
  token=$(api "$jar" "$BASE_SRC" POST /api/invitations "$(jq -n --arg r "$2" '{role:$r, language:"en"}')" | jq -r '.url | split("/") | last | split("?") | first')
  api "$(jar_of src "$1")" "$BASE_SRC" POST /api/invitations/accept \
    "$(jq -n --arg t "$token" --arg u "$1" --arg p "$PW" '{token:$t, username:$u, email:"drill@example.com", password:$p, language:"en"}')" | jq -r .id > "$WORK/ids/$1"
}

seed_phase_a() {
  local admin; admin=$(jar_of src admin)
  api "$admin" "$BASE_SRC" POST /api/setup "$(jq -n --arg p "$PW" '{username:"admin", email:"admin@example.com", password:$p, language:"en"}')" > /dev/null
  api "$admin" "$BASE_SRC" POST /api/categories '{"name":"Printing"}' | jq -r .id > "$WORK/ids/category"
  api "$admin" "$BASE_SRC" POST /api/priorities '{"name":"Normal"}' | jq -r .id > "$WORK/ids/priority"
  invite lead team_lead; invite spec specialist; invite req1 user; invite req2 user; invite req3 user
  api "$admin" "$BASE_SRC" POST /api/teams "$(jq -n --arg l "$(cat "$WORK/ids/lead")" '{name:"Desk", lead_account_id:$l}')" | jq -r .id > "$WORK/ids/team"
  api "$admin" "$BASE_SRC" PUT "/api/accounts/$(cat "$WORK/ids/spec")" \
    "$(jq -n --arg t "$(cat "$WORK/ids/team")" '{role:"specialist", team_id:$t, expertise:"Printers"}')" > /dev/null
  for user in lead spec req1 req2 req3; do login src "$BASE_SRC" "$user"; done
  # An SMTP relay is stored (password encrypted with ENCRYPTION_KEY) but left disabled.
  api "$admin" "$BASE_SRC" PUT /api/settings/smtp \
    '{"host":"smtp.invalid","port":587,"security":"starttls","sender_name":"Tikkit","sender_address":"help@example.invalid","username":"mailer","password":"drill-smtp-password","enabled":false}' > /dev/null

  local s0 s1 s2 s3 team; team=$(cat "$WORK/ids/team")
  s0=$(upload req1 "$PNG0" shot0); s1=$(upload req2 "$PNG1" shot1); s2=$(upload req3 "$PNG2" shot2); s3=$(upload req1 "$PNG3" shot3)
  printf '%s\n' "$s0" "$s1" "$s2" "$s3" > "$WORK/ids/screenshots"

  # 1. operator-resolved: dispatched, claimed, proposed, accepted by the requester
  local t; t=$(submit req1 "Operator resolved" "$s0"); echo "$t" > "$WORK/ids/t_operator_resolved"
  assign admin "$t" "$(jq -n --arg t "$team" '{action:"dispatch", team_id:$t}')"
  assign spec "$t" '{"action":"claim"}'
  staff spec "$t" '{"action":"start_work"}'
  staff spec "$t" '{"action":"propose_resolution","summary":"Replaced the toner; the printer works."}'
  requester req1 "$t" '{"action":"accept_resolution","proposal_version":1}' > /dev/null
  # 2. auto-closed: the proposal's snapshotted deadline passes, the worker closes it
  t=$(submit req2 "Auto closed" "$s1"); echo "$t" > "$WORK/ids/t_auto_closed"
  assign admin "$t" "$(jq -n --arg t "$team" '{action:"dispatch", team_id:$t}')"
  assign spec "$t" '{"action":"claim"}'
  staff spec "$t" '{"action":"start_work"}'
  staff spec "$t" '{"action":"propose_resolution","summary":"Driver reinstalled."}'
  sql src -c "UPDATE tickets SET confirmation_deadline = now() - interval '1 minute' WHERE id = '$t'"
  for _ in $(seq 1 60); do [ "$(status_of req2 "$t")" = closed ] && break; sleep 2; done
  # 3. cancelled by the requester
  t=$(submit req3 "Cancelled by requester" "$s2"); echo "$t" > "$WORK/ids/t_cancelled"
  requester req3 "$t" '{"action":"cancel","reason":"Colleague fixed it."}' > /dev/null
  # 4. open in the shared queue
  t=$(submit req1 "Still open" "$s3"); echo "$t" > "$WORK/ids/t_open"
}

# The provider call is the one thing a Compose stack cannot do offline, so the AI
# stage runs against an API in restore mode: submission, the attempt row, the
# requester decision and every other code path are the real ones; only the
# provider's answer is written by SQL (what the attempt worker would store).
seed_phase_b() {
  local admin; admin=$(jar_of src admin)
  src_seed up -d --wait --no-deps api
  wait_ready "$BASE_SRC"
  for user in admin lead spec req1 req2 req3; do login src "$BASE_SRC" "$user"; done
  api "$admin" "$BASE_SRC" PUT /api/settings/ai '{"enabled":false,"provider":"openai","model":"gpt-6-astra"}' > /dev/null
  api "$admin" "$BASE_SRC" PUT /api/settings/ai/credentials/openai "$(jq -n --arg k "sk-drill-$(openssl rand -hex 16)" '{api_key:$k}')" > /dev/null
  api "$admin" "$BASE_SRC" PUT /api/settings/ai '{"enabled":true,"provider":"openai","model":"gpt-6-astra"}' > /dev/null
  # Email is switched on only now, with no worker running (restore mode), so the
  # notification rows of the events below stay pending: the rows a cutover must handle.
  api "$admin" "$BASE_SRC" PUT /api/settings/smtp \
    '{"host":"smtp.invalid","port":587,"security":"starttls","sender_name":"Tikkit","sender_address":"help@example.invalid","enabled":true}' > /dev/null
  local t
  t=$(submit req2 "Solved by AI"); echo "$t" > "$WORK/ids/t_ai_solved"
  [ "$(status_of req2 "$t")" = ai_processing ] || die "AI ticket did not enter ai_processing"
  sql src <<SQL
BEGIN;
UPDATE ai_attempts SET state = 'suggestion_ready', explanation = 'Restart the print spooler service.',
    steps = '["Open Services","Restart Print Spooler"]'::jsonb, finished_at = clock_timestamp(), claimed_until = NULL
  WHERE ticket_id = '$t' AND state = 'processing';
UPDATE tickets SET status = 'awaiting_ai_decision', updated_at = clock_timestamp() WHERE id = '$t' AND status = 'ai_processing';
INSERT INTO ticket_events (ticket_id, actor_account_id, event_type, from_status, to_status, payload)
  VALUES ('$t', NULL, 'ai_suggestion_ready', 'ai_processing', 'awaiting_ai_decision', '{}');
COMMIT;
SQL
  [ "$(requester req2 "$t" '{"action":"ai_solved"}' | jq -r .status)" = ai_solved ] || die "ai_solved decision failed"
  # An attempt that is still in flight when the backup is taken.
  t=$(submit req3 "AI attempt in flight"); echo "$t" > "$WORK/ids/t_ai_inflight"
  [ "$(status_of req3 "$t")" = ai_processing ] || die "in-flight AI ticket did not enter ai_processing"
  [ "$(sql src -c "SELECT count(*) FROM notification_outbox WHERE status = 'pending'")" -gt 0 ] \
    || die "no pending notification rows to carry through the restore"
}

# Optional bulk: a table of incompressible rows and 5 MB files, so durations reflect size.
seed_bulk() {
  [ "$BULK_MB" -gt 0 ] || return 0
  sql src -c "CREATE TABLE drill_bulk AS SELECT b.g AS id, (SELECT string_agg(md5(random()::text || k::text || b.g::text), '') FROM generate_series(1, 32) k) AS filler FROM generate_series(1, $((BULK_MB * 1024))) b(g); ALTER TABLE drill_bulk OWNER TO tikkit"
  src exec -T api sh -c "i=0; while [ \$i -lt $((BULK_MB / 5)) ]; do head -c 5242880 /dev/urandom > /var/lib/tikkit/screenshots/00000000-0000-4000-8000-\$(printf %012d \$i); i=\$((i + 1)); done"
  mark bulk_database_bytes "$(sql src -c "SELECT pg_database_size('tikkit')")"
  mark bulk_volume_bytes "$(src exec -T api sh -c 'du -sk /var/lib/tikkit/screenshots' | cut -f1 | awk '{print $1 * 1024}')"
}

# --- capture and compare -----------------------------------------------------------

# Everything a restored copy must reproduce, as normalized JSON/text files in <dir>.
capture() { # <stack name: src|dst> <base> <stack fn> <dir>
  local stack=$1 base=$2 fn=$3 dir=$4 jar t name id
  mkdir -p "$dir"
  for user in admin lead spec req1 req2 req3; do
    [ -s "$(jar_of "$stack" "$user")" ] || login "$stack" "$base" "$user"
  done
  for name in operator_resolved auto_closed cancelled open ai_solved ai_inflight; do
    id=$(cat "$WORK/ids/t_$name")
    case $name in
      operator_resolved|open) owner=req1 ;; auto_closed|ai_solved) owner=req2 ;; *) owner=req3 ;;
    esac
    [ "$name" != open ] || owner=req1
    api "$(jar_of "$stack" "$owner")" "$base" GET "/api/tickets/$id" | jq -S . > "$dir/ticket-$name.json"
    api "$(jar_of "$stack" "$owner")" "$base" GET "/api/tickets/$id/activity" | jq -S . > "$dir/activity-$name.json"
    api "$(jar_of "$stack" admin)" "$base" GET "/api/tickets/$id" | jq -S . > "$dir/admin-ticket-$name.json" || echo "no-admin-view" > "$dir/admin-ticket-$name.json"
    echo "$(code_of "$(jar_of "$stack" spec)" "$base" GET "/api/tickets/$id")" > "$dir/operator-code-$name.txt"
  done
  for user in req1 req2 req3; do
    api "$(jar_of "$stack" "$user")" "$base" GET "/api/tickets?limit=100" | jq -S . > "$dir/own-tickets-$user.json"
  done
  api "$(jar_of "$stack" spec)" "$base" GET "/api/queue?view=general&status=queued&limit=100" | jq -S . > "$dir/queue-general.json"
  api "$(jar_of "$stack" spec)" "$base" GET "/api/queue?view=team&limit=100" | jq -S . > "$dir/queue-team.json"
  api "$(jar_of "$stack" spec)" "$base" GET /api/queue/counts | jq -S . > "$dir/queue-counts.json"
  for path in /api/admin/ai/outcomes /api/admin/ai/records "/api/admin/submissions?limit=100" /api/settings/ai/usage; do
    api "$(jar_of "$stack" admin)" "$base" GET "$path" | jq -S . > "$dir/admin-$(echo "$path" | tr -c 'a-z0-9\n' '_').json"
  done
  # AI-solved records are final: requester and staff are refused, whatever is asked.
  id=$(cat "$WORK/ids/t_ai_solved")
  {
    code_of "$(jar_of "$stack" req2)" "$base" POST "/api/tickets/$id/actions" '{"action":"reopen"}'
    code_of "$(jar_of "$stack" req2)" "$base" POST "/api/tickets/$id/actions" '{"action":"cancel","reason":"x"}'
    code_of "$(jar_of "$stack" req2)" "$base" POST "/api/tickets/$id/actions" '{"action":"ai_not_solved"}'
    code_of "$(jar_of "$stack" spec)" "$base" POST "/api/tickets/$id/staff-actions" '{"action":"start_work","expected_updated_at":"2030-01-01T00:00:00Z"}'
  } > "$dir/ai-solved-final-codes.txt"
  # Screenshot bytes through the API, as the requester sees them.
  : > "$dir/screenshot-downloads.txt"
  i=0
  for owner in req1 req2 req3 req1; do
    id=$(sed -n "$((i + 1))p" "$WORK/ids/screenshots"); i=$((i + 1))
    curl -sS --cacert "$WORK/ca/${base##*:}.crt" -b "$(jar_of "$stack" "$owner")" -H 'X-Requested-With: tikkit' \
      -o "$WORK/.dl" -w '%{http_code} ' "$base/api/screenshots/$id/download" >> "$dir/screenshot-downloads.txt" || true
    sha256sum < "$WORK/.dl" 2> /dev/null >> "$dir/screenshot-downloads.txt" || shasum -a 256 < "$WORK/.dl" >> "$dir/screenshot-downloads.txt"
  done
  # Database aggregates and the files on the volume.
  {
    sql "$fn" -c "SELECT 'tickets', status, coalesce(resolution_source::text,''), coalesce(closure_reason::text,''), count(*) FROM tickets GROUP BY 2,3,4 ORDER BY 2,3,4"
    sql "$fn" -c "SELECT 'events', event_type, count(*) FROM ticket_events GROUP BY 2 ORDER BY 2"
    sql "$fn" -c "SELECT 'ai_attempts', state, count(*) FROM ai_attempts GROUP BY 2 ORDER BY 2"
    sql "$fn" -c "SELECT 'ai_usage', count(*) FROM ai_usage"
    sql "$fn" -c "SELECT 'audit', count(*) FROM audit_events WHERE event_type <> 'login_succeeded'"
    sql "$fn" -c "SELECT 'accounts', role::text, count(*) FROM accounts GROUP BY 2 ORDER BY 2"
    sql "$fn" -c "SELECT 'screenshots', count(*), count(ticket_id) FROM screenshots"
  } > "$dir/aggregates.txt"
  # Rows the restore cutover suppressed count as the pending rows they were.
  sql "$fn" -c "SELECT CASE WHEN last_error = 'suppressed_on_restore' THEN 'pending' ELSE status END AS status, channel, count(*) FROM notification_outbox GROUP BY 1,2 ORDER BY 1,2" > "$dir/outbox.txt"
  "$fn" exec -T api sh -c "cd /var/lib/tikkit/screenshots && find . -type f ! -name '*.tmp' -exec sha256sum {} +" | sort -k2 > "$dir/volume-files.txt"
}

# --- stack lifecycle ---------------------------------------------------------------

compose_up_source() {
  src up -d --wait --wait-timeout 180 postgres
  src run --rm --no-deps api migrate
  src up -d --wait --wait-timeout 180 --no-build
}

teardown_restore() { dst down --volumes --remove-orphans > /dev/null 2>&1 || true; }

restore_into_dst() { # <backup dir> <identity> [extra restore.sh args...]
  local backup=$1 identity=$2
  shift 2
  ops/backup/restore.sh --backup "$backup" --identity "$identity" --project "$DST" \
    --env-file "${DST_ENV:-$WORK/restore.env}" --image "$BACKUP_IMAGE" "$@"
}

# --- run ----------------------------------------------------------------------------

DRILL_START=$(date +%s)
write_env "$WORK/source.env" "$SRC_HTTPS" "$SRC_HTTP" "$ENCRYPTION_KEY_VALUE"
write_env "$WORK/restore.env" "$DST_HTTPS" "$DST_HTTP" "$ENCRYPTION_KEY_VALUE"
write_env "$WORK/wrongkey.env" "$DST_HTTPS" "$DST_HTTP" "$(openssl rand -base64 32)"

# Safety first: refuse names that could be a real deployment, and refuse any
# project that already owns a container, volume or network. Only then is a
# teardown installed, so the drill can only ever remove what it created itself.
if [ "$SRC" = "$DST" ]; then die "the source and restore projects must differ"; fi
if [ "$ALLOW_NAMES" != 1 ]; then
  for name in "$SRC" "$DST"; do
    case $name in
      tikkit-drill* | tikkit-ci-drill*) ;;
      *) die "project name '$name' does not start with tikkit-drill or tikkit-ci-drill; use a disposable name, or pass --i-know-these-projects-are-disposable if you are sure it is free" ;;
    esac
  done
fi
for entry in "$SRC:source.env:${BASE_FILES[*]}" "$DST:restore.env:${DST_FILES[*]}"; do
  name=${entry%%:*}
  rest=${entry#*:}
  envfile=$WORK/${rest%%:*}
  fileargs=${rest#*:}
  # shellcheck disable=SC2086
  occupied=$(project_occupied "$envfile" "$name" $fileargs)
  docker network inspect "$name-reserved" > /dev/null 2>&1 && occupied="$occupied
network $name-reserved"
  if [ -n "$occupied" ]; then
    printf 'drill:   exists: %s\n' "$occupied" >&2
    die "project '$name' already owns the resources above. Nothing was changed or removed. Choose other DRILL_*_PROJECT names, or remove that project yourself if it is disposable"
  fi
done
reserve_project "$SRC" || die "project '$SRC' is reserved by another run"
trap cleanup EXIT


# Safe to build and start things now: the names are proven free and reserved.
poison_check() {
  # Compose must take its values from the env file only, never from this shell.
  local origin
  origin=$(src config --format json | jq -r '.services.api.environment.PUBLIC_ORIGIN')
  [ "$origin" = "https://localhost:$SRC_HTTPS" ]
}

say "building the backup image and generating the backup key pair"
docker build -q -f ops/backup/Dockerfile -t "$BACKUP_IMAGE" . > /dev/null
docker run --rm "$BACKUP_IMAGE" age-keygen 2> /dev/null > "$WORK/identity.txt"
chmod 600 "$WORK/identity.txt"
RECIPIENT=$(grep -o 'age1[a-z0-9]*' "$WORK/identity.txt" | head -1)
sed -i.bak "s/^BACKUP_AGE_RECIPIENTS=.*/BACKUP_AGE_RECIPIENTS=$RECIPIENT/" "$WORK/source.env" && rm -f "$WORK/source.env.bak"
# A second key pair stands for "the wrong key / the key is lost".
docker run --rm "$BACKUP_IMAGE" age-keygen 2> /dev/null > "$WORK/other-identity.txt"

check "Compose interpolation ignores the surrounding environment (only the env file counts)" poison_check

host_user=(--user "$(id -u):$(id -g)")
decrypt_manifest() { # <backup dir>
  docker run --rm -i "${host_user[@]}" -v "$WORK/identity.txt":/identity:ro "$BACKUP_IMAGE" age -d -i /identity < "$1/manifest.txt.age"
}
verify_dir() { docker run --rm "${host_user[@]}" -v "$1":/backup:ro "$BACKUP_IMAGE" verify /backup; }
metric() { src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics | sed -n "s/^$1 //p" | tr -d '\r'; }
backup_dirs() { src exec -T backup sh -c 'ls /backups | grep "^2" | sort' | tr -d '\r'; }
run_backup() { # <log file> [exec options...]  -> backup exit status
  local log=$1
  shift
  src exec -T "$@" backup /opt/tikkit-backup/entrypoint.sh run 2> "$log"
}

say "starting the source stack ($SRC) on ports $SRC_HTTPS/$SRC_HTTP"
timed source_stack_start compose_up_source
fetch_ca src "$SRC_HTTPS"
wait_ready "$BASE_SRC"
check "backup sidecar exposes its status metrics" \
  src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics
for _ in $(seq 1 30); do
  [ "$(backup_dirs | grep -c .)" -ge 1 ] && break
  sleep 2
done
check "sidecar took a first backup on its own (no backup since the latest due time)" \
  test "$(metric tikkit_backup_last_run_success)" = 1
# That first backup is of an empty installation: a valid backup with zero screenshots.
ZERO=$(backup_dirs | head -n 1)
src cp "backup:/backups/$ZERO" "$WORK/offsite/$ZERO" > /dev/null

say "project safety: the drill refuses occupied, identical and unmarked project names"
drill_refuses() { # <pattern> <flag|noflag> <env assignments...>
  local pattern=$1 flag=() out
  [ "$2" != flag ] || flag=(--i-know-these-projects-are-disposable)
  shift 2
  out=$(env "$@" DRILL_API_IMAGE="$API_IMAGE" DRILL_WEB_IMAGE="$WEB_IMAGE" DRILL_WORKDIR="$WORK/selftest" ops/backup/drill.sh ${flag[@]+"${flag[@]}"} 2>&1) && return 1
  grep -q "$pattern" <<< "$out"
}
check "an occupied project name aborts the drill before anything is built or removed" \
  drill_refuses "already owns the resources above" flag DRILL_SOURCE_PROJECT="$SRC" DRILL_RESTORE_PROJECT="$DST-other"
check "the source stack is still running after that refusal" \
  test "$(src ps -q | grep -c .)" -ge 4
check "identical project names are refused" \
  drill_refuses "must differ" flag DRILL_SOURCE_PROJECT=tikkit-drill-same DRILL_RESTORE_PROJECT=tikkit-drill-same
check "a name without the drill prefix is refused without the explicit opt-in" \
  drill_refuses "does not start with tikkit-drill" noflag DRILL_SOURCE_PROJECT=tikkit DRILL_RESTORE_PROJECT=tikkit-restore
rm -rf "$WORK/selftest"

say "seeding mixed ticket outcomes"
timed seed_phase_a seed_phase_a
timed seed_phase_b seed_phase_b
timed seed_bulk seed_bulk
capture src "$BASE_SRC" src "$WORK/capture/source"
SRC_PENDING_EMAIL=$(sql src -c "SELECT count(*) FROM notification_outbox WHERE status = 'pending' AND channel = 'email'")
SRC_PENDING_INAPP=$(sql src -c "SELECT count(*) FROM notification_outbox WHERE status = 'pending' AND channel = 'in_app'")
check "source screenshots are all served (200) through the API" \
  test "$(grep -c '^200 ' "$WORK/capture/source/screenshot-downloads.txt")" = 4
check "source has all five outcome kinds" \
  test "$(sql src -c "SELECT count(DISTINCT coalesce(resolution_source::text, status)) FROM tickets")" -ge 5
check "source has pending email rows that a cutover must deal with (the test is not vacuous)" \
  test "$SRC_PENDING_EMAIL" -gt 0

# Retention: dated placeholders older than BACKUP_RETENTION_DAYS go, the newest good copies stay.
src exec -T backup sh -c 'mkdir -m 700 /backups/20200101T000000Z /backups/20200102T000000Z /backups/20200103T000000Z'

say "taking the backup"
BACKUP_START=$(date +%s)
run_backup "$WORK/backup.log" || { cat "$WORK/backup.log" >&2; die "backup run failed"; }
BACKUP_SECONDS=$(($(date +%s) - BACKUP_START))
mark backup_run "${BACKUP_SECONDS}s"
STAMP=$(backup_dirs | tail -n 1)
mark backup_size_bytes "$(metric tikkit_backup_last_size_bytes)"
check "this base has no screenshot-files lock: the backup says so and still runs" \
  grep -q 'screenshot_files_lock_key() does not exist in this database: running WITHOUT' "$WORK/backup.log"
check "old backups past retention were removed" test "$(src exec -T backup sh -c 'ls /backups | grep -c "^2020"')" = 0
check "newest backups were kept" test "$(src exec -T backup sh -c 'ls /backups | grep -c "^2026\|^20[3-9]"')" -ge 1
check "backup files are private (0600) and the directory 0700" \
  test "$(src exec -T backup sh -c "stat -c '%a' /backups/$STAMP /backups/$STAMP/*.age | sort -u | tr '\n' ' '")" = "600 700 "

# The off-host copy: the reconciliation script, with a push command that copies to a directory.
export DRILL_DEST="$WORK/destination"
mkdir -p "$DRILL_DEST"
offsite() { ops/backup/offsite.sh --project "$SRC" --stage "$WORK/stage" --image "$BACKUP_IMAGE" --push 'cp -R "$BACKUP_DIR" "$DRILL_DEST/$BACKUP_NAME"' "$@"; }
offsite > "$WORK/offsite.out" || { cat "$WORK/offsite.out" >&2; die "offsite.sh failed"; }
check "offsite.sh shipped the newest backup and reports the destination's freshness" \
  grep -q "offsite_newest_shipped=$STAMP offsite_age_seconds=" "$WORK/offsite.out"
check "the destination holds a copy that verifies by checksum without the key" verify_dir "$DRILL_DEST/$STAMP"
check "a second offsite.sh run is idempotent" bash -c 'out=$(ops/backup/offsite.sh --project "$1" --stage "$2" --image "$3" --push "false"); grep -q "offsite_newest_shipped=$4" <<< "$out"' _ "$SRC" "$WORK/stage" "$BACKUP_IMAGE" "$STAMP"
check "offsite.sh signals a destination that is older than the allowed age" \
  bash -c '! ops/backup/offsite.sh --project "$1" --stage "$2" --image "$3" --push "false" --max-age-hours 0 > /dev/null' _ "$SRC" "$WORK/stage" "$BACKUP_IMAGE"
cp -R "$DRILL_DEST/$STAMP" "$WORK/offsite/$STAMP"
BACKUP_LISTING=$(ls -A "$WORK/offsite/$STAMP")
check "keyless verification rejects a damaged copy" \
  bash -c 'cp -R "$1" "$2/tampered"; printf x >> "$2/tampered/screenshots.tar.age"; ! docker run --rm --user "$(id -u):$(id -g)" -v "$2/tampered":/backup:ro "$3" verify /backup > /dev/null 2>&1' _ "$WORK/offsite/$STAMP" "$WORK/offsite" "$BACKUP_IMAGE"
check "keyless verification rejects a copy with the manifest and its checksum line removed" \
  bash -c 'cp -R "$1" "$2/nomanifest"; rm "$2/nomanifest/manifest.txt.age"; grep -v manifest.txt.age "$1/SHA256SUMS" > "$2/nomanifest/SHA256SUMS"; ! docker run --rm --user "$(id -u):$(id -g)" -v "$2/nomanifest":/backup:ro "$3" verify /backup > /dev/null 2>&1' _ "$WORK/offsite/$STAMP" "$WORK/offsite" "$BACKUP_IMAGE"
check "keyless verification rejects an extra or duplicated checksum entry" \
  bash -c 'cp -R "$1" "$2/dup"; cat "$1/SHA256SUMS" "$1/SHA256SUMS" > "$2/dup/SHA256SUMS"; ! docker run --rm --user "$(id -u):$(id -g)" -v "$2/dup":/backup:ro "$3" verify /backup > /dev/null 2>&1' _ "$WORK/offsite/$STAMP" "$WORK/offsite" "$BACKUP_IMAGE"
check "plaintext secrets are not in the encrypted files" \
  bash -c '! grep -aqF -e "$1" -e "drill-smtp-password" "$2"/*.age' _ "$ENCRYPTION_KEY_VALUE" "$WORK/offsite/$STAMP"
check "the manifest records the snapshot time used for the email cutover" \
  bash -c 'decrypt() { docker run --rm -i --user "$(id -u):$(id -g)" -v "$1":/identity:ro "$2" age -d -i /identity; }; decrypt "$3" "$4" < "$5/manifest.txt.age" | grep -q "^snapshot_epoch=[0-9]"' _ "$WORK/identity.txt" "$BACKUP_IMAGE" "$WORK/identity.txt" "$BACKUP_IMAGE" "$WORK/offsite/$STAMP"

say "backup run mechanics: locking, unique publication, retention minimum"
concurrent_runs() {
  local a=0 b=0
  run_backup "$WORK/concurrent-a.log" & pa=$!
  run_backup "$WORK/concurrent-b.log" & pb=$!
  wait "$pa" || a=$?
  wait "$pb" || b=$?
  echo "$a $b"
}
before=$(backup_dirs | grep -c .)
rcs=$(concurrent_runs)
check "of two simultaneous runs exactly one succeeds and the other is refused with 75" \
  bash -c 'case "$1" in "0 75"|"75 0") exit 0 ;; *) echo "got: $1" >&2; exit 1 ;; esac' _ "$rcs"
check "the refused run did not touch the winner's staging directory (one more backup, complete)" \
  test "$(backup_dirs | grep -c .)" = "$((before + 1))"
before=$(backup_dirs | grep -c .)
for _ in 1 2 3; do run_backup "$WORK/rapid.log" || die "back-to-back backup failed"; done
check "three back-to-back runs publish three distinct directories" test "$(backup_dirs | grep -c .)" = "$((before + 3))"
check "no backup directory contains another backup" \
  test -z "$(src exec -T backup sh -c 'find /backups -mindepth 2 -maxdepth 2 -name "2*Z"' | tr -d '\r')"
check "no staging directory or lock directory is left behind" \
  test -z "$(src exec -T backup sh -c 'ls -A /backups | grep -E "^\.tmp-|^\.lock$"' | tr -d '\r')"

# Incomplete directories are never counted toward the retention minimum: with
# retention 0 days and a minimum of 2, two bogus newest-named directories must not
# crowd out the real copies.
src exec -T backup sh -c 'mkdir -m 700 /backups/20991231T000001Z /backups/20991231T000002Z'
run_backup "$WORK/retention-min.log" -e BACKUP_RETENTION_DAYS=0 -e BACKUP_MIN_KEEP=2 || die "retention-minimum backup failed"
check "the newest complete copies survive even when bogus directories sort first" \
  test "$(for d in $(backup_dirs | grep -v '^2099'); do src exec -T backup sh -c "cd /backups/$d && sha256sum -c SHA256SUMS > /dev/null 2>&1 && echo ok"; done | tr -d '\r' | grep -c ok)" -ge 2
src exec -T backup sh -c 'rmdir /backups/20991231T000001Z /backups/20991231T000002Z'

say "failure simulation: the database session (and the screenshot-files lock) is lost mid-backup"
sql src -c "CREATE TABLE drill_gate (id int); ALTER TABLE drill_gate OWNER TO tikkit;
  CREATE FUNCTION screenshot_files_lock_key() RETURNS bigint LANGUAGE sql IMMUTABLE AS 'SELECT 4711::bigint';
  ALTER FUNCTION screenshot_files_lock_key() OWNER TO tikkit"
dirs_before=$(backup_dirs)
metric_before=$(metric tikkit_backup_last_success_timestamp_seconds)
# Hold pg_dump behind a table lock so the session stays open for a while.
src exec -T postgres psql -U postgres -d tikkit -c "BEGIN; LOCK TABLE drill_gate IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(40); COMMIT" > /dev/null 2>&1 &
GATE=$!
sleep 2
run_backup "$WORK/backup-lock-lost.log" & LOCKRUN=$!
for _ in $(seq 1 60); do
  [ "$(sql src -c "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'pg_dump' AND wait_event_type = 'Lock'")" -ge 1 ] && break
  sleep 1
done
check "while the dump runs, the backup session holds the screenshot-files advisory lock" \
  test "$(sql src -c "SELECT count(*) FROM pg_locks l JOIN pg_stat_activity a USING (pid) WHERE l.locktype = 'advisory' AND l.granted AND a.application_name = 'tikkit-backup'")" = 1
sql src -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name = 'tikkit-backup'" > /dev/null
sql src -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE query LIKE '%pg_sleep(40)%' AND pid <> pg_backend_pid()" > /dev/null || true
LOCKRC=0
wait "$LOCKRUN" || LOCKRC=$?
wait "$GATE" 2> /dev/null || true
check "a backup whose database session was terminated FAILS (exit non-zero)" test "$LOCKRC" != 0
check "...with a log line that says the lock or session was lost" grep -Eq 'lock was lost|session ended|session did not answer' "$WORK/backup-lock-lost.log"
check "...publishing nothing" test "$(backup_dirs)" = "$dirs_before"
check "...and the metrics report the failed run while the last success is unchanged" \
  test "$(metric tikkit_backup_last_run_success)" = 0 -a "$(metric tikkit_backup_last_success_timestamp_seconds)" = "$metric_before"
run_backup "$WORK/backup-with-lock.log" || { cat "$WORK/backup-with-lock.log" >&2; die "backup with the lock function failed"; }
LOCKED_STAMP=$(backup_dirs | tail -n 1)
src cp "backup:/backups/$LOCKED_STAMP" "$WORK/offsite/$LOCKED_STAMP" > /dev/null
check "the lock-protected backup recorded that it held the lock" \
  bash -c 'docker run --rm -i --user "$(id -u):$(id -g)" -v "$1":/identity:ro "$2" age -d -i /identity < "$3/manifest.txt.age" | grep -q "^screenshot_lock_held=yes$"' _ "$WORK/identity.txt" "$BACKUP_IMAGE" "$WORK/offsite/$LOCKED_STAMP"
check "no advisory lock is left after a successful run" \
  test "$(sql src -c "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND granted")" = 0

say "scheduler: a sidecar that comes back late catches up with the due slot"
# Last success 20 hours ago (inside the old 26-hour threshold), due time 10 minutes ago.
docker volume create "$SRC-sched-test" > /dev/null
DUE=$(date -u -d '10 minutes ago' +%H:%M 2> /dev/null || date -u -v-10M +%H:%M)
NOW_EPOCH=$(date +%s)
docker run --rm -v "$SRC-sched-test":/backups --entrypoint sh "$BACKUP_IMAGE" -c "mkdir -p /backups/.status && printf 'last_success=%s\n' $((NOW_EPOCH - 20 * 3600)) > /backups/.status/state"
SCREENSHOT_VOLUME=$(src ps -q api | xargs docker inspect -f '{{range .Mounts}}{{if eq .Destination "/var/lib/tikkit/screenshots"}}{{.Name}}{{end}}{{end}}')
docker run -d --name "$SRC-sched-test" --network "${SRC}_db" \
  -e BACKUP_AGE_RECIPIENTS="$RECIPIENT" -e BACKUP_TIMES_UTC="$DUE" -e PGHOST=postgres -e PGUSER=tikkit -e PGDATABASE=tikkit \
  -e PGPASSWORD="$(sed -n 's/^TIKKIT_DB_PASSWORD=//p' "$WORK/source.env")" \
  -v "$SRC-sched-test":/backups -v "$SCREENSHOT_VOLUME":/var/lib/tikkit/screenshots:ro "$BACKUP_IMAGE" > /dev/null
for _ in $(seq 1 60); do
  [ "$(docker run --rm -v "$SRC-sched-test":/backups --entrypoint sh "$BACKUP_IMAGE" -c 'ls /backups | grep -c "^2"')" -ge 1 ] && break
  sleep 2
done
check "20 hours after the last backup, with the due time 10 minutes ago, the sidecar backs up at once (not the next day)" \
  test "$(docker run --rm -v "$SRC-sched-test":/backups --entrypoint sh "$BACKUP_IMAGE" -c 'ls /backups | grep -c "^2"')" -ge 1
docker rm -f "$SRC-sched-test" > /dev/null
docker volume rm "$SRC-sched-test" > /dev/null

# Work after the backup is the data a restore loses (the RPO).
# (The source API stays in restore mode: SMTP is enabled and must never be tried.)
src_seed up -d --wait --no-deps api
wait_ready "$BASE_SRC"
for user in admin req1; do login src "$BASE_SRC" "$user"; done
LATE=$(submit req1 "Created after the backup")
echo "$LATE" > "$WORK/ids/t_late"

# --- restore: refusals first ------------------------------------------------------

say "restore safety: a pre-existing volume, an old API image"
docker volume create "${DST}_screenshots-data" > /dev/null
docker run --rm --user 0 -v "${DST}_screenshots-data":/v --entrypoint sh "$BACKUP_IMAGE" -c 'echo precious > /v/keep.txt'
restore_refused() { # <pattern> <backup> <env file>
  local out
  out=$(ops/backup/restore.sh --backup "$2" --identity "$WORK/identity.txt" --project "$DST" --env-file "$3" --image "$BACKUP_IMAGE" 2>&1) && return 1
  grep -q "$1" <<< "$out"
}
check "restore refuses a project whose volume already exists by name, even without Compose labels" \
  restore_refused "exists: volume ${DST}_screenshots-data" "$WORK/offsite/$STAMP" "$WORK/restore.env"
check "...and the existing volume is untouched" \
  test "$(docker run --rm --user 0 -v "${DST}_screenshots-data":/v --entrypoint sh "$BACKUP_IMAGE" -c 'ls /v')" = keep.txt
check "...and nothing else was created (no containers, no reservation left behind)" \
  test -z "$(docker ps -aq --filter label=com.docker.compose.project=$DST)$(docker network ls -q --filter name=${DST}-reserved)"
docker volume rm "${DST}_screenshots-data" > /dev/null
sed "s#^TIKKIT_API_IMAGE=.*#TIKKIT_API_IMAGE=$BACKUP_IMAGE#" "$WORK/restore.env" > "$WORK/old-image.env"
check "restore refuses an API image that cannot confirm restore mode, before any container exists" \
  restore_refused "does not support restore mode" "$WORK/offsite/$STAMP" "$WORK/old-image.env"
check "...and left no containers or volumes" \
  test -z "$(docker ps -aq --filter label=com.docker.compose.project=$DST)$(docker volume ls -q --filter label=com.docker.compose.project=$DST)"
if [ -n "${DRILL_OLD_API_IMAGE:-}" ]; then
  sed "s#^TIKKIT_API_IMAGE=.*#TIKKIT_API_IMAGE=$DRILL_OLD_API_IMAGE#" "$WORK/restore.env" > "$WORK/old-api.env"
  check "restore refuses a real older API image that ignores RESTORE_MODE" \
    restore_refused "does not support restore mode" "$WORK/offsite/$STAMP" "$WORK/old-api.env"
fi

say "restoring into the isolated project ($DST) on ports $DST_HTTPS/$DST_HTTP"
RESTORE_START=$(date +%s)
restore_into_dst "$WORK/offsite/$STAMP" "$WORK/identity.txt" --config-out "$WORK/restored-config" | tee "$WORK/restore.out" | tail -n 1 > "$WORK/restore.summary" \
  || die "restore failed"
mark restore_total "$(($(date +%s) - RESTORE_START))s"
mark restore_detail "$(cat "$WORK/restore.summary")"
check "restore left the backup directory exactly as it was (no decrypted metadata written into it)" \
  test "$(ls -A "$WORK/offsite/$STAMP")" = "$BACKUP_LISTING"
check "main: restore reports no rows without a file and no files without a row (bulk files have no row, by design)" \
  grep -q "screenshot_rows_without_file=0 screenshot_files_without_row=$((BULK_MB / 5)) " "$WORK/restore.summary"
check "main: the email cutover suppressed exactly the email rows pending at the backup" \
  grep -q "email_suppressed=$SRC_PENDING_EMAIL\$" "$WORK/restore.summary"
fetch_ca dst "$DST_HTTPS"
wait_ready "$BASE_DST"

verify_restored() { # <label>
  local dir=$WORK/capture/restored-$1
  capture dst "$BASE_DST" dst "$dir"
  check "$1: ticket content, activity, operator views and AI-solved rules match the source" \
    bash -c 'cd "$1" && for f in *; do [ "$f" = outbox.txt ] || [ "$f" = volume-files.txt ] || [ "$f" = aggregates.txt ] || [ "$f" = screenshot-downloads.txt ] || diff -u "$2/$f" "$f" || exit 1; done' _ "$dir" "$WORK/capture/source"
  check "$1: report and aggregate statistics are equal (the cutover's own audit event aside)" \
    bash -c 'diff -u <(grep -v "^audit|" "$1") <(grep -v "^audit|" "$2")' _ "$WORK/capture/source/aggregates.txt" "$dir/aggregates.txt"
  check "$1: screenshot files match the source by SHA-256" diff -u "$WORK/capture/source/volume-files.txt" "$dir/volume-files.txt"
  check "$1: screenshot bytes served through the API match the source" diff -u "$WORK/capture/source/screenshot-downloads.txt" "$dir/screenshot-downloads.txt"
  check "$1: notification outbox rows are carried over (suppressed ones counted as the pending rows they were)" diff -u "$WORK/capture/source/outbox.txt" "$dir/outbox.txt"
}

verify_restored main
check "main: no restored email row is pending; in-portal rows stayed pending untouched" \
  test "$(sql dst -c "SELECT count(*) FROM notification_outbox WHERE status = 'pending' AND channel = 'email'")" = 0 \
  -a "$(sql dst -c "SELECT count(*) FROM notification_outbox WHERE status = 'pending' AND channel = 'in_app'")" = "$SRC_PENDING_INAPP"
check "main: the cutover is one audit event with the cutoff and counts only" \
  test "$(sql dst -c "SELECT count(*) FROM audit_events WHERE event_type = 'notifications_suppressed_on_restore' AND metadata ? 'cutoff' AND (metadata->>'email_suppressed')::int = $SRC_PENDING_EMAIL")" = 1
for f in queue-general.json queue-team.json; do
  check "main: AI-solved ticket is absent from the operator queue in $f" \
    bash -c '! jq -e --arg id "$2" "[.. | .id? // empty] | index(\$id)" "$1" > /dev/null' _ "$WORK/capture/restored-main/$f" "$(cat "$WORK/ids/t_ai_solved")"
done
check "main: AI-solved ticket is final for requester and refused to staff" \
  bash -c 'jq -e ".status == \"ai_solved\"" "$1/ticket-ai_solved.json" > /dev/null && { grep -qx 404 "$1/operator-code-ai_solved.txt" || grep -qx 403 "$1/operator-code-ai_solved.txt"; }' _ "$WORK/capture/restored-main"
check "main: the ticket created after the backup is not in the restore (recovery point)" \
  test "$(code_of "$(jar_of dst req1)" "$BASE_DST" GET "/api/tickets/$(cat "$WORK/ids/t_late")")" = 404
dst_logs_have() { local out; out=$(dst logs api 2>&1); grep -q "$1" <<< "$out"; }
check "main: restored API logs that RESTORE_MODE is on" dst_logs_have "RESTORE_MODE is on"
dst_networks() {
  docker inspect -f '{{range $n, $v := .NetworkSettings.Networks}}{{$n}}{{"\n"}}{{end}}' "$(dst ps -q api)" | grep -v '^$' | sed "s/^${DST}_//" | sort | tr '\n' ' '
}
check "main: restored API is attached to the internal networks only (no egress network)" test "$(dst_networks)" = "app db "
check "main: /api/ready reports restore_mode=true" \
  bash -c 'curl -sS --cacert "$1" "$2/api/ready" | jq -e ".restore_mode == true" > /dev/null' _ "$WORK/ca/$DST_HTTPS.crt" "$BASE_DST"
dst_metrics() { local out; out=$(dst exec -T api curl -fsS http://127.0.0.1:9091/metrics); grep -qx "$1" <<< "$out"; }
check "main: the metrics expose tikkit_restore_mode 1" dst_metrics "tikkit_restore_mode 1"
check "main: an Admin sees the restore mode banner (page rendered for the signed-in Admin)" \
  bash -c 'curl -sS --cacert "$1" -b "$2" "$3/?lang=en" | grep -q "Restore mode is on"' _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst admin)" "$BASE_DST"
check "main: a requester does not see the Admin banner" \
  bash -c '! curl -sS --cacert "$1" -b "$2" "$3/?lang=en" | grep -q "Restore mode is on"' _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst req1)" "$BASE_DST"
check "main: provider and mail connection tests are refused" \
  test "$(code_of "$(jar_of dst admin)" "$BASE_DST" POST /api/settings/ai/test)" = 409
RESETS_BEFORE=$(sql dst -c "SELECT count(*) FROM password_resets")
check "main: password recovery is refused identically for a known and an unknown user" \
  test "$(code_of "$WORK/jars/anon" "$BASE_DST" POST /api/auth/recovery '{"username":"req1"}')$(code_of "$WORK/jars/anon" "$BASE_DST" POST /api/auth/recovery '{"username":"nobody-here"}')" = 409409
check "main: ...and created no reset token" \
  test "$(sql dst -c "SELECT count(*) FROM password_resets")" = "$RESETS_BEFORE"
say "waiting 25s: restored jobs must not run (outbox, AI attempt, deadlines)"
sleep 25
capture dst "$BASE_DST" dst "$WORK/capture/restored-main-later"
check "main: pending notifications stayed as they were (nothing was sent)" diff -u "$WORK/capture/source/outbox.txt" "$WORK/capture/restored-main-later/outbox.txt"
check "main: the in-flight AI attempt made no provider call (attempt still processing, no usage)" \
  bash -c 'diff -u <(grep -v "^audit|" "$1") <(grep -v "^audit|" "$2")' _ "$WORK/capture/source/aggregates.txt" "$WORK/capture/restored-main-later/aggregates.txt"
dst_creds() { dst run --rm -T --no-deps api credentials-check 2> /dev/null | tail -n 1; }
check "main: stored credentials are readable with the same ENCRYPTION_KEY (read back by the API image)" \
  test "$(dst_creds)" = "ai_credentials=readable smtp_credentials=readable"
check "main: ...and through the Admin API" \
  bash -c 'curl -sS --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/settings/ai" | jq -e "[.providers[].credential | select(. != null) | .readable] | length > 0 and all" > /dev/null' _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst admin)" "$BASE_DST"

say "restore of a backup with zero screenshots (an installation that never had one)"
teardown_restore
ZERO_RESTORE_OUT=$(restore_into_dst "$WORK/offsite/$ZERO" "$WORK/identity.txt" 2> "$WORK/restore-zero.err" | tail -n 1) \
  || { cat "$WORK/restore-zero.err" >&2; die "restoring a backup with zero screenshots failed"; }
check "a valid backup without any screenshot restores (empty file list is not corruption)" \
  grep -q 'screenshot_rows_without_file=0 screenshot_files_without_row=0' <<< "$ZERO_RESTORE_OUT"
teardown_restore

say "failure simulation: backup decryption key missing"
check "a wrong or missing backup decryption key stops the restore before anything is created" \
  bash -c '! ops/backup/restore.sh --backup "$1" --identity "$2" --project "$3" --env-file "$4" --image "$5" > /dev/null 2>&1 && test -z "$(docker ps -aq --filter label=com.docker.compose.project=$3)$(docker volume ls -q --filter label=com.docker.compose.project=$3)$(docker network ls -q --filter name=$3)"' \
    _ "$WORK/offsite/$STAMP" "$WORK/other-identity.txt" "$DST" "$WORK/restore.env" "$BACKUP_IMAGE"

say "failure simulation: ENCRYPTION_KEY missing at startup"
check "the API refuses to start without ENCRYPTION_KEY and says why (no silent fallback)" \
  bash -c 'out=$(docker run --rm -e DATABASE_URL=postgres://x:x@127.0.0.1/x -e PUBLIC_ORIGIN=https://localhost -e SESSION_SECRET="$1" "$2" 2>&1); [ $? -ne 0 ] && grep -q ENCRYPTION_KEY <<< "$out"' _ "$(openssl rand -base64 32)" "$API_IMAGE"

if [ "$MODE" = full ]; then
  say "failure simulation: wrong ENCRYPTION_KEY after restore"
  DST_ENV=$WORK/wrongkey.env
  restore_into_dst "$WORK/offsite/$STAMP" "$WORK/identity.txt" > "$WORK/restore-wrongkey.out" 2> "$WORK/restore-wrongkey.err" || die "restore with a different ENCRYPTION_KEY failed"
  check "wrong key: restore itself warns that stored credentials are unreadable" \
    grep -q 'stored credentials with this ENCRYPTION_KEY: ai_credentials=unreadable smtp_credentials=unreadable' "$WORK/restore-wrongkey.err"
  fetch_ca dst "$DST_HTTPS"
  wait_ready "$BASE_DST"
  for user in admin req1; do login dst "$BASE_DST" "$user"; done
  check "wrong key: tickets and screenshots remain fully usable" \
    bash -c 'curl -sS --fail --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/tickets/$4" | jq -e ".status == \"queued\"" > /dev/null' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst req1)" "$BASE_DST" "$(cat "$WORK/ids/t_open")"
  check "wrong key: stored AI credential is reported unreadable and AI is unavailable" \
    bash -c 'curl -sS --fail --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/settings/ai" | jq -e "([.providers[].credential | select(. != null) | .readable] | length > 0 and all(not)) and (.status != \"ready\")" > /dev/null && curl -sS --fail --cacert "$1" -b "$4" -H "X-Requested-With: tikkit" "$3/api/ai/policy" | jq -e ".ai_assistance == false" > /dev/null' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst admin)" "$BASE_DST" "$(jar_of dst req1)"
  check "wrong key: the read-back says AI and SMTP credentials are unreadable" \
    test "$(dst_creds)" = "ai_credentials=unreadable smtp_credentials=unreadable"
  check "wrong key: re-entering the AI key saves" \
    test "$(code_of "$(jar_of dst admin)" "$BASE_DST" PUT /api/settings/ai/credentials/openai "$(jq -n --arg k "sk-drill-$(openssl rand -hex 16)" '{api_key:$k}')")" = 200
  check "wrong key: the SMTP password alone is not enough, the username is encrypted too" \
    test "$(code_of "$(jar_of dst admin)" "$BASE_DST" PUT /api/settings/smtp \
      '{"host":"smtp.invalid","port":587,"security":"starttls","sender_name":"Tikkit","sender_address":"help@example.invalid","password":"drill-smtp-password-2","enabled":false}')" = 200 \
    -a "$(dst_creds)" = "ai_credentials=readable smtp_credentials=unreadable"
  check "wrong key: entering SMTP username and password makes both credentials readable again" \
    test "$(code_of "$(jar_of dst admin)" "$BASE_DST" PUT /api/settings/smtp \
      '{"host":"smtp.invalid","port":587,"security":"starttls","sender_name":"Tikkit","sender_address":"help@example.invalid","username":"mailer","password":"drill-smtp-password-2","enabled":false}')" = 200 \
    -a "$(dst_creds)" = "ai_credentials=readable smtp_credentials=readable"
  teardown_restore
  unset DST_ENV

  say "failure simulation: a screenshot file is missing (and a directory takes its place)"
  MISSING=$(sed -n 1p "$WORK/ids/screenshots")
  src exec -T api sh -c "rm -f /var/lib/tikkit/screenshots/$MISSING && mkdir /var/lib/tikkit/screenshots/$MISSING"
  src exec -T backup sh -c 'mkdir -m 700 /backups/20200201T000000Z'
  run_backup "$WORK/backup-missing.log" || die "backup with a missing file should still succeed"
  check "backup warns about the row without a file and calls itself degraded" grep -q 'this backup is degraded and rotates nothing' "$WORK/backup-missing.log"
  check "a degraded backup rotates nothing (an old directory past retention is still there)" \
    test "$(src exec -T backup sh -c 'ls /backups | grep -c "^20200201"')" = 1
  check "backup exposes tikkit_backup_screenshots_missing=1" test "$(metric tikkit_backup_screenshots_missing)" = 1
  STAMP2=$(backup_dirs | grep -v '^2020' | tail -n 1)
  src cp "backup:/backups/$STAMP2" "$WORK/offsite/$STAMP2" > /dev/null
  check "the degraded backup is marked for operators without the key" test -f "$WORK/offsite/$STAMP2/DEGRADED"
  check "restore refuses a backup with missing screenshot files by default" \
    bash -c '! ops/backup/restore.sh --backup "$1" --identity "$2" --project "$3" --env-file "$4" --image "$5" > /dev/null 2>&1' \
      _ "$WORK/offsite/$STAMP2" "$WORK/identity.txt" "$DST" "$WORK/restore.env" "$BACKUP_IMAGE"
  teardown_restore
  restore_into_dst "$WORK/offsite/$STAMP2" "$WORK/identity.txt" --allow-missing-files > "$WORK/restore-missing.out" 2> "$WORK/restore-missing.err" || die "restore with --allow-missing-files failed"
  check "missing file: restore reports exactly one row without a file, by id (a directory with that name is not a file)" \
    bash -c 'grep -q "screenshot_rows_without_file=1" "$1" && grep -q "rows without a file (previews" "$2" && grep -q "$3" "$2"' _ "$WORK/restore-missing.out" "$WORK/restore-missing.err" "$MISSING"
  fetch_ca dst "$DST_HTTPS"
  wait_ready "$BASE_DST"
  for user in admin req1 req2 req3; do login dst "$BASE_DST" "$user"; done
  check "missing file: the ticket itself restores intact" \
    bash -c 'curl -sS --fail --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/tickets/$4" | jq -e ".status == \"queued\"" > /dev/null' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst req1)" "$BASE_DST" "$(cat "$WORK/ids/t_open")"
  check "missing file: only that screenshot fails (500), the others are served" \
    bash -c 'a=$(curl -sS -o /dev/null -w "%{http_code}" --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/screenshots/$4/download"); b=$(curl -sS -o /dev/null -w "%{http_code}" --cacert "$1" -b "$5" -H "X-Requested-With: tikkit" "$3/api/screenshots/$6/download"); [ "$a" = 500 ] && [ "$b" = 200 ]' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst req1)" "$BASE_DST" "$MISSING" "$(jar_of dst req2)" "$(sed -n 2p "$WORK/ids/screenshots")"
  teardown_restore
fi

DRILL_SECONDS=$(($(date +%s) - DRILL_START))
mark drill_total "${DRILL_SECONDS}s"
echo
echo "drill: PASSED $PASSED checks ($MODE mode)"
sed 's/^/drill: /' "$TIMES"
