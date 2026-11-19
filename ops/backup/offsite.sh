#!/usr/bin/env bash
# Off-host copy as an idempotent, retryable reconciliation (docs/backup-restore.md#off-host-copy).
# Run it every few minutes from cron or a systemd timer; it copies every published
# backup that has not been shipped yet, newest first, so a run that failed or a
# copy that raced the sidecar is simply picked up on the next run.
#
#   ops/backup/offsite.sh --project tikkit --stage /srv/offsite-staging \
#       --push 'rclone copy "$BACKUP_DIR" "remote:tikkit-backups/$BACKUP_NAME"' \
#       [--image tikkit-backup:local] [--keep 14] [--max-age-hours 30]
#
# For each missing backup: copy it out of the sidecar's volume, check it by checksum
# without any key, then run --push with BACKUP_DIR (the staged copy) and BACKUP_NAME
# in the environment. Prints one status line and exits non-zero when the newest
# backup is not at the destination, or (with --max-age-hours) when the newest
# shipped copy is older than that: that is the destination freshness signal.
set -euo pipefail
umask 077
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=hostlib.sh
. "$here/hostlib.sh"

project= stage= push= image=${TIKKIT_BACKUP_IMAGE:-tikkit-backup:local} keep=14 max_age_hours=
die() { echo "offsite: $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --project) project=$2; shift 2 ;;
    --stage) stage=$2; shift 2 ;;
    --push) push=$2; shift 2 ;;
    --image) image=$2; shift 2 ;;
    --keep) keep=$2; shift 2 ;;
    --max-age-hours) max_age_hours=$2; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$project" ] && [ -n "$stage" ] && [ -n "$push" ] || die "usage: offsite.sh --project NAME --stage DIR --push 'COMMAND' [--max-age-hours N]"
mkdir -p "$stage" && chmod 700 "$stage"

cid=$(docker ps -q --filter "label=com.docker.compose.project=$project" --filter "label=com.docker.compose.service=backup" | head -n 1)
[ -n "$cid" ] || die "no running backup service in project $project"
names=$(docker exec "$cid" sh -c 'ls /backups | grep "^2" | sort -r')
[ -n "$names" ] || die "the sidecar has no backup yet"

# Serialize runs (a cron overlap is harmless but wasteful).
exec 7> "$stage/.offsite.lock"
if command -v flock > /dev/null; then flock -n 7 || die "another run is in progress"; fi

for name in $names; do
  [ ! -e "$stage/$name.pushed" ] || continue
  rm -rf "$stage/.tmp-$name"
  docker cp "$cid:/backups/$name" "$stage/.tmp-$name" > /dev/null
  if ! docker run --rm --user "$(id -u):$(id -g)" -v "$stage/.tmp-$name":/backup:ro "$image" verify /backup 2> /dev/null; then
    rm -rf "$stage/.tmp-$name"
    echo "offsite: $name is not a complete backup (yet); skipped" >&2
    continue
  fi
  rm -rf "${stage:?}/$name"
  mv "$stage/.tmp-$name" "$stage/$name"
  if BACKUP_DIR="$stage/$name" BACKUP_NAME="$name" sh -c "$push"; then
    : > "$stage/$name.pushed"
  else
    echo "offsite: pushing $name failed; it is retried on the next run" >&2
  fi
done

# Keep the newest $keep staged copies.
ls "$stage" | grep '^2.*Z$' | sort -r | tail -n +"$((keep + 1))" | while read -r old; do rm -rf "${stage:?}/$old" "$stage/$old.pushed"; done

newest=$(printf '%s\n' "$names" | head -n 1)
shipped=$(ls "$stage" | grep '\.pushed$' | sed 's/\.pushed$//' | sort | tail -n 1)
if [ -z "$shipped" ]; then
  echo "offsite_newest_backup=$newest offsite_newest_shipped=none"
  exit 1
fi
epoch=$(date -u -d "$(printf '%s' "$shipped" | sed 's/^\(....\)\(..\)\(..\)T\(..\)\(..\)\(..\)Z.*/\1-\2-\3 \4:\5:\6/')" +%s 2> /dev/null \
  || date -u -j -f '%Y%m%dT%H%M%SZ' "${shipped%%-*}" +%s)
age=$(($(date +%s) - epoch))
echo "offsite_newest_backup=$newest offsite_newest_shipped=$shipped offsite_age_seconds=$age"
[ "$shipped" = "$newest" ] || exit 1
[ -z "$max_age_hours" ] || [ "$age" -le $((max_age_hours * 3600)) ] || exit 1
