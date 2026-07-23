#!/usr/bin/env bash

set -euo pipefail

channel="${1:?release channel is required}"
shift

execute=false
if [[ "${1:-}" == "--execute" ]]; then
    execute=true
    shift
fi
if (($# != 0)); then
    echo "usage: $0 <stable|prerelease> [--execute]" >&2
    exit 2
fi

workspace_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
repo_dir="$(git -C "$workspace_dir" rev-parse --show-toplevel)"
cd "$workspace_dir"

packages=(
    rumqttc-session-store-file-core-next
    rumqttc-session-store-file-next
)

version="$({ cargo metadata --no-deps --format-version 1; } | python3 -c '
import json, sys
packages = {
    package["name"]: package["version"]
    for package in json.load(sys.stdin)["packages"]
    if package["name"].startswith("rumqttc-")
}
versions = set(packages.values())
if len(packages) != 2 or len(versions) != 1:
    raise SystemExit(f"storage package versions are not coordinated: {packages}")
print(versions.pop())
')"

case "$channel" in
    stable)
        if [[ "$version" == *-* ]]; then
            echo "error: $version is a prerelease; use publish-crates-alpha.sh" >&2
            exit 1
        fi
        ;;
    prerelease)
        if [[ "$version" != *-* ]]; then
            echo "error: $version is stable; use publish-crates.sh" >&2
            exit 1
        fi
        ;;
    *)
        echo "error: unsupported release channel: $channel" >&2
        exit 2
        ;;
esac

if [[ -n "$(git -C "$repo_dir" status --short)" ]]; then
    echo "error: release requires a clean worktree" >&2
    exit 1
fi
if ! grep -Fq "## [$version] - " CHANGELOG.md; then
    echo "error: cut session-store-file/CHANGELOG.md for $version before publishing" >&2
    exit 1
fi

client_version_for_alias() {
    python3 -c '
import sys, tomllib
with open("adapter/Cargo.toml", "rb") as source:
    print(tomllib.load(source)["dependencies"][sys.argv[1]]["version"])
' "$1"
}

wait_for_crate() {
    local package="$1"
    local crate_version="$2"
    local attempt
    for attempt in $(seq 1 30); do
        if curl --max-time 15 -fsS \
            --user-agent "rumqtt-session-store-file-release/1.0" \
            "https://crates.io/api/v1/crates/${package}/${crate_version}" >/dev/null; then
            return 0
        fi
        echo "waiting for crates.io to index ${package} ${crate_version} (${attempt}/30)"
        sleep 10
    done
    echo "error: ${package} ${crate_version} did not appear on crates.io" >&2
    exit 1
}

for client in rumqttc-v4 rumqttc-v5; do
    client_package="${client}-next"
    client_version="$(client_version_for_alias "$client")"
    wait_for_crate "$client_package" "$client_version"
done

cargo fmt --all --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
cargo test --locked --workspace --doc
cargo package --locked --no-verify -p "${packages[0]}"
for package in "${packages[@]:1}"; do
    # Full adapter packaging becomes possible after the core reaches crates.io.
    cargo package --locked --list -p "$package" >/dev/null
done

if [[ "$execute" != true ]]; then
    echo "Validated storage release $version. Re-run with --execute to publish."
    exit 0
fi

for package in "${packages[@]}"; do
    cargo publish --locked -p "$package"
    wait_for_crate "$package" "$version"
done

for package in "${packages[@]}"; do
    git -C "$repo_dir" tag -a "${package}-${version}" -m "${package} ${version}"
done

echo "Published storage release $version and created local annotated tags."
