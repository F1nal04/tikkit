# Shared helpers, sourced by the other scripts. POSIX sh (BusyBox ash).

BACKUP_DIR=${BACKUP_DIR:-/backups}
STATUS_DIR=$BACKUP_DIR/.status

# One logfmt line per event on stderr; `level=error` lines are what operators grep for.
log() {
  level=$1
  shift
  printf 'time=%s level=%s component=backup msg="%s"\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$level" "$*" >&2
}

die() {
  log error "$*"
  exit 1
}

# 20261118T031500Z -> epoch seconds
epoch_of() {
  date -u -D '%Y%m%dT%H%M%SZ' -d "$1" +%s
}

state_get() {
  sed -n "s/^$1=//p" "$STATUS_DIR/state" 2>/dev/null | tail -n 1
}

state_set() {
  mkdir -p "$STATUS_DIR"
  { grep -v "^$1=" "$STATUS_DIR/state" 2>/dev/null || true; printf '%s=%s\n' "$1" "$2"; } > "$STATUS_DIR/.state.$$"
  mv "$STATUS_DIR/.state.$$" "$STATUS_DIR/state"
}

# Rewrites the Prometheus text file the sidecar serves from the recorded state.
render_metrics() {
  mkdir -p "$STATUS_DIR"
  copies=$(find "$BACKUP_DIR" -maxdepth 1 -type d -name '2*Z' | wc -l | tr -d ' ')
  cat > "$STATUS_DIR/.metrics.$$" <<METRICS
# HELP tikkit_backup_last_run_success 1 when the most recent backup run succeeded, 0 when it failed.
# TYPE tikkit_backup_last_run_success gauge
tikkit_backup_last_run_success $(state_get last_run_success | grep . || echo 0)
# HELP tikkit_backup_last_attempt_timestamp_seconds Unix time the most recent backup run started.
# TYPE tikkit_backup_last_attempt_timestamp_seconds gauge
tikkit_backup_last_attempt_timestamp_seconds $(state_get last_attempt | grep . || echo 0)
# HELP tikkit_backup_last_success_timestamp_seconds Unix time the most recent successful backup run started (0: never).
# TYPE tikkit_backup_last_success_timestamp_seconds gauge
tikkit_backup_last_success_timestamp_seconds $(state_get last_success | grep . || echo 0)
# HELP tikkit_backup_last_duration_seconds Duration of the most recent backup run.
# TYPE tikkit_backup_last_duration_seconds gauge
tikkit_backup_last_duration_seconds $(state_get last_duration | grep . || echo 0)
# HELP tikkit_backup_last_size_bytes Encrypted size of the most recent successful backup.
# TYPE tikkit_backup_last_size_bytes gauge
tikkit_backup_last_size_bytes $(state_get last_size | grep . || echo 0)
# HELP tikkit_backup_screenshots_missing Screenshot rows without a file when the most recent backup was taken.
# TYPE tikkit_backup_screenshots_missing gauge
tikkit_backup_screenshots_missing $(state_get last_missing | grep . || echo 0)
# HELP tikkit_backup_copies Backups kept in the local backup directory.
# TYPE tikkit_backup_copies gauge
tikkit_backup_copies $copies
METRICS
  mv "$STATUS_DIR/.metrics.$$" "$STATUS_DIR/metrics"
}

BACKUP_FILES="database.pgdump.age screenshots.tar.age config.tar.age manifest.txt.age"

# A backup directory is complete only when SHA256SUMS has exactly one entry for
# each expected file, nothing else, and every checksum matches. Prints the reason
# on failure. Run inside the backup directory.
checksums_ok() {
  [ -f SHA256SUMS ] || { echo "SHA256SUMS is missing"; return 1; }
  for f in $BACKUP_FILES; do
    n=$(awk -v f="$f" '$2 == f' SHA256SUMS | wc -l | tr -d ' ')
    [ "$n" = 1 ] || { echo "SHA256SUMS has $n entries for $f (expected exactly 1)"; return 1; }
  done
  [ "$(wc -l < SHA256SUMS | tr -d ' ')" = 4 ] || { echo "SHA256SUMS has unexpected entries"; return 1; }
  sha256sum -c SHA256SUMS > /dev/null 2>&1 || { echo "a file does not match SHA256SUMS"; return 1; }
}

# A copy that counts toward the retention minimum: complete, verified, not degraded.
backup_is_good() {
  [ ! -e "$1/DEGRADED" ] && (cd "$1" && checksums_ok > /dev/null)
}
