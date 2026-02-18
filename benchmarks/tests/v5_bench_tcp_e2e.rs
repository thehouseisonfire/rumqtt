use serde_json::Value;
use std::process::Command;

#[test]
#[ignore]
fn v5_bench_tcp_e2e() {
    let broker_url =
        std::env::var("RUMQTT_BENCH_TCP_URL").unwrap_or_else(|_| "mqtt://127.0.0.1:1883".to_string());

    let output = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "rumqttv5bench",
            "--release",
            "--",
            "--mode",
            "throughput",
            "--duration",
            "3",
            "--warmup",
            "1",
            "--url",
            &broker_url,
            "--qos",
            "1",
            "--publishers",
            "1",
            "--subscribers",
            "1",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("failed to execute rumqttv5bench");

    assert!(
        output.status.success(),
        "benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: Value = serde_json::from_slice(&output.stdout).expect("output must be JSON");
    assert_eq!(json["library"], "rumqtt-v5");
    assert_eq!(json["mode"], "throughput");
    assert_eq!(json["config"]["transport"], "tcp");
    assert!(json["results"]["throughput_avg"].as_f64().unwrap_or(0.0) >= 0.0);
}
