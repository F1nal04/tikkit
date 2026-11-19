#!/bin/sh
# Sidecar loop: runs backup.sh at BACKUP_TIMES_UTC (HH:MM, comma separated), at
# start when there is no recent successful backup, and again after a failure.
# Also serves the status metrics on :9092 for the private Prometheus scrape.
set -eu
. /opt/tikkit-backup/lib.sh

TIMES=$(echo "${BACKUP_TIMES_UTC:-03:15}" | tr ',' ' ')
MAX_AGE_HOURS=${BACKUP_MAX_AGE_HOURS:-26}
RETRY_MINUTES=${BACKUP_RETRY_MINUTES:-30}

mkdir -p "$STATUS_DIR"
rmdir "$BACKUP_DIR/.lock" 2>/dev/null || true
rm -rf "$BACKUP_DIR"/.tmp-* 2>/dev/null || true
render_metrics
httpd -f -p 9092 -h "$STATUS_DIR" &
httpd_pid=$!
trap 'kill "$httpd_pid" 2>/dev/null; exit 0' TERM INT

retry_at=0
last_slot=
run() {
  if /opt/tikkit-backup/backup.sh; then
    retry_at=0
  else
    retry_at=$(($(date +%s) + RETRY_MINUTES * 60))
  fi
}

last_success=$(state_get last_success | grep . || echo 0)
if [ $(($(date +%s) - last_success)) -ge $((MAX_AGE_HOURS * 3600)) ]; then
  log info "no backup in the last ${MAX_AGE_HOURS}h: running one now"
  run
fi
log info "scheduled at $TIMES UTC; metrics on :9092"
while :; do
  kill -0 "$httpd_pid" 2>/dev/null || die "metrics server stopped"
  now=$(date +%s)
  slot=$(date -u +%Y%m%d%H:%M)
  for time in $TIMES; do
    if [ "$(date -u +%H:%M)" = "$time" ] && [ "$last_slot" != "$slot" ]; then
      last_slot=$slot
      run
    fi
  done
  if [ "$retry_at" -gt 0 ] && [ "$now" -ge "$retry_at" ]; then
    log warn "retrying the failed backup"
    run
  fi
  sleep 20 &
  wait $! || true
done
