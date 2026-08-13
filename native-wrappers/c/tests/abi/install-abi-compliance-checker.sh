#!/usr/bin/env bash
set -euo pipefail

version=7c175c45a8ba9ac41b8e47d8ebbab557b623b18e
ref=refs/heads/master
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
install_dir="${1:-${repo_dir}/target/abi-tools/abi-compliance-checker}"

if [[ -x "${install_dir}/abi-compliance-checker.pl" ]] &&
    [[ "$(git -C "${install_dir}" rev-parse HEAD)" == "${version}" ]]; then
    printf '%s\n' "${install_dir}/abi-compliance-checker.pl"
    exit 0
fi

mkdir -p "$(dirname "${install_dir}")"
if [[ ! -d "${install_dir}/.git" ]]; then
    git clone --filter=blob:none --no-checkout https://github.com/lvc/abi-compliance-checker.git "${install_dir}"
fi
git -C "${install_dir}" fetch --depth 1 origin "${ref}"
test "$(git -C "${install_dir}" rev-parse FETCH_HEAD)" = "${version}"
git -C "${install_dir}" checkout --detach FETCH_HEAD
test "$(git -C "${install_dir}" rev-parse HEAD)" = "${version}"
printf '%s\n' "${install_dir}/abi-compliance-checker.pl"
