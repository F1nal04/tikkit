#!/bin/sh
# docker run IMAGE [schedule|run|verify <dir>|<any command, e.g. age-keygen>]
set -eu
case "${1:-schedule}" in
  schedule) exec /opt/tikkit-backup/schedule.sh ;;
  run) exec /opt/tikkit-backup/backup.sh ;;
  verify) shift; exec /opt/tikkit-backup/verify.sh "$@" ;;
  *) exec "$@" ;;
esac
