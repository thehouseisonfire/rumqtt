use serde_json::Value;
use std::process::Command;

fn run_codec(protocol: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rumqtt-bench"))
        .args([
            "codec",
            "roundtrip",
            "--protocol",
            protocol,
            "--messages",
            "1000",
            "--payload-size",
            "64",
            "--qos",
            "1",
            "--run-id",
            "codec-smoke",
        ])
        .output()
        .expect("failed to run rumqtt-bench");

    assert!(
        output.status.success(),
        "benchmark failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("benchmark output must be JSON")
}

#[test]
fn v4_codec_roundtrip_emits_stable_json() {
    let json = run_codec("v4");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["run_id"], "codec-smoke");
    assert_eq!(json["scenario"], "codec-v4-roundtrip");
    assert_eq!(json["config"]["protocol"], "v4");
    assert_eq!(json["metrics"]["messages"], 1000.0);
    assert!(json["metrics"]["messages_sec"].as_f64().unwrap_or(0.0) > 0.0);
}

#[test]
fn v5_codec_roundtrip_emits_stable_json() {
    let json = run_codec("v5");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["run_id"], "codec-smoke");
    assert_eq!(json["scenario"], "codec-v5-roundtrip");
    assert_eq!(json["config"]["protocol"], "v5");
    assert_eq!(json["metrics"]["messages"], 1000.0);
    assert!(json["metrics"]["messages_sec"].as_f64().unwrap_or(0.0) > 0.0);
}
