#!/usr/bin/env bash
# Restores one encrypted backup into a NEW, isolated Compose project (fresh volumes,
# RESTORE_MODE on, no egress network, proxy on loopback). docs/backup-restore.md.
#
#   ops/backup/restore.sh --backup /path/to/20261118T031500Z --identity /path/to/key.txt \
#       --project tikkit-restore --env-file .env.restore [--image tikkit-backup:local] \
#       [--config-out DIR] [--allow-missing-files] [--keep-pending-email] [--no-start]
#
# The backup directory and the age identity (private key) stay on the host; the
# identity is mounted read-only into short-lived age containers and decrypted data
# is streamed straight into PostgreSQL and the new screenshot volume, never to disk.
# Needs Docker with Compose v2.33.1+ and jq; run it from the deployment directory.
set -euo pipefail
umask 077
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=hostlib.sh
. "$here/hostlib.sh"

backup= identity= project= env_file= config_out= allow_missing=0 keep_email=0 start=1
image=${TIKKIT_BACKUP_IMAGE:-tikkit-backup:local}
files=(-f docker-compose.prod.example.yml -f docker-compose.restore.example.yml)

die() { echo "restore: $*" >&2; exit 1; }
step() { echo "restore: $*" >&2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --backup) backup=$2; shift 2 ;;
    --identity) identity=$2; shift 2 ;;
    --project) project=$2; shift 2 ;;
    --env-file) env_file=$2; shift 2 ;;
    --image) image=$2; shift 2 ;;
    --config-out) config_out=$2; shift 2 ;;
    --allow-missing-files) allow_missing=1; shift ;;
    --keep-pending-email) keep_email=1; shift ;;
    --no-start) start=0; shift ;;
    *) die "unknown option $1" ;;
  esac
done
[ -d "$backup" ] && [ -f "$identity" ] && [ -f "$env_file" ] && [ -n "$project" ] \
  || die "usage: restore.sh --backup DIR --identity FILE --project NAME --env-file FILE"
command -v jq > /dev/null || die "jq is required"
backup=$(cd "$backup" && pwd)
identity=$(cd "$(dirname "$identity")" && pwd)/$(basename "$identity")

compose() { dc --env-file "$env_file" -p "$project" "${files[@]}" "$@"; }
age_container() {
  docker run --rm -i --user "$(id -u):$(id -g)" -v "$identity":/identity:ro "$image" "$@"
}
decrypt() { age_container age -d -i /identity; }
now() { date +%s; }

# 0. Claim the project name, then refuse if ANY resource this project would use
#    already exists, by exact name (a non-empty volume without Compose labels counts).
reserved=0
trap '[ "$reserved" = 0 ] || release_project "$project"' EXIT
reserve_project "$project" || die "project '$project' is already reserved or in use (network $project-reserved exists); restore needs a fresh project"
reserved=1
occupied=$(project_occupied "$env_file" "$project" "${files[@]}")
if [ -n "$occupied" ]; then
  printf 'restore:   exists: %s\n' "$occupied" >&2
  die "project '$project' already has the resources listed above; restore needs a fresh project name (nothing was changed)"
fi

started=$(now)
step "1/8 verifying the backup (checksums, then decryption with the key)"
docker run --rm --user "$(id -u):$(id -g)" -v "$backup":/backup:ro -v "$identity":/identity:ro \
  -e BACKUP_IDENTITY_FILE=/identity "$image" verify /backup

manifest=$(decrypt < "$backup/manifest.txt.age")
value() { printf '%s\n' "$manifest" | sed -n "s/^$1=//p"; }
echo "restore: backup $(value created_utc), migration $(value migration_version), $(value tickets) tickets, $(value screenshot_files) screenshot files" >&2

step "2/8 checking that the API image supports restore mode (before anything serves)"
api_image=$(compose config --format json | jq -r '.services.api.image')
envfile=$(mktemp)
trap 'rm -f "$envfile"; [ "$reserved" = 0 ] || release_project "$project"' EXIT
compose config --format json | jq -r '.services.api.environment | to_entries[] | "\(.key)=\(.value)"' > "$envfile"
check=$(docker run --rm --env-file "$envfile" "$api_image" restore-check 2>&1 || true)
rm -f "$envfile"
[ "$check" = "restore_mode=on" ] \
  || die "the API image '$api_image' does not support restore mode (restore-check said: ${check:-nothing}). Use an image built from a release that includes restore mode; older images would start the notification, AI and timeout workers on the restored data."

step "3/8 starting PostgreSQL in project $project (empty volume, fresh)"
compose up -d --wait postgres

step "4/8 restoring the database"
db_started=$(now)
decrypt < "$backup/database.pgdump.age" \
  | compose exec -T postgres pg_restore -U postgres -d tikkit --exit-on-error --single-transaction
db_seconds=$(($(now) - db_started))

step "5/8 restoring the screenshot files"
files_started=$(now)
decrypt < "$backup/screenshots.tar.age" \
  | compose run --rm -T --no-deps --entrypoint tar api --no-same-owner -C /var/lib/tikkit/screenshots -xf -
files_seconds=$(($(now) - files_started))

