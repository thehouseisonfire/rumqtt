use serde_json::Value;
use std::process::Command;

fn run_codec(protocol: &str) -> Value {
    let qos = if protocol == "nats" { "0" } else { "1" };
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
            qos,
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

fn run_validation_cost(protocol: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_rumqtt-bench"))
        .args([
            "codec",
            "validation-cost",
            "--protocol",
            protocol,
            "--rounds",
            "3",
            "--messages",
            "1000",
            "--payload-size",
            "8",
            "--qos",
            "1",
            "--run-id",
            "validation-cost-smoke",
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
fn nats_codec_roundtrip_emits_stable_json() {
    let json = run_codec("nats");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["scenario"], "codec-nats-roundtrip");
    assert_eq!(json["config"]["protocol"], "nats");
    assert_eq!(json["config"]["qos"], 0);
    assert!(json["metrics"]["messages_sec"].as_f64().unwrap_or(0.0) > 0.0);
}

#[cfg(not(feature = "profiling"))]
#[test]
fn profile_output_requires_profiling_feature() {
    let output = Command::new(env!("CARGO_BIN_EXE_rumqtt-bench"))
        .args([
            "codec",
            "decode",
            "--protocol",
            "v5",
            "--messages",
            "1",
            "--profile-output",
            "unused.pb",
        ])
        .output()
        .expect("failed to run rumqtt-bench");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("requires building benchmarks with --features profiling")
    );
}

#[test]
fn nats_codec_rejects_mqtt_qos() {
    let output = Command::new(env!("CARGO_BIN_EXE_rumqtt-bench"))
        .args([
            "codec",
            "decode",
            "--protocol",
            "nats",
            "--messages",
            "1",
            "--qos",
            "1",
        ])
        .output()
        .expect("failed to run rumqtt-bench");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("require --qos 0"));
}

#[cfg(feature = "url")]
fn run_parse_url(protocol: &str) -> Value {
    let url = format!("mqtt://localhost:1883?client_id=bench-{protocol}&keep_alive_secs=30");
    let output = Command::new(env!("CARGO_BIN_EXE_rumqtt-bench"))
        .args([
            "options",
            "parse-url",
            "--protocol",
            protocol,
            "--parses",
            "100",
            "--url",
            &url,
            "--run-id",
            "parse-url-smoke",
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

#[test]
fn codec_validation_cost_emits_paired_samples_for_both_protocols() {
    for protocol in ["v4", "v5"] {
        let json = run_validation_cost(protocol);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["run_id"], "validation-cost-smoke");
        assert_eq!(
            json["scenario"],
            format!("codec-{protocol}-validation-cost")
        );
        assert_eq!(json["config"]["protocol"], protocol);
        assert_eq!(json["metrics"]["rounds"], 3.0);
        assert_eq!(
            json["samples"]["checked_elapsed_sec"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            json["samples"]["prevalidated_elapsed_sec"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert!(
            json["metrics"]["checked_messages_sec"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.0
        );
        assert!(
            json["metrics"]["prevalidated_messages_sec"]
                .as_f64()
                .unwrap_or(0.0)
                > 0.0
        );
    }
}

#[cfg(feature = "url")]
#[test]
fn v5_parse_url_emits_stable_json() {
    let json = run_parse_url("v5");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["run_id"], "parse-url-smoke");
    assert_eq!(json["scenario"], "options-parse-url-v5");
    assert_eq!(json["config"]["protocol"], "v5");
    assert_eq!(json["metrics"]["parses"], 100.0);
    assert!(json["metrics"]["parses_sec"].as_f64().unwrap_or(0.0) > 0.0);
}
