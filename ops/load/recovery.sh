#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --long) export LOAD_RECOVERY_LONG=1 ;;
  '') ;;
  *) echo "usage: $0 [--long]" >&2; exit 2 ;;
esac
exec "$(dirname "$0")/run.sh" recovery
