#!/bin/sh
# One encrypted backup run (docs/backup-restore.md): a PostgreSQL dump, then the
# screenshot files, then the non-secret deployment configuration, each encrypted
# to the public age recipients and described by an encrypted manifest and a
# plain SHA256SUMS. Exits non-zero, logs `level=error` and records the failure
# for the Prometheus signal when anything goes wrong.
set -eu -o pipefail
umask 077
. /opt/tikkit-backup/lib.sh

SCREENSHOT_DIR=${SCREENSHOT_DIR:-/var/lib/tikkit/screenshots}
CONFIG_DIR=${CONFIG_DIR:-/config}
RETENTION_DAYS=${BACKUP_RETENTION_DAYS:-14}
MIN_KEEP=${BACKUP_MIN_KEEP:-3}
# Only these deployment variables are written to the backup (never a secret).
DEPLOYMENT_VARS="PUBLIC_ORIGIN TIKKIT_WEB_IMAGE TIKKIT_API_IMAGE PROXY_BIND HTTP_PORT HTTPS_PORT CADDY_GLOBAL_OPTIONS METRICS_PORT"
export PGHOST=${PGHOST:-postgres} PGUSER=${PGUSER:-tikkit} PGDATABASE=${PGDATABASE:-tikkit}

started=$(date +%s)
stamp=$(date -u +%Y%m%dT%H%M%SZ)
work=$BACKUP_DIR/.tmp-$stamp
ok=0
locked=0
lock_pid=

# --- retention/sweep coordination -------------------------------------------------
# While a backup runs, one database session holds a session-level advisory lock
# from before the dump until after the screenshot copy. The retention worker and
# the stray-file sweep (#43) defer file deletions while it is held, so no file
# the dump references disappears before it is archived.
# PLACEHOLDER: the key below is NOT final. #43 defines the real key/SQL; replace
# the two integers (or set BACKUP_LOCK_SQL_ARGS) when it lands. Postgres' own
# advisory-lock functions always exist, so the script also works on a base that
# has no worker taking this lock. BACKUP_ADVISORY_LOCK=off skips it.
LOCK_KEY_ARGS=${BACKUP_LOCK_SQL_ARGS:-"hashtext('tikkit_backup'), 0"} # PLACEHOLDER key (see above)

lock_acquire() {
  [ "${BACKUP_ADVISORY_LOCK:-on}" = on ] || return 0
  mkfifo "$work/.lock-in"
  psql --no-password -Atq < "$work/.lock-in" > "$work/.lock-out" 2> "$work/.lock-err" &
  lock_pid=$!
  exec 9> "$work/.lock-in"
  echo "SELECT pg_try_advisory_lock($LOCK_KEY_ARGS);" >&9
  i=0
  while [ "$i" -lt 50 ] && [ ! -s "$work/.lock-out" ]; do sleep 0.2; i=$((i + 1)); done
  [ "$(cat "$work/.lock-out")" = t ] || die "cannot take the backup advisory lock (another backup running, or the database refused): $(cat "$work/.lock-err")"
}

# Fails if the holding session died, because the lock was then not held throughout.
lock_release() {
  [ -n "$lock_pid" ] || return 0
  pid=$lock_pid
  lock_pid=
  alive=0
  kill -0 "$pid" 2> /dev/null && alive=1
  { echo "SELECT pg_advisory_unlock($LOCK_KEY_ARGS);"; echo '\q'; } >&9 2> /dev/null || true
  exec 9>&-
  wait "$pid" 2> /dev/null || true
  [ "$alive" = 1 ] || die "the advisory-lock session ended early; the backup was not protected against file deletions"
}

finish() {
  rc=$?
  trap - EXIT
  lock_release 2> /dev/null || true
  rm -rf "$work"
  duration=$(($(date +%s) - started))
  if [ "$locked" = 1 ]; then
    state_set last_attempt "$started"
    state_set last_duration "$duration"
    if [ "$ok" = 1 ]; then
      state_set last_run_success 1
    else
      state_set last_run_success 0
      log error "backup failed after ${duration}s (exit $rc); the last good backup is unchanged"
    fi
    render_metrics
    rmdir "$BACKUP_DIR/.lock" 2>/dev/null || true
  fi
  exit "$rc"
}
trap finish EXIT

[ -n "${BACKUP_AGE_RECIPIENTS:-}" ] || die "BACKUP_AGE_RECIPIENTS is empty: set one or more age public keys (age1...)"
mkdir -p "$BACKUP_DIR" "$STATUS_DIR"
if ! mkdir "$BACKUP_DIR/.lock" 2>/dev/null; then
  log error "another backup run holds $BACKUP_DIR/.lock"
  exit 75
fi
locked=1
mkdir -m 0700 "$work"
for recipient in $BACKUP_AGE_RECIPIENTS; do printf '%s\n' "$recipient"; done > "$work/recipients"
printf probe | age -R "$work/recipients" > /dev/null || die "BACKUP_AGE_RECIPIENTS is not a valid age public key list"
log info "backup $stamp started"

# usage: <producer> | encrypt_stream <name>  -> $work/<name>.age and $work/.sha-<name>
encrypt_stream() {
  fifo=$work/.fifo-$1
  mkfifo "$fifo"
  sha256sum < "$fifo" | cut -d' ' -f1 > "$work/.sha-$1" &
  hasher=$!
  rc=0
  tee "$fifo" | age -R "$work/recipients" -o "$work/$1.age" || rc=$?
  wait "$hasher" || true
  rm -f "$fifo"
  return "$rc"
}

