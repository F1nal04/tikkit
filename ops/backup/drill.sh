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
#   DRILL_SOURCE_PROJECT   default tikkit-m4-47            (source stack)
#   DRILL_RESTORE_PROJECT  default tikkit-m4-47-restore     (isolated restore stack)
#   DRILL_PORT_BASE        default 18444 (uses +0/+1 HTTPS, +36/+37 HTTP)
#   DRILL_MODE             full (default) | reduced (no wrong-key and missing-file restores)
#   DRILL_WORKDIR          scratch directory (default: a new temp dir, removed unless DRILL_KEEP=1)
#   DRILL_BULK_MB          default 0; adds about this much random data to the database and to the
#                          screenshot volume (a throughput measurement for RPO/RTO sizing)
#   DRILL_KEEP=1           leave both projects and the work directory in place
set -euo pipefail
cd "$(dirname "$0")/../.."

SRC=${DRILL_SOURCE_PROJECT:-tikkit-m4-47}
DST=${DRILL_RESTORE_PROJECT:-tikkit-m4-47-restore}
PORT=${DRILL_PORT_BASE:-18444}
SRC_HTTPS=$PORT SRC_HTTP=$((PORT + 36)) DST_HTTPS=$((PORT + 1)) DST_HTTP=$((PORT + 37))
API_IMAGE=${DRILL_API_IMAGE:?set DRILL_API_IMAGE to the API image under test}
WEB_IMAGE=${DRILL_WEB_IMAGE:?set DRILL_WEB_IMAGE to the web image under test}
BACKUP_IMAGE=${DRILL_BACKUP_IMAGE:-tikkit-drill-backup:local}
MODE=${DRILL_MODE:-full}
KEEP=${DRILL_KEEP:-0}
BULK_MB=${DRILL_BULK_MB:-0}
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
ENV
}

# --- compose helpers -------------------------------------------------------------

BASE_FILES=(-f docker-compose.prod.example.yml -f docker-compose.backup.example.yml)
src() { docker compose --env-file "$WORK/source.env" -p "$SRC" "${BASE_FILES[@]}" "$@"; }
src_seed() { docker compose --env-file "$WORK/source.env" -p "$SRC" "${BASE_FILES[@]}" -f docker-compose.restore.example.yml "$@"; }
dst() { docker compose --env-file "${DST_ENV:-$WORK/restore.env}" -p "$DST" -f docker-compose.prod.example.yml -f docker-compose.restore.example.yml "$@"; }
sql() { local stack=$1; shift; "$stack" exec -T postgres psql -U postgres -d tikkit -v ON_ERROR_STOP=1 -Atq "$@"; }

cleanup() {
  local rc=$?
  if [ "$KEEP" != 1 ]; then
    src down --volumes --remove-orphans > /dev/null 2>&1 || true
    dst down --volumes --remove-orphans > /dev/null 2>&1 || true
    rm -rf "$WORK"
  else
    say "left in place: projects $SRC and $DST, work directory $WORK"
  fi
  [ "$rc" = 0 ] || say "drill FAILED (exit $rc)"
  exit "$rc"
}
trap cleanup EXIT

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
  sql "$fn" -c "SELECT status, channel, count(*) FROM notification_outbox GROUP BY 1,2 ORDER BY 1,2" > "$dir/outbox.txt"
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

# The two project names belong to the drill: start from nothing.
src down --volumes --remove-orphans > /dev/null 2>&1 || true
dst down --volumes --remove-orphans > /dev/null 2>&1 || true

say "building the backup image and generating the backup key pair"
docker build -q -f ops/backup/Dockerfile -t "$BACKUP_IMAGE" . > /dev/null
docker run --rm "$BACKUP_IMAGE" age-keygen 2> /dev/null > "$WORK/identity.txt"
chmod 600 "$WORK/identity.txt"
RECIPIENT=$(grep -o 'age1[a-z0-9]*' "$WORK/identity.txt" | head -1)
sed -i.bak "s/^BACKUP_AGE_RECIPIENTS=.*/BACKUP_AGE_RECIPIENTS=$RECIPIENT/" "$WORK/source.env" && rm -f "$WORK/source.env.bak"
# A second key pair stands for "the wrong key / the key is lost".
docker run --rm "$BACKUP_IMAGE" age-keygen 2> /dev/null > "$WORK/other-identity.txt"

