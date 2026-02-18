use serde_json::Value;
use std::process::Command;

#[test]
#[ignore]
fn v5_bench_tls_e2e() {
    let broker_url = match std::env::var("RUMQTT_BENCH_TLS_URL") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("RUMQTT_BENCH_TLS_URL not set; skipping TLS e2e benchmark test");
            return;
        }
    };
    let ca_cert = match std::env::var("RUMQTT_BENCH_CA_CERT") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("RUMQTT_BENCH_CA_CERT not set; skipping TLS e2e benchmark test");
            return;
        }
    };

    let output = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "rumqttv5bench",
            "--release",
            "--",
            "--mode",
            "latency",
            "--duration",
            "3",
            "--warmup",
            "1",
            "--url",
            &broker_url,
            "--ca-cert",
            &ca_cert,
            "--qos",
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
    assert_eq!(json["mode"], "latency");
    assert_eq!(json["config"]["transport"], "tls");

    let p50 = json["results"]["p50_us"].as_u64().unwrap_or(0);
    let p95 = json["results"]["p95_us"].as_u64().unwrap_or(0);
    let p99 = json["results"]["p99_us"].as_u64().unwrap_or(0);
    assert!(p50 <= p95 && p95 <= p99);
}
