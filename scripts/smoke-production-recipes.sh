#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="$repository_root/docs/recipes/fixtures/mosquitto/compose.yaml"
recipe_project="${COMPOSE_PROJECT_NAME:-rumqtt-recipe-smoke-$$}"

cleanup() {
    docker compose --project-name "$recipe_project" --file "$compose_file" down --volumes
}
trap cleanup EXIT

docker compose --project-name "$recipe_project" --file "$compose_file" config --quiet
docker compose --project-name "$recipe_project" --file "$compose_file" up --detach --wait

cargo run --quiet -p rumqttc-v4-next --example broker_recipe_smoke
cargo run --quiet -p rumqttc-v5-next --example broker_recipe_smoke_v5