say "starting the source stack ($SRC) on ports $SRC_HTTPS/$SRC_HTTP"
timed source_stack_start compose_up_source
fetch_ca src "$SRC_HTTPS"
wait_ready "$BASE_SRC"
check "backup sidecar exposes its status metrics" \
  src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics
for _ in $(seq 1 30); do
  [ "$(src exec -T backup sh -c 'ls /backups | grep -c "^2"')" -ge 1 ] && break
  sleep 2
done
check "sidecar took a first backup on its own (no recent backup at start)" \
  test "$(src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics | sed -n 's/^tikkit_backup_last_run_success //p')" = 1

say "seeding mixed ticket outcomes"
timed seed_phase_a seed_phase_a
timed seed_phase_b seed_phase_b
timed seed_bulk seed_bulk
capture src "$BASE_SRC" src "$WORK/capture/source"
check "source screenshots are all served (200) through the API" \
  test "$(grep -c '^200 ' "$WORK/capture/source/screenshot-downloads.txt")" = 4
check "source has all five outcome kinds" \
  test "$(sql src -c "SELECT count(DISTINCT coalesce(resolution_source::text, status)) FROM tickets")" -ge 5

# Retention: dated placeholders older than BACKUP_RETENTION_DAYS go, the newest copies stay.
src exec -T backup sh -c 'mkdir -m 700 /backups/20200101T000000Z /backups/20200102T000000Z /backups/20200103T000000Z'

