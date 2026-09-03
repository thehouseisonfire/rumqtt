use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;

enum ServerBehavior {
    Router,
    Refuse,
    WithholdConnack,
}

async fn run_connections(client: &str, behavior: ServerBehavior) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = match behavior {
        ServerBehavior::Router => tokio::spawn(async move {
            let _ = benchmarks::synthetic_router::run(listener).await;
        }),
        ServerBehavior::Refuse => tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = stream.write_all(&[0x20, 0x03, 0x00, 0x87, 0x00]).await;
                    let _ = stream.shutdown().await;
                });
            }
        }),
        ServerBehavior::WithholdConnack => tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        }),
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_rumqtt-library-bench"));
    command
        .current_dir(std::env::temp_dir())
        .kill_on_drop(true)
        .args([
            "--client",
            client,
            "--run-id",
            "connection-contract",
            "connections",
            "--broker-url",
            &format!("mqtt://{address}"),
            "--duration-sec",
            "1",
            "--concurrency",
            "1",
            "--keep-alive-seconds",
            "2",
            "--connect-timeout-sec",
            "1",
            "--disconnect-timeout-sec",
            "1",
            "--drain-sec",
            "2",
        ]);
    let output = tokio::time::timeout(Duration::from_secs(12), command.output())
        .await
        .expect("connection benchmark must terminate")
        .expect("connection benchmark must start");
    server.abort();
    let _ = server.await;
    assert!(
        output.status.success(),
        "{client}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("connection output must be JSON")
}

fn workspace_lock_sha256() -> String {
    let lockfile = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("benchmarks package must be inside the workspace root")
        .join("Cargo.lock");
    Sha256::digest(std::fs::read(lockfile).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn successful_cycles_require_connack_and_disconnect_completion() {
    for client in ["rumqttc", "mqtt5"] {
        let output = run_connections(client, ServerBehavior::Router).await;
        assert_eq!(output["quality"]["valid"], true);
        assert!(output["metrics"]["attempts"].as_f64().unwrap() > 0.0);
        assert!(output["metrics"]["successful_cycles"].as_f64().unwrap() > 0.0);
        assert_eq!(
            output["environment"]["cargo_lock_sha256"],
            workspace_lock_sha256()
        );
    }
}

#[tokio::test]
async fn refused_connections_are_classified_and_invalidate() {
    for client in ["rumqttc", "mqtt5"] {
        let output = run_connections(client, ServerBehavior::Refuse).await;
        assert_eq!(output["quality"]["valid"], false);
        assert!(output["metrics"]["connect_failures"].as_f64().unwrap() > 0.0);
        assert_eq!(output["metrics"]["successful_cycles"], 0.0);
    }
}

#[tokio::test]
async fn withheld_connack_times_out_within_the_configured_bound() {
    for client in ["rumqttc", "mqtt5"] {
        let output = run_connections(client, ServerBehavior::WithholdConnack).await;
        assert_eq!(output["quality"]["valid"], false);
        assert!(output["metrics"]["connect_timeouts"].as_f64().unwrap() > 0.0);
        assert_eq!(output["metrics"]["successful_cycles"], 0.0);
    }
}
