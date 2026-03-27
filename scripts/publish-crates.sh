#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PACKAGE_NAMES=(
  "mqttbytes-core-next"
  "rumqttc-core-next"
  "rumqttc-v4-next"
  "rumqttc-v5-next"
  "rumqttc-next"
)

PUBLISH_ORDER=(
  "mqttbytes-core-next"
  "rumqttc-core-next"
  "rumqttc-v4-next"
  "rumqttc-v5-next"
  "rumqttc-next"
)

MANIFESTS=(
  "mqttbytes-core/Cargo.toml"
  "rumqttc-core/Cargo.toml"
  "rumqttc-v4/Cargo.toml"
  "rumqttc-v5/Cargo.toml"
  "rumqttc-next/Cargo.toml"
)

require_clean_tree() {
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "error: git working tree is not clean" >&2
    git status --short
    exit 1
  fi
}

require_tools() {
  local tool
  for tool in cargo cargo-audit curl git perl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      echo "error: required tool '$tool' not found" >&2
      exit 1
    fi
  done
}

current_version() {
  perl -ne 'if (/^version = "([^"]+)"/) { print "$1\n"; exit }' rumqttc-next/Cargo.toml
}

assert_versions_match() {
  local expected="$1"
  local manifest version
  for manifest in "${MANIFESTS[@]}"; do
    version="$(perl -ne 'if (/^version = "([^"]+)"/) { print "$1\n"; exit }' "$manifest")"
    if [[ "$version" != "$expected" ]]; then
      echo "error: $manifest has version $version, expected $expected" >&2
      exit 1
    fi
  done
}

next_version() {
  local current="$1"
  local release_type="$2"
  local major minor patch

  IFS=. read -r major minor patch <<<"$current"

  case "$release_type" in
    current)
      printf '%s\n' "$current"
      ;;
    patch)
      printf '%s.%s.%s\n' "$major" "$minor" "$((patch + 1))"
      ;;
    minor)
      printf '%s.%s.0\n' "$major" "$((minor + 1))"
      ;;
    *)
      echo "error: unknown release type '$release_type'" >&2
      exit 1
      ;;
  esac
}

prompt_release_type() {
  local reply
  while true; do
    printf 'Release type: current (%s), patch (0.0.x), or minor (0.x.0)? [current/patch/minor]: ' "$(current_version)" >&2
    read -r reply
    case "$reply" in
      current|patch|minor)
        printf '%s\n' "$reply"
        return 0
        ;;
      *)
        echo "Please answer 'current', 'patch', or 'minor'." >&2
        ;;
    esac
  done
}

confirm_release() {
  local branch="$1"
  local current="$2"
  local next="$3"
  local release_type="$4"

  echo
  echo "Current version : $current"
  if [[ "$release_type" == "current" ]]; then
    echo "Release version : $next (publish current version as-is)"
  else
    echo "Next version    : $next"
  fi
  echo "Branch          : $branch"
  echo "Changelog title : rumqttc-next $next"
  echo "Publish order   : ${PUBLISH_ORDER[*]}"
  echo
  printf 'Continue with this release? [y/N]: '
  read -r reply
  [[ "$reply" == "y" || "$reply" == "Y" ]]
}

replace_all_versions() {
  local old="$1"
  local new="$2"

  perl -0pi -e "s/^version = \"\Q$old\E\"/version = \"$new\"/m" mqttbytes-core/Cargo.toml
  perl -0pi -e "s/^version = \"\Q$old\E\"/version = \"$new\"/m" rumqttc-core/Cargo.toml

  perl -0pi -e '
    s/^version = "\Q'"$old"'\E"/version = "'"$new"'"/m;
    s/(rumqttc-core = \{[^}]*version = )"\Q'"$old"'\E"/$1"'"$new"'"/m;
    s/(mqttbytes-core = \{[^}]*version = )"\Q'"$old"'\E"/$1"'"$new"'"/m;
  ' rumqttc-v4/Cargo.toml

  perl -0pi -e '
    s/^version = "\Q'"$old"'\E"/version = "'"$new"'"/m;
    s/(rumqttc-core = \{[^}]*version = )"\Q'"$old"'\E"/$1"'"$new"'"/m;
    s/(mqttbytes-core = \{[^}]*version = )"\Q'"$old"'\E"/$1"'"$new"'"/m;
  ' rumqttc-v5/Cargo.toml

  perl -0pi -e '
    s/^version = "\Q'"$old"'\E"/version = "'"$new"'"/m;
    s/(rumqttc_v5 = \{[^}]*version = )"\Q'"$old"'\E"/$1"'"$new"'"/m;
  ' rumqttc-next/Cargo.toml
}

