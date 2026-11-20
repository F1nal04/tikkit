#!/bin/sh
# Sidecar loop. One rule: a backup must have STARTED since the latest due time in
# BACKUP_TIMES_UTC (HH:MM, comma separated). That covers a normal slot, a sidecar
# that was down or restarted late (it runs at once, not at the next slot), and the
# first start (no backup yet). After a failed run it waits BACKUP_RETRY_MINUTES
# and tries again. Also serves the status metrics on :9092 for the private
# Prometheus scrape.
set -eu
. /opt/tikkit-backup/lib.sh

TIMES=$(echo "${BACKUP_TIMES_UTC:-03:15}" | tr ',' ' ')
RETRY_MINUTES=${BACKUP_RETRY_MINUTES:-30}
for time in $TIMES; do
  case $time in
    [0-1][0-9]:[0-5][0-9] | 2[0-3]:[0-5][0-9]) ;;
    *) die "BACKUP_TIMES_UTC entry '$time' is not HH:MM" ;;
  esac
done

mkdir -p "$STATUS_DIR"
rmdir "$BACKUP_DIR/.lock" 2> /dev/null || true
# Leftovers of a crashed run (only if no run holds the lock).
(flock -n 8 && rm -rf "$BACKUP_DIR"/.tmp-*) 8> "$BACKUP_DIR/.flock" || true
render_metrics
httpd -f -p 9092 -h "$STATUS_DIR" &
httpd_pid=$!
trap 'kill "$httpd_pid" 2>/dev/null; exit 0' TERM INT

# Epoch of the most recent due time that is not in the future.
latest_due() {
  today=$(date -u +%Y-%m-%d)
  now=$(date +%s)
  best=0
  for time in $TIMES; do
    due=$(date -u -d "$today $time:00" +%s)
    [ "$due" -le "$now" ] || due=$((due - 86400))
    [ "$due" -le "$best" ] || best=$due
  done
  echo "$best"
}

retry_at=0
run() {
  if /opt/tikkit-backup/backup.sh; then
    retry_at=0
  else
    retry_at=$(($(date +%s) + RETRY_MINUTES * 60))
  fi
}

log info "scheduled at $TIMES UTC; metrics on :9092"
while :; do
  kill -0 "$httpd_pid" 2> /dev/null || die "metrics server stopped"
  last_success=$(state_get last_success | grep . || echo 0)
  due=$(latest_due)
  if [ "$last_success" -lt "$due" ] && [ "$(date +%s)" -ge "$retry_at" ]; then
    log info "no backup has started since the due time $(date -u -d "@$due" +%Y-%m-%dT%H:%M:%SZ): running one now"
    run
  fi
  sleep 20 &
  wait $! || true
done
