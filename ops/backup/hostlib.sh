# Helpers for the host-side scripts (restore.sh, drill.sh, offsite.sh). Sourced, bash.

# Docker Compose with NO inherited interpolation: the env file given with
# --env-file is the only source of ${VARIABLES}. A surrounding shell or CI job that
# exports HTTP_PORT, PUBLIC_ORIGIN or COMPOSE_FILE cannot leak into the project.
dc() {
  local keep=(PATH="$PATH" HOME="${HOME:-/}") v
  for v in DOCKER_HOST DOCKER_CONTEXT DOCKER_CONFIG DOCKER_CERT_PATH DOCKER_TLS_VERIFY XDG_CONFIG_HOME TMPDIR; do
    [ -z "${!v:-}" ] || keep+=("$v=${!v}")
  done
  env -i "${keep[@]}" docker compose "$@"
}

# Every volume, network and container name a project would use, from the rendered
# Compose config (not from labels), one per line as "kind name".
project_names() { # <env-file> <project> <compose -f args...>
  local env_file=$1 project=$2 cfg
  shift 2
  cfg=$(dc --env-file "$env_file" -p "$project" "$@" config --format json) || return 1
  jq -r '(.volumes // {}) | to_entries[] | "volume \(.value.name)"' <<< "$cfg"
  jq -r '(.networks // {}) | to_entries[] | select(.value.external != true) | "network \(.value.name)"' <<< "$cfg"
  jq -r --arg p "$project" '.services | keys[] | "container \($p)-\(.)-1"' <<< "$cfg"
}

# Resources that already exist for the project: by exact expected name, by Compose
# label. (The reservation network is the caller's business.) Prints "kind name" lines; empty means free.
project_occupied() { # <env-file> <project> <compose -f args...>
  local env_file=$1 project=$2 kind name
  shift 2
  {
    project_names "$env_file" "$project" "$@" || echo "error cannot-render-the-compose-config"
  } | while read -r kind name; do
    case $kind in
      error) echo "$kind $name" ;;
      *) docker "$kind" inspect "$name" > /dev/null 2>&1 && echo "$kind $name" ;;
    esac
  done
  docker ps -a --filter "label=com.docker.compose.project=$project" --format 'container {{.Names}}'
  docker volume ls --filter "label=com.docker.compose.project=$project" --format 'volume {{.Name}}'
  docker network ls --filter "label=com.docker.compose.project=$project" --format 'network {{.Name}}'
  true
}

# Atomically claims the project name: network names are unique, so a second
# invocation (or a leftover) fails here before anything else is created.
reserve_project() { docker network create --internal --label tikkit.reservation="$1" "$1-reserved" > /dev/null 2>&1; }
release_project() { docker network rm "$1-reserved" > /dev/null 2>&1 || true; }
