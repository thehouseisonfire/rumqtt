#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

EXPECTED_GITHUB_USER="${EXPECTED_GITHUB_USER:-thehouseisonfire}"

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

VERSIONED_DOCUMENTS=(
    "README.md"
    "MIGRATION.md"
    "docs/recipes/proxies.md"
    "docs/recipes/tls.md"
    "docs/recipes/tracing.md"
    "docs/recipes/websockets.md"
    "rumqttc-next/README.md"
    "rumqttc-v4/README.md"
    "rumqttc-v5/README.md"
)

require_clean_tree() {
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "error: git working tree is not clean" >&2
        git status --short
        exit 1
    fi
}

require_tool() {
    local tool="$1"

    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: required tool '$tool' not found" >&2
        exit 1
    fi
}

require_tools() {
    local tool
    for tool in awk cargo cargo-audit curl git grep perl python3 rg; do
        require_tool "$tool"
    done
}

require_github_user() {
    local actual_user

    if ! actual_user="$(gh api user --jq .login 2>/dev/null)"; then
        echo "error: GitHub CLI is not logged in or cannot access the current user" >&2
        echo "run: gh auth login" >&2
        exit 1
    fi

    if [[ "$actual_user" != "$EXPECTED_GITHUB_USER" ]]; then
        echo "error: GitHub CLI is logged in as '$actual_user', expected '$EXPECTED_GITHUB_USER'" >&2
        echo "run: gh auth switch --user '$EXPECTED_GITHUB_USER'" >&2
        exit 1
    fi
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

assert_unreleased_has_changes() {
    if ! awk '
    NR == 1 && $0 != "## [Unreleased]" { exit 1 }
    NR > 1 && $0 == "---" { exit found ? 0 : 1 }
    NR > 1 && /^- / { found = 1 }
    END { if (!found) exit 1 }
  ' CHANGELOG.md; then
        echo "error: CHANGELOG.md has no entries in its Unreleased section" >&2
        exit 1
    fi
}

assert_release_channel() {
    local version="$1"

    if [[ "$version" != *-* ]]; then
        echo "error: ${version} is not a prerelease; use ./scripts/publish-crates.sh" >&2
        exit 1
    fi
}

confirm_unmanaged_version_references() {
    local old="$1"
    local matches

    matches="$(git grep -n -F "$old" -- . \
        ':(exclude)Cargo.lock' \
        ':(exclude)CHANGELOG.md' \
        ':(exclude)mqttbytes-core/Cargo.toml' \
        ':(exclude)rumqttc-core/Cargo.toml' \
        ':(exclude)rumqttc-v4/Cargo.toml' \
        ':(exclude)rumqttc-v5/Cargo.toml' \
        ':(exclude)rumqttc-next/Cargo.toml' \
        ':(exclude)README.md' \
        ':(exclude)MIGRATION.md' \
        ':(exclude)docs/recipes/proxies.md' \
        ':(exclude)docs/recipes/tls.md' \
        ':(exclude)docs/recipes/tracing.md' \
        ':(exclude)docs/recipes/websockets.md' \
        ':(exclude)rumqttc-next/README.md' \
        ':(exclude)rumqttc-v4/README.md' \
        ':(exclude)rumqttc-v5/README.md' || true)"

    if [[ -z "$matches" ]]; then
        return 0
    fi

    echo >&2
    echo "warning: references to the old version remain in files the release script does not rewrite:" >&2
    echo "$matches" >&2
    echo >&2
    printf 'Proceed without changing these references? [y/N]: ' >&2
    read -r reply
    [[ "$reply" == "y" || "$reply" == "Y" ]]
}

confirm_documented_package_versions() {
    local expected="$1"
    local references stale notes_heading

    references="$(rg -n \
        '(cargo add (rumqttc-next|rumqttc-v[45]-next)@|package = "rumqttc-(next|v[45]-next)"|rumqttc-(next|v[45]-next) = \{)' \
        "${VERSIONED_DOCUMENTS[@]}" || true)"
    stale="$(printf '%s\n' "$references" | grep -Fv "$expected" || true)"

    if [[ -f RELEASE-NOTES.md ]] && grep -q '[^[:space:]]' RELEASE-NOTES.md; then
        notes_heading="$(grep -m 1 '^# rumqttc-next ' RELEASE-NOTES.md || true)"
        if [[ -n "$notes_heading" && "$notes_heading" != "# rumqttc-next ${expected}" ]]; then
            stale="${stale}${stale:+$'\n'}RELEASE-NOTES.md: ${notes_heading}"
        fi
    fi

    if [[ -z "$stale" ]]; then
        return 0
    fi

    echo >&2
    echo "warning: consumer-facing release references do not use version ${expected}:" >&2
    echo "$stale" >&2
    echo >&2
    printf 'Proceed with these documentation versions? [y/N]: ' >&2
    read -r reply
    [[ "$reply" == "y" || "$reply" == "Y" ]]
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
        current | patch | minor)
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
    if [[ -f RELEASE-NOTES.md ]] && grep -q '[^[:space:]]' RELEASE-NOTES.md; then
        echo "Release notes   : RELEASE-NOTES.md followed by CHANGELOG.md"
    else
        echo "Release notes   : CHANGELOG.md"
    fi
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

    if [[ "$old" != "$new" ]]; then
        OLD_VERSION="$old" NEW_VERSION="$new" perl -0pi -e '
      s/\Q$ENV{OLD_VERSION}\E/$ENV{NEW_VERSION}/g
    ' "${VERSIONED_DOCUMENTS[@]}"
    fi
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
      $unreleased =~ s{^\#\# \s \[Unreleased\] \n (?=\n \#\#\# \s Added \n)}{}xmg;
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

    s{
      ( ^\#\# \s \[[^\]]+\] [^\n]* \n \n )
      \#\# \s \[Unreleased\] \n \n
      (?= \#\#\# \s Added \n )
    }{$1}xmsg;
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
    cargo fmt --all --check
    python3 scripts/check-markdown-links.py

    cargo check \
        -p mqttbytes-core-next \
        -p rumqttc-core-next \
        -p rumqttc-v4-next \
        -p rumqttc-v5-next \
        -p rumqttc-next

    cargo test --locked --doc \
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
    git add CHANGELOG.md Cargo.lock "${MANIFESTS[@]}" "${VERSIONED_DOCUMENTS[@]}"
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

build_github_release_notes() {
    local version="$1"

    if ! grep -Fq "## [rumqttc-next ${version}] - " CHANGELOG.md; then
        echo "error: CHANGELOG.md has no release section for rumqttc-next ${version}" >&2
        return 1
    fi

    if [[ -f RELEASE-NOTES.md ]] && grep -q '[^[:space:]]' RELEASE-NOTES.md; then
        perl -0pe 's/[[:space:]]+\z/\n/' RELEASE-NOTES.md
        echo
    fi

    echo '## Changelog'
    echo
    awk -v heading="## [rumqttc-next ${version}]" '
    index($0, heading) == 1 {
        found = 1
        next
    }
    found && $0 == "---" {
        exit
    }
    found {
        if (!started && $0 == "") {
            next
        }
        started = 1
        print
    }
    END {
        if (!found) {
            exit 1
        }
    }
  ' CHANGELOG.md
}

create_github_release() {
    local version="$1"

    echo
    echo "Creating GitHub prerelease rumqttc-next-${version}"
    build_github_release_notes "$version" |
        gh release create "rumqttc-next-${version}" \
            --verify-tag \
            --title "rumqttc-next ${version}" \
            --notes-file - \
            --prerelease \
            --latest=false
}

maybe_push_and_release() {
    local branch="$1"
    local version="$2"

    echo
    printf 'Push branch and tags, then create the GitHub prerelease now? [y/N]: '
    read -r reply
    if [[ "$reply" == "y" || "$reply" == "Y" ]]; then
        git push origin "$branch"
        git push origin --tags
        create_github_release "$version"
    else
        echo "Skipped GitHub publication. Push later with:"
        echo "  git push origin ${branch}"
        echo "  git push origin --tags"
        echo "Then create the GitHub prerelease from tag rumqttc-next-${version}."
    fi
}

main() {
    local branch current release_type next today

    require_tool gh
    require_github_user
    require_tools
    require_clean_tree

    branch="$(git branch --show-current)"
    current="$(current_version)"

    assert_versions_match "$current"
    assert_unreleased_has_changes

    release_type="$(prompt_release_type)"
    next="$(next_version "$current" "$release_type")"
    today="$(date +%d-%m-%Y)"

    assert_release_channel "$next"
    assert_release_not_present "$next"

    if [[ "$current" != "$next" ]] && ! confirm_unmanaged_version_references "$current"; then
        echo "aborted"
        exit 1
    fi

    if ! confirm_release "$branch" "$current" "$next" "$release_type"; then
        echo "aborted"
        exit 1
    fi

    replace_all_versions "$current" "$next"

    if ! confirm_documented_package_versions "$next"; then
        echo "aborted; review the generated version changes before retrying"
        exit 1
    fi

    cut_changelog "$next" "$today"

    verify_release
    commit_release "$next"
    publish_release "$next"
    create_tags "$next"
    maybe_push_and_release "$branch" "$next"

    echo
    echo "Published coordinated release ${next}."
}

main "$@"
