#!/usr/bin/env bash
set -euo pipefail

workspace_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
crate_dir="${workspace_dir}/c"
report_dir="${ABI_REPORT_DIR:-${workspace_dir}/target/abi-reports}"
host_os="$(uname -s)"
host_arch="$(uname -m)"
case "${host_os}:${host_arch}" in
    Linux:x86_64) detected_platform=linux-x86_64 ;;
    Darwin:arm64) detected_platform=macos-arm64 ;;
    *)
        echo "unsupported ABI comparison host: ${host_os} ${host_arch}" >&2
        echo "supported hosts: Linux x86_64 and macOS arm64" >&2
        exit 2
        ;;
esac
platform="${RUMQTTC_ABI_PLATFORM:-${detected_platform}}"
if [[ "${platform}" != "${detected_platform}" ]]; then
    echo "ABI platform ${platform} does not match host ${detected_platform}" >&2
    exit 2
fi
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${crate_dir}/Cargo.toml" | head -1)"
abi_major="$(sed -n 's/^#define RUMQTTC_ABI_VERSION_MAJOR \([0-9]*\)u/\1/p' "${crate_dir}/include/rumqttc.h")"
abi_minor="$(sed -n 's/^#define RUMQTTC_ABI_VERSION_MINOR \([0-9]*\)u/\1/p' "${crate_dir}/include/rumqttc.h")"
if [[ "${abi_major}" == 0 ]]; then abi_line="0.${abi_minor}"; else abi_line="${abi_major}"; fi

mkdir -p "${report_dir}"
python3 "${crate_dir}/tests/abi/baseline.py" \
    --version "${version}" --platform "${platform}" --output "${report_dir}/baseline"
if [[ -f "${report_dir}/baseline/no-baseline" ]]; then
    exit 0
fi

case "${host_os}" in
    Linux)
        library="${workspace_dir}/target/release/librumqttc.so"
        abi_rustflags="-C link-arg=-Wl,-soname,librumqttc.so.${abi_line}"
        ;;
    Darwin)
        library="${workspace_dir}/target/release/librumqttc.dylib"
        abi_rustflags="-C link-arg=-Wl,-install_name,@rpath/librumqttc.${abi_line}.dylib"
        abi_rustflags+=" -C link-arg=-Wl,-compatibility_version,${abi_major}.${abi_minor}.0"
        abi_rustflags+=" -C link-arg=-Wl,-current_version,${abi_major}.${abi_minor}.0"
        ;;
esac
RUSTFLAGS="${RUSTFLAGS:-} ${abi_rustflags}" cargo build --locked --release \
    --manifest-path "${workspace_dir}/Cargo.toml" -p rumqttc-c-next
python3 "${crate_dir}/tests/abi/contract.py" generate \
    --header "${crate_dir}/include/rumqttc.h" \
    --library "${library}" \
    --package-version "${version}" \
    --target "${platform}" \
    --output "${report_dir}/current-contract.json"
baseline_contract="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["contract"])' \
    "${report_dir}/baseline/baseline.json")"
contract_status=0
python3 "${crate_dir}/tests/abi/contract.py" compare \
    --old "${baseline_contract}" --new "${report_dir}/current-contract.json" --mode containment \
    > "${report_dir}/contract-report.txt" 2>&1 || contract_status=$?

if [[ "${host_os}" != Linux ]]; then
    exit "${contract_status}"
fi

baseline_root="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["root"])' \
    "${report_dir}/baseline/baseline.json")"
baseline_header="${baseline_root}/include/rumqttc.h"
baseline_library="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["library"])' \
    "${report_dir}/baseline/baseline.json")"
tool="$(${crate_dir}/tests/abi/install-abi-compliance-checker.sh)"
printf '<version>%s</version>\n<headers>%s</headers>\n<libs>%s</libs>\n' \
    baseline "${baseline_header}" "${baseline_library}" > "${report_dir}/old.xml"
printf '<version>%s</version>\n<headers>%s</headers>\n<libs>%s</libs>\n' \
    current "${crate_dir}/include/rumqttc.h" "${library}" \
    > "${report_dir}/new.xml"
abicc_status=0
"${tool}" -l rumqttc -old "${report_dir}/old.xml" -new "${report_dir}/new.xml" \
    -binary -report-path "${report_dir}/abi-compliance-report.html" \
    > "${report_dir}/abi-compliance-checker.txt" 2>&1 || abicc_status=$?
printf 'ABI Compliance Checker supplemental exit status: %s\n' "${abicc_status}" \
    >> "${report_dir}/abi-compliance-checker.txt"
exit "${contract_status}"