step "6/8 migration history, credentials and the email cutover"
compose run --rm -T --no-deps api migrate
creds=$(compose run --rm -T --no-deps api credentials-check 2> /dev/null | tail -n 1)
echo "restore: stored credentials with this ENCRYPTION_KEY: $creds" >&2
case $creds in
  *unreadable*) echo "restore: WARNING: some stored credentials cannot be read with this ENCRYPTION_KEY; the Admin must enter them again (docs/backup-restore.md#when-encryption_key-is-missing-or-wrong-after-a-restore)" >&2 ;;
esac
email_suppressed=0
if [ "$keep_email" = 0 ]; then
  # A relay may already have accepted what the database still shows as pending.
  cutoff_epoch=$(value snapshot_epoch)
  cutoff=$(date -u -d "@$cutoff_epoch" +%Y-%m-%dT%H:%M:%SZ 2> /dev/null || date -u -r "$cutoff_epoch" +%Y-%m-%dT%H:%M:%SZ)
  cut=$(compose run --rm -T --no-deps api suppress-restored-email --before "$cutoff" 2> /dev/null | tail -n 1)
  echo "restore: email cutover (pending at $cutoff suppressed, never sent): $cut" >&2
  email_suppressed=$(printf '%s\n' "$cut" | sed -n 's/.*email_suppressed=\([0-9]*\).*/\1/p')
  [ -n "$email_suppressed" ] || die "the email cutover did not report a count"
else
  echo "restore: --keep-pending-email: pending email rows stay pending and WILL be sent when jobs start" >&2
fi

step "7/8 consistency between database rows and files"
declared=$(value screenshot_files)
if [ "$declared" -gt 0 ]; then
  # Fails (non-zero) on any mismatch or unreadable file.
  if ! changed=$(printf '%s\n' "$manifest" | sed '1,/^--files--$/d' \
    | compose run --rm -T --no-deps --entrypoint sh api -c 'cd /var/lib/tikkit/screenshots && sha256sum -c --quiet -' 2>&1); then
    printf '%s\n' "$changed" >&2
    die "restored screenshot files do not match the manifest checksums"
  fi
fi
rows=$(compose exec -T postgres psql -U postgres -d tikkit -Atc "SELECT id FROM screenshots" | tr -d '\r' | sort)
# Inventory of regular files by relative path (a directory named like an id is not a file).
present=$(compose run --rm -T --no-deps --entrypoint find api /var/lib/tikkit/screenshots -type f ! -name '*.tmp' -printf '%P\n' | tr -d '\r' | sort)
[ "$(printf '%s\n' "$present" | grep -c .)" = "$declared" ] \
  || die "the restored volume has $(printf '%s\n' "$present" | grep -c .) files but the manifest declares $declared"
missing=$(comm -23 <(printf '%s\n' "$rows" | grep .) <(printf '%s\n' "$present" | grep .) || true)
extra=$(comm -13 <(printf '%s\n' "$rows" | grep .) <(printf '%s\n' "$present" | grep .) || true)
missing_count=$(printf '%s\n' "$missing" | grep -c . || true)
extra_count=$(printf '%s\n' "$extra" | grep -c . || true)
echo "restore: consistency report: rows without a file: $missing_count; files without a row: $extra_count" >&2
if [ "$extra_count" -gt 0 ]; then
  echo "restore:   files without a row (harmless; swept 24 hours after leaving restore mode):" >&2
  printf '%s\n' "$extra" | head -n 20 | sed 's/^/restore:     /' >&2
  [ "$extra_count" -le 20 ] || echo "restore:     ... and $((extra_count - 20)) more" >&2
fi
if [ "$missing_count" -gt 0 ]; then
  echo "restore:   rows without a file (previews and downloads of these return 500):" >&2
  printf '%s\n' "$missing" | head -n 20 | sed 's/^/restore:     /' >&2
  [ "$missing_count" -le 20 ] || echo "restore:     ... and $((missing_count - 20)) more" >&2
  [ "$allow_missing" = 1 ] || die "$missing_count screenshot rows have no file. The tickets are intact. Rerun with a fresh project and --allow-missing-files to accept this, or restore an older backup that has the files"
fi

if [ -n "$config_out" ]; then
  mkdir -p "$config_out" && chmod 700 "$config_out"
  decrypt < "$backup/config.tar.age" | tar -xf - -C "$config_out"
  step "non-secret deployment configuration extracted to $config_out"
fi

if [ "$start" = 1 ]; then
  step "8/8 starting the isolated copy (RESTORE_MODE=true, no egress network)"
  compose up -d --wait
  # The running API must itself report restore mode.
  compose exec -T api curl -fsS http://127.0.0.1:3001/api/ready | jq -e '.restore_mode == true' > /dev/null \
    || { compose stop api web proxy > /dev/null 2>&1 || true; die "the running API does not report restore_mode=true; it was stopped"; }
else
  step "8/8 not starting the application services (--no-start)"
fi
total=$(($(now) - started))
echo "restore: done in ${total}s (database ${db_seconds}s, screenshots ${files_seconds}s). Do the post-restore checklist in docs/backup-restore.md before any live work." >&2
echo "restore_seconds=$total database_restore_seconds=$db_seconds screenshots_restore_seconds=$files_seconds screenshot_rows_without_file=$missing_count screenshot_files_without_row=$extra_count email_suppressed=$email_suppressed"