psql_value() { psql --no-password -Atqc "$1"; }

# 1. Database first. A dump is one consistent snapshot; every screenshot row in
#    it already had its file when the dump started, and the files are copied after.
lock_acquire
migration=$(psql_value "SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success")
server=$(psql_value "SHOW server_version")
db_started=$(date +%s)
pg_dump --no-password -Fc | encrypt_stream database.pgdump
db_finished=$(date +%s)
log info "database dumped in $((db_finished - db_started))s"

# 2. Then the screenshot files. Files are immutable and written by atomic rename,
#    so a file is either complete or absent. One that disappears while the
#    archive is written (an expired pending upload) makes tar fail: retry.
files_started=$(date +%s)
attempt=1
while :; do
  (cd "$SCREENSHOT_DIR" && find . -type f ! -name '*.tmp' | sort) > "$work/files.list"
  if [ -s "$work/files.list" ]; then
    # (BusyBox tar refuses an empty list; an empty archive is two zero blocks.)
    if (cd "$SCREENSHOT_DIR" && tar -cf - -T "$work/files.list" | encrypt_stream screenshots.tar); then break; fi
  elif head -c 1024 /dev/zero | encrypt_stream screenshots.tar; then
    break
  fi
  [ "$attempt" -lt 3 ] || die "screenshot archive failed after $attempt attempts"
  attempt=$((attempt + 1))
  log warn "screenshot archive failed, retrying"
done
(cd "$SCREENSHOT_DIR" && xargs -r sha256sum < "$work/files.list") > "$work/files.sha256"
files_finished=$(date +%s)
file_count=$(wc -l < "$work/files.list" | tr -d ' ')

lock_release

# Rows now in the database whose file is not in the archive (attached rows are
# never deleted, so any here means the source volume lost a file).
psql_value "SELECT id FROM screenshots" | sort > "$work/rows.list"
sed 's|^\./||' "$work/files.list" | sort > "$work/names.list"
missing=$(comm -23 "$work/rows.list" "$work/names.list" | wc -l | tr -d ' ')
if [ "$missing" -gt 0 ]; then
  log warn "$missing screenshot rows have no file on the source volume; see docs/backup-restore.md"
fi
log info "screenshots archived ($file_count files) in $((files_finished - files_started))s"

# 3. Non-secret deployment configuration.
mkdir "$work/config"
[ -d "$CONFIG_DIR" ] && cp -R "$CONFIG_DIR/." "$work/config/"
for name in $DEPLOYMENT_VARS; do
  eval "value=\${$name:-}"
  [ -z "$value" ] || printf '%s=%s\n' "$name" "$value"
done > "$work/config/deployment.env"
tar -cf - -C "$work/config" . | encrypt_stream config.tar
rm -rf "$work/config"

# 4. Encrypted manifest and the plain checksum list.
tickets=$(psql_value "SELECT count(*) FROM tickets")
{
  echo "format=tikkit-backup-1"
  echo "created_utc=$stamp"
  echo "created_epoch=$started"
  echo "database_dump_started_epoch=$db_started"
  echo "database_dump_finished_epoch=$db_finished"
  echo "screenshots_started_epoch=$files_started"
  echo "screenshots_finished_epoch=$files_finished"
  echo "postgres_server_version=$server"
  echo "migration_version=$migration"
  echo "tickets=$tickets"
  echo "screenshot_rows=$(wc -l < "$work/rows.list" | tr -d ' ')"
  echo "screenshot_files=$file_count"
  echo "screenshot_rows_without_file=$missing"
  echo "sha256.database.pgdump=$(cat "$work/.sha-database.pgdump")"
  echo "sha256.screenshots.tar=$(cat "$work/.sha-screenshots.tar")"
  echo "sha256.config.tar=$(cat "$work/.sha-config.tar")"
  echo "--files--"
  cat "$work/files.sha256"
} | age -R "$work/recipients" -o "$work/manifest.txt.age"
(cd "$work" && sha256sum database.pgdump.age screenshots.tar.age config.tar.age manifest.txt.age > SHA256SUMS)
rm -f "$work"/.sha-* "$work"/files.* "$work"/rows.list "$work"/names.list "$work/recipients"
sync
mv "$work" "$BACKUP_DIR/$stamp"
work=$BACKUP_DIR/.gone

size=$(cat "$BACKUP_DIR/$stamp"/*.age | wc -c | tr -d ' ')
state_set last_success "$started"
state_set last_size "$size"
state_set last_missing "$missing"
ok=1

# 5. Retention: drop copies older than RETENTION_DAYS, but always keep the
#    newest MIN_KEEP. Runs only after a successful backup.
now=$(date +%s)
index=0
for name in $(find "$BACKUP_DIR" -maxdepth 1 -type d -name '2*Z' -exec basename {} \; | sort -r); do
  index=$((index + 1))
  [ "$index" -gt "$MIN_KEEP" ] || continue
  if [ $(((now - $(epoch_of "$name")) / 86400)) -ge "$RETENTION_DAYS" ]; then
    rm -rf "${BACKUP_DIR:?}/$name"
    log info "expired backup $name removed (older than $RETENTION_DAYS days)"
  fi
done
log info "backup $stamp complete: $size bytes, $(($(date +%s) - started))s"
