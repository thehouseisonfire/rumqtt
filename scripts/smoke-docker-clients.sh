#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="$repository_root/docs/recipes/fixtures/docker-clients/compose.yaml"
docker_project="${COMPOSE_PROJECT_NAME:-rumqtt-docker-client-smoke-$$}"

cleanup() {
    exit_status=$?
    trap - EXIT
    docker compose --project-name "$docker_project" --file "$compose_file" down --volumes --remove-orphans || true
    exit "$exit_status"
}
trap cleanup EXIT

docker compose --project-name "$docker_project" --file "$compose_file" config --quiet
docker compose --project-name "$docker_project" --file "$compose_file" build client-v4 client-v5
docker compose --project-name "$docker_project" --file "$compose_file" up --detach --wait broker

v4_output="$(
    docker compose --project-name "$docker_project" --file "$compose_file" run --rm --no-deps client-v4
)"
echo "$v4_output"
grep --fixed-strings --quiet "MQTT 3.1.1 broker recipe smoke test passed" <<<"$v4_output"

v5_output="$(
    docker compose --project-name "$docker_project" --file "$compose_file" run --rm --no-deps client-v5
)"
echo "$v5_output"
grep --fixed-strings --quiet "MQTT 5 broker recipe smoke test passed" <<<"$v5_output"
