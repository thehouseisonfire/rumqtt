#!/usr/bin/env bash
set -euo pipefail

workspace_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
crate_dir="${workspace_dir}/c"
target_dir="${workspace_dir}/target/debug"
check="${1:-all}"
case "${check}" in
    all|package|native|ffi-header|exports) ;;
    *) echo "unknown C check: ${check}" >&2; exit 2 ;;
esac

cargo build --manifest-path "${crate_dir}/Cargo.toml"

if [[ "${check}" == all || "${check}" == package ]]; then
pkgconfig_build_dir="${target_dir}/pkgconfig-check"
pkgconfig_original_prefix="${target_dir}/pkgconfig-original"
pkgconfig_relocated_prefix="${target_dir}/pkgconfig-relocated"
cmake -S "${crate_dir}" -B "${pkgconfig_build_dir}" \
    -DCMAKE_INSTALL_PREFIX="${pkgconfig_original_prefix}"
mkdir -p "${pkgconfig_relocated_prefix}/include" \
    "${pkgconfig_relocated_prefix}/lib/pkgconfig"
cp "${pkgconfig_build_dir}/rumqttc.pc" \
    "${pkgconfig_relocated_prefix}/lib/pkgconfig/rumqttc.pc"
cp "${crate_dir}/include/rumqttc.h" "${pkgconfig_relocated_prefix}/include/"
cp "${target_dir}/librumqttc.a" "${pkgconfig_relocated_prefix}/lib/"

case "$(uname -s)" in
    Linux)
        expected_private='Libs.private: -lpthread -ldl -lm'
        ;;
    Darwin)
        expected_private='Libs.private: -lpthread -lm -framework Security'
        expected_private+=' -framework CoreFoundation -framework SystemConfiguration'
        ;;
esac
grep -Fx 'prefix=${pcfiledir}/../..' \
    "${pkgconfig_relocated_prefix}/lib/pkgconfig/rumqttc.pc"
grep -Fx "${expected_private}" "${pkgconfig_relocated_prefix}/lib/pkgconfig/rumqttc.pc"
pkgconfig_flags="$(PKG_CONFIG_PATH="${pkgconfig_relocated_prefix}/lib/pkgconfig" \
    pkg-config --static --cflags --libs rumqttc)"
case "${pkgconfig_flags}" in
    *"${pkgconfig_original_prefix}"*)
        echo "pkg-config output retained the original install prefix: ${pkgconfig_flags}" >&2
        exit 1
        ;;
    *"${pkgconfig_relocated_prefix}"*) ;;
    *)
        echo "pkg-config output did not resolve relative to its relocated file: ${pkgconfig_flags}" >&2
        exit 1
        ;;
esac
read -r -a pkgconfig_args <<< "${pkgconfig_flags}"
cc -std=c11 -Wall -Wextra -Werror -DRUMQTTC_STATIC \
    "${crate_dir}/tests/c/header_smoke.c" "${pkgconfig_args[@]}" \
    -o "${target_dir}/rumqttc-pkgconfig-static"
"${target_dir}/rumqttc-pkgconfig-static"
fi

if [[ "${check}" == all || "${check}" == native ]]; then
cc -std=c11 -Wall -Wextra -Werror -I"${crate_dir}/include" \
    "${crate_dir}/tests/c/header_smoke.c" -L"${target_dir}" -lrumqttc \
    -Wl,-rpath,"${target_dir}" -o "${target_dir}/rumqttc-header-smoke-c"
c++ -std=c++17 -Wall -Wextra -Werror -I"${crate_dir}/include" \
    "${crate_dir}/tests/c/header_smoke.cpp" -L"${target_dir}" -lrumqttc \
    -Wl,-rpath,"${target_dir}" -o "${target_dir}/rumqttc-header-smoke-cpp"
"${target_dir}/rumqttc-header-smoke-c"
"${target_dir}/rumqttc-header-smoke-cpp"
fi

if [[ "${check}" == all || "${check}" == ffi-header ]]; then
generated_header="$(find "${target_dir}/build" -path '*rumqttc-c-next*/out/rumqttc.generated.h' -print0 \
    | xargs -0 ls -t | head -1)"
generated_functions="$(find "${target_dir}/build" -path '*rumqttc-c-next*/out/rumqttc.generated-functions.h' -print0 \
    | xargs -0 ls -t | head -1)"
python3 "${crate_dir}/tests/abi/contract.py" generate \
    --header "${crate_dir}/include/rumqttc.h" --output "${target_dir}/rumqttc-checked-contract.json"
python3 "${crate_dir}/tests/abi/contract.py" generate \
    --header "${generated_header}" --output "${target_dir}/rumqttc-generated-contract.json"
python3 - "${target_dir}/rumqttc-checked-contract.json" "${target_dir}/rumqttc-generated-contract.json" <<'PY'
import json
import sys

checked_contract = json.load(open(sys.argv[1], encoding="utf-8"))
generated_contract = json.load(open(sys.argv[2], encoding="utf-8"))
for category in ("functions", "records"):
    checked = set(checked_contract[category])
    generated = set(generated_contract[category])
    if checked != generated:
        raise SystemExit(
            f"checked/generated {category} differ: "
            f"checked-only={sorted(checked-generated)}, "
            f"generated-only={sorted(generated-checked)}"
        )
PY
python3 "${crate_dir}/tests/abi/contract.py" compare \
    --old "${target_dir}/rumqttc-checked-contract.json" \
    --new "${target_dir}/rumqttc-generated-contract.json" \
    --mode containment --categories functions,records \
    > "${target_dir}/rumqttc-ffi-source-differences.txt"
cc -std=c11 -Wall -Wextra -Werror -x c -fsyntax-only "${generated_header}"
c++ -std=c++17 -Wall -Wextra -Werror -x c++ -fsyntax-only "${generated_header}"
printf '#include "%s"\n#include "%s"\n' "${crate_dir}/include/rumqttc.h" "${generated_functions}" \
    | cc -std=c11 -Wall -Wextra -Werror -x c -fsyntax-only -
printf '#include "%s"\n#include "%s"\n' "${crate_dir}/include/rumqttc.h" "${generated_functions}" \
    | c++ -std=c++17 -Wall -Wextra -Werror -x c++ -fsyntax-only -
fi

if [[ "${check}" == all || "${check}" == exports ]]; then
case "$(uname -s)" in
    Darwin) library="${target_dir}/librumqttc.dylib" ;;
    *) library="${target_dir}/librumqttc.so" ;;
esac
python3 "${crate_dir}/tests/abi/contract.py" generate \
    --header "${crate_dir}/include/rumqttc.h" --library "${library}" \
    --output "${target_dir}/rumqttc-abi-contract.json"
python3 "${crate_dir}/tests/abi/contract.py" verify-exports \
    --contract "${target_dir}/rumqttc-abi-contract.json"
fi
