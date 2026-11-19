#!/usr/bin/env bash
# Restores one encrypted backup into a NEW, isolated Compose project (fresh volumes,
# RESTORE_MODE on, no egress network, proxy on loopback). docs/backup-restore.md.
#
#   ops/backup/restore.sh --backup /path/to/20261118T031500Z --identity /path/to/key.txt \
#       --project tikkit-restore --env-file .env.restore [--image tikkit-backup:local] \
#       [--config-out DIR] [--allow-missing-files] [--no-start]
#
# The backup directory and the age identity (private key) stay on the host; the
# identity is mounted read-only into short-lived age containers and decrypted data
# is streamed straight into PostgreSQL and the new screenshot volume, never to disk.
# Needs Docker with Compose v2.33.1+; run it from the deployment directory.
set -euo pipefail

backup= identity= project= env_file= config_out= allow_missing=0 start=1
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
    --no-start) start=0; shift ;;
    *) die "unknown option $1" ;;
  esac
done
[ -d "$backup" ] && [ -f "$identity" ] && [ -f "$env_file" ] && [ -n "$project" ] \
  || die "usage: restore.sh --backup DIR --identity FILE --project NAME --env-file FILE"
backup=$(cd "$backup" && pwd)
identity=$(cd "$(dirname "$identity")" && pwd)/$(basename "$identity")

compose() { docker compose --env-file "$env_file" -p "$project" "${files[@]}" "$@"; }
age_container() {
  docker run --rm -i --user "$(id -u):$(id -g)" -v "$identity":/identity:ro "$image" "$@"
}
decrypt() { age_container age -d -i /identity; }
now() { date +%s; }

if docker volume ls -q --filter "label=com.docker.compose.project=$project" | grep -q . \
  || [ -n "$(docker ps -aq --filter "label=com.docker.compose.project=$project")" ]; then
  die "project '$project' already has containers or volumes; restore needs a fresh project (remove it first if it is a previous restore)"
fi

started=$(now)
step "1/7 verifying the backup (checksums, then decryption with the key)"
docker run --rm --user "$(id -u):$(id -g)" -v "$backup":/backup:ro -v "$identity":/identity:ro \
  -e BACKUP_IDENTITY_FILE=/identity "$image" verify /backup

manifest=$(decrypt < "$backup/manifest.txt.age")
value() { printf '%s\n' "$manifest" | sed -n "s/^$1=//p"; }
echo "restore: backup $(value created_utc), migration $(value migration_version), $(value tickets) tickets, $(value screenshot_files) screenshot files" >&2

step "2/7 starting PostgreSQL in project $project (empty volume, fresh)"
compose up -d --wait postgres

step "3/7 restoring the database"
db_started=$(now)
decrypt < "$backup/database.pgdump.age" \
  | compose exec -T postgres pg_restore -U postgres -d tikkit --exit-on-error --single-transaction
db_seconds=$(($(now) - db_started))

step "4/7 restoring the screenshot files"
files_started=$(now)
decrypt < "$backup/screenshots.tar.age" \
  | compose run --rm -T --no-deps --entrypoint tar api --no-same-owner -C /var/lib/tikkit/screenshots -xf -
files_seconds=$(($(now) - files_started))

step "5/7 checking the migration history against this API image"
compose run --rm -T --no-deps api migrate

step "6/7 consistency between database rows and files"
printf '%s\n' "$manifest" | sed '1,/^--files--$/d' > "$backup/.restore-files.sha256.$$"
trap 'rm -f "$backup/.restore-files.sha256.$$" 2>/dev/null || true' EXIT
changed=$(compose run --rm -T --no-deps --entrypoint sh api \
  -c 'cd /var/lib/tikkit/screenshots && sha256sum -c --quiet - 2>&1 || true' < "$backup/.restore-files.sha256.$$")
if [ -n "$changed" ]; then
  printf '%s\n' "$changed" >&2
  die "restored screenshot files do not match the manifest checksums"
fi
rows=$(compose exec -T postgres psql -U postgres -d tikkit -Atc "SELECT id FROM screenshots" | tr -d '\r' | sort)
present=$(compose run --rm -T --no-deps --entrypoint ls api -1 /var/lib/tikkit/screenshots | tr -d '\r' | grep -v '\.tmp$' | sort || true)
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
  [ "$allow_missing" = 1 ] || die "$missing_count screenshot rows have no file. The tickets are intact. Rerun with --allow-missing-files to accept this, or restore an older backup that has the files"
fi

if [ -n "$config_out" ]; then
  mkdir -p "$config_out" && chmod 700 "$config_out"
  decrypt < "$backup/config.tar.age" | tar -xf - -C "$config_out"
  step "non-secret deployment configuration extracted to $config_out"
fi

if [ "$start" = 1 ]; then
  step "7/7 starting the isolated copy (RESTORE_MODE=true, no egress network)"
  compose up -d --wait
else
  step "7/7 not starting the application services (--no-start)"
fi
total=$(($(now) - started))
echo "restore: done in ${total}s (database ${db_seconds}s, screenshots ${files_seconds}s). Do the post-restore checklist in docs/backup-restore.md before any live work." >&2
echo "restore_seconds=$total database_restore_seconds=$db_seconds screenshots_restore_seconds=$files_seconds screenshot_rows_without_file=$missing_count screenshot_files_without_row=$extra_count"