cut_changelog() {
  local version="$1"
  local today="$2"

  VERSION="$version" TODAY="$today" perl -0pi -e '
    my $version = $ENV{VERSION};
    my $today = $ENV{TODAY};
    s{
      ## \s \[Unreleased\] \n \n
      (.*?)
      \n \n --- \n
    }{
      my $unreleased = $1;
      <<"EOF";
## [Unreleased]

### Added
### Changed
### Deprecated
### Removed
### Fixed
### Security

---

## [rumqttc-next $version] - $today

$unreleased

---
EOF
    }xse
      or die "failed to cut CHANGELOG.md\n";
  ' CHANGELOG.md
}

assert_release_not_present() {
  local version="$1"
  local package

  if rg -n "\\[rumqttc-next ${version}\\]" CHANGELOG.md >/dev/null 2>&1; then
    echo "error: CHANGELOG.md already contains rumqttc-next $version" >&2
    exit 1
  fi

  for package in "${PACKAGE_NAMES[@]}"; do
    if git rev-parse -q --verify "refs/tags/${package}-${version}" >/dev/null; then
      echo "error: tag ${package}-${version} already exists" >&2
      exit 1
    fi
  done
}

verify_release() {
  cargo generate-lockfile

  cargo check --locked \
    -p mqttbytes-core-next \
    -p rumqttc-core-next \
    -p rumqttc-v4-next \
    -p rumqttc-v5-next \
    -p rumqttc-next

  cargo test --doc \
    -p mqttbytes-core-next \
    -p rumqttc-core-next \
    -p rumqttc-v4-next \
    -p rumqttc-v5-next \
    -p rumqttc-next

  if ! cargo audit; then
    echo >&2
    printf 'cargo audit reported findings. Continue anyway? [y/N]: ' >&2
    read -r reply
    if [[ "$reply" != "y" && "$reply" != "Y" ]]; then
      echo "aborted" >&2
      exit 1
    fi
  fi
}

commit_release() {
  local version="$1"
  git add CHANGELOG.md Cargo.lock "${MANIFESTS[@]}"
  git commit -m "release(packages): cut ${version}" \
    -m "Prepare the coordinated rumqttc-next crates for release ${version}, cut the changelog, and verify the publishable packages."
}

wait_for_crate_version() {
  local package="$1"
  local version="$2"
  local attempt

  for attempt in $(seq 1 30); do
    if curl -fsS "https://crates.io/api/v1/crates/${package}/${version}" >/dev/null 2>&1; then
      return 0
    fi

    echo "waiting for crates.io to index ${package} ${version} (${attempt}/30)"
    sleep 10
  done

  echo "error: ${package} ${version} did not appear on crates.io in time" >&2
  exit 1
}

publish_release() {
  local version="$1"
  local package

  for package in "${PUBLISH_ORDER[@]}"; do
    echo
    echo "Publishing ${package} ${version}"
    cargo publish --locked -p "$package"
    wait_for_crate_version "$package" "$version"
  done
}

create_tags() {
  local version="$1"
  local package

  for package in "${PACKAGE_NAMES[@]}"; do
    git tag -a "${package}-${version}" -m "${package} ${version}"
  done
}

maybe_push() {
  local branch="$1"

  echo
  printf 'Push branch and tags to origin now? [y/N]: '
  read -r reply
  if [[ "$reply" == "y" || "$reply" == "Y" ]]; then
    git push origin "$branch"
    git push origin --tags
  else
    echo "Skipped pushing. Push later with:"
    echo "  git push origin ${branch}"
    echo "  git push origin --tags"
  fi
}

main() {
  local branch current release_type next today

  require_tools
  require_clean_tree

  branch="$(git branch --show-current)"
  current="$(current_version)"

  assert_versions_match "$current"

  release_type="$(prompt_release_type)"
  next="$(next_version "$current" "$release_type")"
  today="$(date +%d-%m-%Y)"

  assert_release_not_present "$next"

  if ! confirm_release "$branch" "$current" "$next" "$release_type"; then
    echo "aborted"
    exit 1
  fi

  replace_all_versions "$current" "$next"
  cut_changelog "$next" "$today"

  verify_release
  commit_release "$next"
  publish_release "$next"
  create_tags "$next"
  maybe_push "$branch"

  echo
  echo "Published coordinated release ${next}."
}

main "$@"
