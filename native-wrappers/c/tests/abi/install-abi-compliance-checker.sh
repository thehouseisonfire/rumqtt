#!/usr/bin/env bash
set -euo pipefail

version=8e819827e8d707c7addc4a08f5cf74045f2302bb
ref=refs/tags/2.3
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
resolved="$(git -C "${install_dir}" rev-parse 'FETCH_HEAD^{}')"
test "${resolved}" = "${version}"
git -C "${install_dir}" checkout --detach "${resolved}"
test "$(git -C "${install_dir}" rev-parse HEAD)" = "${version}"
chmod +x "${install_dir}/abi-compliance-checker.pl"
printf '%s\n' "${install_dir}/abi-compliance-checker.pl"
