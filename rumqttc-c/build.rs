use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=cbindgen-functions.toml");
    println!("cargo:rerun-if-changed=include/rumqttc.h");

    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("rumqttc.generated.h");
    let config = cbindgen::Config::from_file(crate_dir.join("cbindgen.toml"))
        .expect("valid cbindgen configuration");
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .expect("generate candidate C declarations")
        .write_to_file(output);

    let function_output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("rumqttc.generated-functions.h");
    let function_config = cbindgen::Config::from_file(crate_dir.join("cbindgen-functions.toml"))
        .expect("valid cbindgen function configuration");
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(function_config)
        .generate()
        .expect("generate candidate C function declarations")
        .write_to_file(function_output);
}
