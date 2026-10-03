fn main() {
    println!("cargo:rustc-check-cfg=cfg(native_tls_version_bounds)");
    for key in [
        "DEP_OPENSSL_VERSION_NUMBER",
        "DEP_OPENSSL_LIBRESSL_VERSION_NUMBER",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let version = |key| {
        std::env::var(key)
            .ok()
            .and_then(|value| u64::from_str_radix(&value, 16).ok())
    };
    // Match native-tls's have_min_max_version gate. Older implementations can
    // silently omit maximum-version restrictions, so do not expose them.
    if version("DEP_OPENSSL_VERSION_NUMBER").is_some_and(|v| v >= 0x1010_0000)
        || version("DEP_OPENSSL_LIBRESSL_VERSION_NUMBER").is_some_and(|v| v >= 0x2060_1000)
    {
        println!("cargo:rustc-cfg=native_tls_version_bounds");
    }
    println!("cargo:rerun-if-changed=tests/fixtures/transport.c");
    #[cfg(feature = "transport-proof")]
    {
        let mut build = cc::Build::new();
        build
            .file("tests/fixtures/transport.c")
            .warnings_into_errors(true);
        if build.get_compiler().is_like_msvc() {
            build.flag("/std:c11");
        } else {
            build.flag("-std=c11").flag("-Wextra").flag("-Wpedantic");
        }
        build.compile("rumqttc_transport_proof");
    }
}
