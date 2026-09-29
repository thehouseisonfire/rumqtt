fn main() {
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
