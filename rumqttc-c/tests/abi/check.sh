#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
crate_dir="${repo_dir}/rumqttc-c"
target_dir="${repo_dir}/target/debug"

cargo build --manifest-path "${crate_dir}/Cargo.toml"

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

cc -std=c11 -Wall -Wextra -Werror -I"${crate_dir}/include" \
    "${crate_dir}/tests/c/header_smoke.c" -L"${target_dir}" -lrumqttc \
    -Wl,-rpath,"${target_dir}" -o "${target_dir}/rumqttc-header-smoke-c"
c++ -std=c++17 -Wall -Wextra -Werror -I"${crate_dir}/include" \
    "${crate_dir}/tests/c/header_smoke.cpp" -L"${target_dir}" -lrumqttc \
    -Wl,-rpath,"${target_dir}" -o "${target_dir}/rumqttc-header-smoke-cpp"
"${target_dir}/rumqttc-header-smoke-c"
"${target_dir}/rumqttc-header-smoke-cpp"

generated_header="$(find "${target_dir}/build" -path '*rumqttc-c-next*/out/rumqttc.generated.h' -print0 \
    | xargs -0 ls -t | head -1)"
generated_functions="$(find "${target_dir}/build" -path '*rumqttc-c-next*/out/rumqttc.generated-functions.h' -print0 \
    | xargs -0 ls -t | head -1)"
sed -n 's/.*\(rumqttc_[A-Za-z0-9_]*\)(.*/\1/p' "${crate_dir}/include/rumqttc.h" | sort -u \
    > "${target_dir}/rumqttc-checked-functions"
sed -n 's/.*\(rumqttc_[A-Za-z0-9_]*\)(.*/\1/p' "${generated_header}" | sort -u \
    > "${target_dir}/rumqttc-generated-functions"
diff -u "${target_dir}/rumqttc-checked-functions" "${target_dir}/rumqttc-generated-functions"
cc -std=c11 -Wall -Wextra -Werror -x c -fsyntax-only "${generated_header}"
c++ -std=c++17 -Wall -Wextra -Werror -x c++ -fsyntax-only "${generated_header}"
printf '#include "%s"\n#include "%s"\n' "${crate_dir}/include/rumqttc.h" "${generated_functions}" \
    | cc -std=c11 -Wall -Wextra -Werror -x c -fsyntax-only -
printf '#include "%s"\n#include "%s"\n' "${crate_dir}/include/rumqttc.h" "${generated_functions}" \
    | c++ -std=c++17 -Wall -Wextra -Werror -x c++ -fsyntax-only -

case "$(uname -s)" in
    Linux)
        nm -D --defined-only "${target_dir}/librumqttc.so" | awk '{print $3}' | sort \
            > "${target_dir}/rumqttc-exported-symbols"
        diff -u "${crate_dir}/tests/abi/rumqttc-v1.symbols" \
            "${target_dir}/rumqttc-exported-symbols"
        ;;
    Darwin)
        nm -gU "${target_dir}/librumqttc.dylib" | awk '{print $3}' | sed 's/^_//' | sort \
            > "${target_dir}/rumqttc-exported-symbols"
        diff -u "${crate_dir}/tests/abi/rumqttc-v1.symbols" \
            "${target_dir}/rumqttc-exported-symbols"
        ;;
esac