say "taking the backup"
BACKUP_START=$(date +%s)
src exec -T backup /opt/tikkit-backup/entrypoint.sh run 2> "$WORK/backup.log" || { cat "$WORK/backup.log" >&2; die "backup run failed"; }
BACKUP_SECONDS=$(($(date +%s) - BACKUP_START))
mark backup_run "${BACKUP_SECONDS}s"
STAMP=$(src exec -T backup sh -c 'ls /backups | grep "^2" | sort | tail -n 1' | tr -d '\r')
mark backup_size_bytes "$(src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics | sed -n 's/^tikkit_backup_last_size_bytes //p')"
check "old backups past retention were removed" test "$(src exec -T backup sh -c 'ls /backups | grep -c "^2020"')" = 0
check "newest backups were kept" test "$(src exec -T backup sh -c 'ls /backups | grep -c "^2026\|^20[3-9]"')" -ge 1
check "backup files are private (0600) and the directory 0700" \
  test "$(src exec -T backup sh -c "stat -c '%a' /backups/$STAMP /backups/$STAMP/*.age | sort -u | tr '\n' ' '")" = "600 700 "

# The off-host copy: any file copy of the directory works; here docker cp.
src cp "backup:/backups/$STAMP" "$WORK/offsite/$STAMP" > /dev/null
check "off-host copy verifies by checksum without the decryption key" \
  docker run --rm -v "$WORK/offsite/$STAMP":/backup:ro "$BACKUP_IMAGE" verify /backup
cp -R "$WORK/offsite/$STAMP" "$WORK/offsite/tampered"
printf 'x' >> "$WORK/offsite/tampered/screenshots.tar.age"
check "a damaged copy is rejected" \
  bash -c '! docker run --rm -v "$1":/backup:ro "$2" verify /backup > /dev/null 2>&1' _ "$WORK/offsite/tampered" "$BACKUP_IMAGE"
check "plaintext secrets are not in the encrypted files" \
  bash -c '! grep -aqF -e "$1" -e "drill-smtp-password" "$2"/*.age' _ "$ENCRYPTION_KEY_VALUE" "$WORK/offsite/$STAMP"

# Work after the backup is the data a restore loses (the RPO).
sql src -c "SELECT 1" > /dev/null
src up -d --wait --no-deps api
wait_ready "$BASE_SRC"
for user in admin req1; do login src "$BASE_SRC" "$user"; done
LATE=$(submit req1 "Created after the backup")
echo "$LATE" > "$WORK/ids/t_late"

say "restoring into the isolated project ($DST) on ports $DST_HTTPS/$DST_HTTP"
RESTORE_START=$(date +%s)
restore_into_dst "$WORK/offsite/$STAMP" "$WORK/identity.txt" --config-out "$WORK/restored-config" | tee "$WORK/restore.out" | tail -n 1 > "$WORK/restore.summary" \
  || die "restore failed"
mark restore_total "$(($(date +%s) - RESTORE_START))s"
mark restore_detail "$(cat "$WORK/restore.summary")"
check "main: restore reports no rows without a file and no files without a row" \
  grep -q 'screenshot_rows_without_file=0 screenshot_files_without_row=0' "$WORK/restore.summary"
fetch_ca dst "$DST_HTTPS"
wait_ready "$BASE_DST"

verify_restored() { # <label>
  local dir=$WORK/capture/restored-$1
  capture dst "$BASE_DST" dst "$dir"
  check "$1: ticket content, activity, operator views and AI-solved rules match the source" \
    bash -c 'cd "$1" && for f in *; do [ "$f" = outbox.txt ] || [ "$f" = volume-files.txt ] || [ "$f" = aggregates.txt ] || [ "$f" = screenshot-downloads.txt ] || diff -u "$2/$f" "$f" || exit 1; done' _ "$dir" "$WORK/capture/source"
  check "$1: report and aggregate statistics are equal" diff -u "$WORK/capture/source/aggregates.txt" "$dir/aggregates.txt"
  check "$1: screenshot files match the source by SHA-256" diff -u "$WORK/capture/source/volume-files.txt" "$dir/volume-files.txt"
  check "$1: screenshot bytes served through the API match the source" diff -u "$WORK/capture/source/screenshot-downloads.txt" "$dir/screenshot-downloads.txt"
  check "$1: notification outbox rows are carried over" diff -u "$WORK/capture/source/outbox.txt" "$dir/outbox.txt"
}

verify_restored main
check "main: AI-solved ticket is closed for requester and staff (final)" \
  bash -c 'jq -e ".status == \"ai_solved\"" "$1/ticket-ai_solved.json" > /dev/null && grep -qx 404 "$1/operator-code-ai_solved.txt" || grep -qx 403 "$1/operator-code-ai_solved.txt"' _ "$WORK/capture/restored-main"
check "main: AI-solved ticket is absent from operator queues" \
  bash -c '! jq -e --arg id "$2" "[.. | .id? // empty] | index(\$id)" "$1/queue-general.json" "$1/queue-team.json" > /dev/null' _ "$WORK/capture/restored-main" "$(cat "$WORK/ids/t_ai_solved")"
check "main: the ticket created after the backup is not in the restore (recovery point)" \
  test "$(code_of "$(jar_of dst req1)" "$BASE_DST" GET "/api/tickets/$(cat "$WORK/ids/t_late")")" = 404
dst_logs_have() { local out; out=$(dst logs api 2>&1); grep -q "$1" <<< "$out"; }
check "main: restored API logs that RESTORE_MODE is on" dst_logs_have "RESTORE_MODE is on"
dst_networks() {
  docker inspect -f '{{range $n, $v := .NetworkSettings.Networks}}{{$n}}{{"\n"}}{{end}}' "$(dst ps -q api)" | grep -v '^$' | sed "s/^${DST}_//" | sort | tr '\n' ' '
}
check "main: restored API is attached to the internal networks only (no egress network)" test "$(dst_networks)" = "app db "
check "main: provider and mail connection tests are refused" \
  test "$(code_of "$(jar_of dst admin)" "$BASE_DST" POST /api/settings/ai/test)" = 409
say "waiting 25s: restored jobs must not run (outbox, AI attempt, deadlines)"
sleep 25
capture dst "$BASE_DST" dst "$WORK/capture/restored-main-later"
check "main: pending notifications stayed pending (nothing was sent)" diff -u "$WORK/capture/source/outbox.txt" "$WORK/capture/restored-main-later/outbox.txt"
check "main: the in-flight AI attempt made no provider call (attempt still processing, no usage)" \
  diff -u "$WORK/capture/source/aggregates.txt" "$WORK/capture/restored-main-later/aggregates.txt"
check "main: stored credentials are readable with the same ENCRYPTION_KEY" \
  bash -c 'curl -sS --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/settings/ai" | jq -e "[.providers[].credential | select(. != null) | .readable] | length > 0 and all" > /dev/null' _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst admin)" "$BASE_DST"

say "failure simulation: backup decryption key missing"
teardown_restore
check "a wrong or missing backup decryption key stops the restore before anything is created" \
  bash -c '! ops/backup/restore.sh --backup "$1" --identity "$2" --project "$3" --env-file "$4" --image "$5" > /dev/null 2>&1 && test -z "$(docker ps -aq --filter label=com.docker.compose.project=$3)$(docker volume ls -q --filter label=com.docker.compose.project=$3)"' \
    _ "$WORK/offsite/$STAMP" "$WORK/other-identity.txt" "$DST" "$WORK/restore.env" "$BACKUP_IMAGE"

say "failure simulation: ENCRYPTION_KEY missing at startup"
check "the API refuses to start without ENCRYPTION_KEY and says why (no silent fallback)" \
  bash -c 'out=$(docker run --rm -e DATABASE_URL=postgres://x:x@127.0.0.1/x -e PUBLIC_ORIGIN=https://localhost -e SESSION_SECRET="$1" "$2" 2>&1); [ $? -ne 0 ] && grep -q ENCRYPTION_KEY <<< "$out"' _ "$(openssl rand -base64 32)" "$API_IMAGE"

if [ "$MODE" = full ]; then
  say "failure simulation: wrong ENCRYPTION_KEY after restore"
  DST_ENV=$WORK/wrongkey.env restore_into_dst "$WORK/offsite/$STAMP" "$WORK/identity.txt" > /dev/null || die "restore with a different ENCRYPTION_KEY failed"
  fetch_ca dst "$DST_HTTPS"
  wait_ready "$BASE_DST"
  for user in admin req1; do login dst "$BASE_DST" "$user"; done
  check "wrong key: tickets and screenshots remain fully usable" \
    bash -c 'curl -sS --fail --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/tickets/$4" | jq -e ".status == \"queued\"" > /dev/null' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst req1)" "$BASE_DST" "$(cat "$WORK/ids/t_open")"
  check "wrong key: stored AI credential is reported unreadable and AI is unavailable" \
    bash -c 'curl -sS --fail --cacert "$1" -b "$2" -H "X-Requested-With: tikkit" "$3/api/settings/ai" | jq -e "([.providers[].credential | select(. != null) | .readable] | length > 0 and all(not)) and (.status != \"ready\")" > /dev/null && curl -sS --fail --cacert "$1" -b "$4" -H "X-Requested-With: tikkit" "$3/api/ai/policy" | jq -e ".ai_assistance == false" > /dev/null' \
      _ "$WORK/ca/$DST_HTTPS.crt" "$(jar_of dst admin)" "$BASE_DST" "$(jar_of dst req1)"
  check "wrong key: the Admin can store a new AI key again" \
    test "$(code_of "$(jar_of dst admin)" "$BASE_DST" PUT /api/settings/ai/credentials/openai "$(jq -n --arg k "sk-drill-$(openssl rand -hex 16)" '{api_key:$k}')")" = 200
  check "wrong key: the Admin can store the SMTP password again" \
    test "$(code_of "$(jar_of dst admin)" "$BASE_DST" PUT /api/settings/smtp \
      '{"host":"smtp.invalid","port":587,"security":"starttls","sender_name":"Tikkit","sender_address":"help@example.invalid","password":"drill-smtp-password-2","enabled":false}')" = 200
  teardown_restore

  say "failure simulation: a screenshot file is missing"
  MISSING=$(sed -n 1p "$WORK/ids/screenshots")
  src exec -T api rm -f "/var/lib/tikkit/screenshots/$MISSING"
  src exec -T backup /opt/tikkit-backup/entrypoint.sh run 2> "$WORK/backup-missing.log" || die "backup with a missing file should still succeed"
  check "backup warns about the row without a file" grep -q 'screenshot rows have no file' "$WORK/backup-missing.log"
  check "backup exposes tikkit_backup_screenshots_missing=1" \
    test "$(src exec -T backup wget -q -O - http://127.0.0.1:9092/metrics | sed -n 's/^tikkit_backup_screenshots_missing //p' | tr -d '\r')" = 1
  STAMP2=$(src exec -T backup sh -c 'ls /backups | grep "^2" | sort | tail -n 1' | tr -d '\r')
  src cp "backup:/backups/$STAMP2" "$WORK/offsite/$STAMP2" > /dev/null
  check "restore refuses a backup with missing screenshot files by default" \
    bash -c '! ops/backup/restore.sh --backup "$1" --identity "$2" --project "$3" --env-file "$4" --image "$5" > /dev/null 2>&1' \
      _ "$WORK/offsite/$STAMP2" "$WORK/identity.txt" "$DST" "$WORK/restore.env" "$BACKUP_IMAGE"
  teardown_restore
  restore_into_dst "$WORK/offsite/$STAMP2" "$WORK/identity.txt" --allow-missing-files > "$WORK/restore-missing.out" 2> "$WORK/restore-missing.err" || die "restore with --allow-missing-files failed"
  check "missing file: restore reports exactly one row without a file, by id" \
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
