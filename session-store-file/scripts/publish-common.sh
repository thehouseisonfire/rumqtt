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

if [[ -n "${STORAGE_RELEASE_PACKAGES:-}" ]]; then
    read -r -a packages <<<"$STORAGE_RELEASE_PACKAGES"
else
    packages=(atomic-blob-store rumqttc-session-store-file-next)
fi
for package in "${packages[@]}"; do
    case "$package" in
        atomic-blob-store|rumqttc-session-store-file-next) ;;
        *)
            echo "error: unsupported storage package: $package" >&2
            exit 2
            ;;
    esac
done

versions="$({ cargo metadata --no-deps --format-version 1; } | python3 -c '
import json, sys
wanted = {"atomic-blob-store", "rumqttc-session-store-file-next"}
packages = {
    package["name"]: package["version"]
    for package in json.load(sys.stdin)["packages"]
    if package["name"] in wanted
}
if set(packages) != wanted:
    raise SystemExit(f"storage packages are missing: {packages}")
for name in ("atomic-blob-store", "rumqttc-session-store-file-next"):
    print(f"{name}={packages[name]}")
')"

declare -A package_versions
while IFS='=' read -r package package_version; do
    package_versions["$package"]="$package_version"
done <<<"$versions"

for package in "${packages[@]}"; do
    package_version="${package_versions[$package]}"
    case "$channel" in
        stable)
            [[ "$package_version" != *-* ]] || {
                echo "error: $package $package_version is a prerelease" >&2
                exit 1
            }
            ;;
        prerelease)
            [[ "$package_version" == *-* ]] || {
                echo "error: $package $package_version is stable" >&2
                exit 1
            }
            ;;
        *)
            echo "error: unsupported release channel: $channel" >&2
            exit 2
            ;;
    esac
done

if [[ -n "$(git -C "$repo_dir" status --short)" ]]; then
    echo "error: release requires a clean worktree" >&2
    exit 1
fi
if [[ " ${packages[*]} " == *" atomic-blob-store "* ]] &&
    ! grep -Fq "## [${package_versions[atomic-blob-store]}] - " atomic-blob-store/CHANGELOG.md; then
    echo "error: cut the atomic blob store changelog before publishing" >&2
    exit 1
fi
if [[ " ${packages[*]} " == *" rumqttc-session-store-file-next "* ]] &&
    ! grep -Fq "## [${package_versions[rumqttc-session-store-file-next]}] - " CHANGELOG.md; then
    echo "error: cut the adapter changelog before publishing" >&2
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
for package in "${packages[@]}"; do
    if [[ "$package" == atomic-blob-store ]]; then
        cargo package --locked --no-verify -p "$package"
    else
        # Full adapter packaging becomes possible after the core reaches crates.io.
        cargo package --locked --list -p "$package" >/dev/null
    fi
done

if [[ "$execute" != true ]]; then
    echo "Validated independent core and adapter releases. Re-run with --execute to publish."
    exit 0
fi

for package in "${packages[@]}"; do
    cargo publish --locked -p "$package"
    wait_for_crate "$package" "${package_versions[$package]}"
done

for package in "${packages[@]}"; do
    package_version="${package_versions[$package]}"
    git -C "$repo_dir" tag -a "${package}-${package_version}" -m "${package} ${package_version}"
done

echo "Published storage releases and created local annotated tags."
