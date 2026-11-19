# Sourced by run.sh and its children. Run against the disposable project only.
. ops/backup/hostlib.sh
project=tikkit-m4-50
files=(-f docker-compose.prod.example.yml -f ops/compose/workflows.yml -f ops/load/compose.yml)
stack() { dc --env-file "${LOAD_WORK:?}/stack.env" -p "$project" "${files[@]}" "$@"; }
sql() { stack exec -T postgres psql -U postgres -d tikkit -v ON_ERROR_STOP=1 -Atq "$@"; }
