#!/bin/sh
# One encrypted backup run (docs/backup-restore.md): a PostgreSQL dump, then the
# screenshot files, then the non-secret deployment configuration, each encrypted
# to the public age recipients and described by an encrypted manifest and a
# plain SHA256SUMS. Exits non-zero, logs `level=error` and records the failure
# for the Prometheus signal when anything goes wrong; a failed or degraded run
# never rotates away older copies.
set -eu -o pipefail
umask 077
. /opt/tikkit-backup/lib.sh
# A write to a dead database session must fail the command, not kill the shell.
trap '' PIPE

SCREENSHOT_DIR=${SCREENSHOT_DIR:-/var/lib/tikkit/screenshots}
CONFIG_DIR=${CONFIG_DIR:-/config}
RETENTION_DAYS=${BACKUP_RETENTION_DAYS:-14}
MIN_KEEP=${BACKUP_MIN_KEEP:-3}
SESSION_TIMEOUT_TENTHS=${BACKUP_SESSION_TIMEOUT_TENTHS:-6000}
# Only these deployment variables are written to the backup (never a secret).
DEPLOYMENT_VARS="PUBLIC_ORIGIN TIKKIT_WEB_IMAGE TIKKIT_API_IMAGE PROXY_BIND HTTP_PORT HTTPS_PORT CADDY_GLOBAL_OPTIONS METRICS_PORT"
export PGHOST=${PGHOST:-postgres} PGUSER=${PGUSER:-tikkit} PGDATABASE=${PGDATABASE:-tikkit}

started=$(date +%s)
stamp=$(date -u +%Y%m%dT%H%M%SZ)
work=
ok=0
locked=0
sess_pid=
seq=0
SESS_OUT=

finish() {
  rc=$?
  trap - EXIT
  if [ -n "$sess_pid" ]; then
    { exec 9>&-; } 2> /dev/null || true
    kill "$sess_pid" 2> /dev/null || true
  fi
  [ -z "$work" ] || rm -rf "$work"
  duration=$(($(date +%s) - started))
  if [ "$locked" = 1 ]; then
    state_set last_attempt "$started"
    state_set last_duration "$duration"
    if [ "$ok" = 1 ]; then
      state_set last_run_success 1
    else
      state_set last_run_success 0
      log error "backup failed after ${duration}s (exit $rc); the last good backup is unchanged and nothing was rotated"
    fi
    render_metrics
  fi
  exit "$rc"
}
trap finish EXIT

mkdir -p "$BACKUP_DIR" "$STATUS_DIR"
# The run lock is a file descriptor lock: the kernel releases it when this
# process exits, however it exits, so a full disk or a kill cannot leave it stuck.
exec 8> "$BACKUP_DIR/.flock"
if ! flock -n 8; then
  log error "another backup run is in progress"
  exit 75
fi
locked=1
# A private staging directory owned by this invocation alone.
work=$(mktemp -d "$BACKUP_DIR/.tmp-$stamp.XXXXXX")
[ -n "${BACKUP_AGE_RECIPIENTS:-}" ] || die "BACKUP_AGE_RECIPIENTS is empty: set one or more age public keys (age1...)"
for recipient in $BACKUP_AGE_RECIPIENTS; do printf '%s\n' "$recipient"; done > "$work/recipients"
printf probe | age -R "$work/recipients" > /dev/null || die "BACKUP_AGE_RECIPIENTS is not a valid age public key list"
log info "backup $stamp started"

# usage: <producer> | encrypt_stream <name>  -> $work/<name>.age and $work/.sha-<name>
# Fails when the producer, age or the hasher fails.
encrypt_stream() {
  fifo=$work/.fifo-$1
  mkfifo "$fifo"
  sha256sum < "$fifo" | cut -d' ' -f1 > "$work/.sha-$1" &
  hasher=$!
  rc=0
  tee "$fifo" | age -R "$work/recipients" -o "$work/$1.age" || rc=$?
  wait "$hasher" || rc=1
  rm -f "$fifo"
  [ "$(wc -c < "$work/.sha-$1" | tr -d ' ')" = 65 ] || rc=1
  return "$rc"
}

# --- one database session for the whole backup --------------------------------------
# It holds the screenshot-files advisory lock (when the database has one), the
# exported snapshot the dump reads, and the row list and counts, so they all
# describe the same moment. It is verified from the inside at the end: if its
# backend was terminated the backup fails, however healthy the client looks.

sess_wait() {
  i=0
  until grep -qx "__END_$1__" "$work/.sess-out" 2> /dev/null; do
    kill -0 "$sess_pid" 2> /dev/null || die "the database session ended: $(tr '\n' ' ' < "$work/.sess-err")"
    sleep 0.1
    i=$((i + 1))
    [ "$i" -lt "$SESSION_TIMEOUT_TENTHS" ] || die "the database session did not answer in time"
  done
}

# usage: sess "<SQL>"  -> output in $SESS_OUT (not a command substitution: the counter must persist)
sess() {
  seq=$((seq + 1))
  printf '%s;\n\\echo __END_%s__\n' "$1" "$seq" >&9 || die "cannot talk to the database session"
  sess_wait "$seq"
  SESS_OUT=$(awk -v a="__END_$((seq - 1))__" -v b="__END_${seq}__" '$0 == b { f = 0 } f { print } $0 == a { f = 1 }' "$work/.sess-out")
}

mkfifo "$work/.sess-in"
PGAPPNAME=tikkit-backup psql --no-password -Atq -v ON_ERROR_STOP=1 < "$work/.sess-in" > "$work/.sess-out" 2> "$work/.sess-err" &
sess_pid=$!
exec 9> "$work/.sess-in"
printf '\\echo __END_0__\n' >&9
sess_wait 0

