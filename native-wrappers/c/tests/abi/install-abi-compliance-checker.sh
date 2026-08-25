#!/usr/bin/env bash
set -euo pipefail

version=8e819827e8d707c7addc4a08f5cf74045f2302bb
archive_sha256=20f6644a68e52d19a874f9da97f02b6821c0c586f37938aa79832d3496a67878
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
install_dir="${1:-${repo_dir}/target/abi-tools/abi-compliance-checker}"
if [[ -z "${install_dir}" || "${install_dir}" == / ]]; then
    printf 'refusing unsafe ABI checker install directory: %q\n' "${install_dir}" >&2
    exit 2
fi

if [[ -x "${install_dir}/abi-compliance-checker.pl" ]] &&
    [[ "$(cat "${install_dir}/.rumqttc-version" 2>/dev/null || true)" == "${version}" ]]; then
    printf '%s\n' "${install_dir}/abi-compliance-checker.pl"
    exit 0
fi

mkdir -p "$(dirname "${install_dir}")"
archive="$(mktemp)"
staging="$(mktemp -d "$(dirname "${install_dir}")/abi-compliance-checker.XXXXXX")"
trap 'rm -f "${archive}"; rm -rf "${staging}"' EXIT
curl --fail --location --retry 3 --silent --show-error \
    "https://codeload.github.com/lvc/abi-compliance-checker/tar.gz/${version}" \
    --output "${archive}"
python3 - "${archive}" "${archive_sha256}" <<'PY'
import hashlib
import pathlib
import sys

archive = pathlib.Path(sys.argv[1])
expected = sys.argv[2]
actual = hashlib.sha256(archive.read_bytes()).hexdigest()
if actual != expected:
    raise SystemExit(f"ABI checker archive checksum mismatch: expected {expected}, got {actual}")
PY
tar -xzf "${archive}" --strip-components=1 -C "${staging}"
printf '%s\n' "${version}" >"${staging}/.rumqttc-version"
rm -rf "${install_dir}"
mv "${staging}" "${install_dir}"
chmod +x "${install_dir}/abi-compliance-checker.pl"
printf '%s\n' "${install_dir}/abi-compliance-checker.pl"