# Take the lock BEFORE the dump. #43 defines the function; this base may not have it.
sess "SELECT to_regprocedure('screenshot_files_lock_key()') IS NOT NULL"
has_lock=$SESS_OUT
if [ "$has_lock" = t ]; then
  sess "SET lock_timeout = '10min'"
  sess "SELECT pg_advisory_lock(screenshot_files_lock_key())"
  log info "holding the screenshot-files lock until the screenshot copy is done"
else
  log warn "screenshot_files_lock_key() does not exist in this database: running WITHOUT the screenshot-files lock"
fi
sess "BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY"
sess "SELECT pg_export_snapshot()"
snapshot=$SESS_OUT
[ -n "$snapshot" ] || die "the database did not export a snapshot"
sess "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint"
snapshot_epoch=$SESS_OUT
sess "SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success"
migration=$SESS_OUT
sess "SHOW server_version"
server=$SESS_OUT
sess "SELECT count(*) FROM tickets"
tickets=$SESS_OUT
sess "SELECT id FROM screenshots"
printf '%s\n' "$SESS_OUT" | grep . | sort > "$work/rows.list" || true

# 1. Database first, from that snapshot. Every screenshot row in the dump already
#    had its file when the snapshot was taken, and the files are copied after.
db_started=$(date +%s)
pg_dump --no-password -Fc --snapshot="$snapshot" | encrypt_stream database.pgdump
db_finished=$(date +%s)
log info "database dumped in $((db_finished - db_started))s"

# 2. Then the screenshot files. Files are immutable and written by atomic rename,
#    so a file is either complete or absent. One that disappears while the
#    archive is written makes tar fail: retry.
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

# End of the protected window: end the snapshot transaction, then give back the
# lock through the SAME session. Anything but `t` means the lock was not held for
# the whole backup, so this backup is not trusted.
sess "COMMIT"
if [ "$has_lock" = t ]; then
  sess "SELECT pg_advisory_unlock(screenshot_files_lock_key())"
  [ "$SESS_OUT" = t ] || die "the screenshot-files lock was lost during the backup (unlock returned '$SESS_OUT')"
else
  sess "SELECT 1"
  [ "$SESS_OUT" = 1 ] || die "the database session was lost during the backup"
fi
printf '\\q\n' >&9
exec 9>&-
sess_rc=0
wait "$sess_pid" || sess_rc=$?
sess_pid=
[ "$sess_rc" = 0 ] || die "the database session ended with status $sess_rc"

# Rows of the snapshot whose file is not in the archive (attached rows are never
# deleted, so any here means the source volume lost a file).
sed 's|^\./||' "$work/files.list" | sort > "$work/names.list"
comm -23 "$work/rows.list" "$work/names.list" > "$work/missing.list"
missing=$(wc -l < "$work/missing.list" | tr -d ' ')
degraded=
if [ "$missing" -gt 0 ]; then
  degraded=screenshots_missing=$missing
  printf '%s\n' "$degraded" > "$work/DEGRADED"
  log warn "$missing screenshot rows have no file on the source volume; this backup is degraded and rotates nothing; see docs/backup-restore.md"
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

# The final name is free (runs are serialized by the lock; a same-second run waits).
while [ -e "$BACKUP_DIR/$stamp" ]; do
  sleep 1
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
done

# 4. Encrypted manifest and the plain checksum list.
{
  echo "format=tikkit-backup-1"
  echo "created_utc=$stamp"
  echo "created_epoch=$started"
  echo "snapshot_epoch=$snapshot_epoch"
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
  echo "screenshot_lock_held=$([ "$has_lock" = t ] && echo yes || echo no)"
  echo "sha256.database.pgdump=$(cat "$work/.sha-database.pgdump")"
  echo "sha256.screenshots.tar=$(cat "$work/.sha-screenshots.tar")"
  echo "sha256.config.tar=$(cat "$work/.sha-config.tar")"
  echo "--files--"
  cat "$work/files.sha256"
} | age -R "$work/recipients" -o "$work/manifest.txt.age"
(cd "$work" && sha256sum $BACKUP_FILES > SHA256SUMS)
rm -f "$work"/.sha-* "$work"/files.* "$work"/rows.list "$work"/names.list "$work"/missing.list "$work/recipients" "$work"/.sess-*
sync
mv "$work" "$BACKUP_DIR/$stamp"
work=
sync

size=$(cat "$BACKUP_DIR/$stamp"/*.age | wc -c | tr -d ' ')
state_set last_success "$started"
state_set last_size "$size"
state_set last_missing "$missing"
ok=1

# 5. Retention, only after a complete, non-degraded backup: drop copies older than
#    RETENTION_DAYS, but always keep the newest MIN_KEEP that are verified
#    complete. Incomplete or degraded copies never count toward that minimum.
if [ -z "$degraded" ]; then
  now=$(date +%s)
  kept=0
  for name in $(find "$BACKUP_DIR" -maxdepth 1 -type d -name '2*Z' -exec basename {} \; | sort -r); do
    if [ "$kept" -lt "$MIN_KEEP" ] && backup_is_good "$BACKUP_DIR/$name"; then
      kept=$((kept + 1))
      continue
    fi
    if [ $(((now - $(epoch_of "${name%%-*}")) / 86400)) -ge "$RETENTION_DAYS" ]; then
      rm -rf "${BACKUP_DIR:?}/$name"
      log info "expired backup $name removed (older than $RETENTION_DAYS days)"
    fi
  done
fi
log info "backup $stamp complete: $size bytes, $(($(date +%s) - started))s"
