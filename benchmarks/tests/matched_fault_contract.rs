use benchmarks::synthetic_router::{FaultAction, FaultConfig, RouterConfig};
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::process::Command;

const TOPIC: &str = "bench/matched/fault-contract";

async fn run_fault(client: &str, action: FaultAction, delay: Duration) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = tokio::spawn(benchmarks::synthetic_router::run_with_config(
        listener,
        RouterConfig {
            fault: Some(FaultConfig {
                action,
                topic: TOPIC.into(),
                occurrence: 1,
                delay,
            }),
        },
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_rumqtt-library-bench"));
    command.kill_on_drop(true).args([
        "--client",
        client,
        "--run-id",
        "fault-contract",
        "throughput",
        "--broker-url",
        &format!("mqtt://{address}"),
        "--topic",
        TOPIC,
        "--qos",
        "1",
        "--payload-size",
        "16",
        "--warmup-sec",
        "0",
        "--duration-sec",
        "1",
        "--drain-sec",
        "2",
        "--window",
        "1",
        "--receive-maximum",
        "1",
        "--keep-alive-seconds",
        "2",
        "--operation-timeout-sec",
        "1",
        "--publishers",
        "1",
        "--subscribers",
        "1",
    ]);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("benchmark subprocess must terminate")
        .expect("benchmark subprocess must start");
    router.abort();
    let _ = router.await;
    assert!(
        output.status.success(),
        "{client} benchmark failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("benchmark output must be JSON")
}

async fn for_both(action: FaultAction, delay: Duration) -> Vec<Value> {
    let mut outputs = Vec::new();
    for client in ["rumqttc", "mqtt5"] {
        outputs.push(run_fault(client, action, delay).await);
    }
    outputs
}

#[tokio::test]
async fn dropped_delivery_reports_loss_and_invalidates_both_adapters() {
    for output in for_both(FaultAction::DropDelivery, Duration::ZERO).await {
        assert_eq!(output["quality"]["valid"], false);
        assert_eq!(output["metrics"]["lost"], 1.0);
    }
}

#[tokio::test]
async fn duplicate_delivery_is_counted_and_invalidates_both_adapters() {
    for output in for_both(FaultAction::DuplicateDelivery, Duration::ZERO).await {
        assert_eq!(output["quality"]["valid"], false);
        assert_eq!(output["metrics"]["duplicates"], 1.0);
    }
}

#[tokio::test]
async fn post_deadline_delivery_is_only_counted_during_drain() {
    for output in for_both(FaultAction::DelayDelivery, Duration::from_millis(1200)).await {
        assert_eq!(output["metrics"]["lost"], 0.0);
        assert!(output["metrics"]["drain_deliveries"].as_f64().unwrap() >= 1.0);
        assert_eq!(output["quality"]["valid"], true);
    }
}

#[tokio::test]
async fn negative_puback_is_classified_and_invalidates_both_adapters() {
    for output in for_both(FaultAction::RejectPublish, Duration::ZERO).await {
        assert_eq!(output["quality"]["valid"], false);
        assert!(output["metrics"]["publish_failures"].as_f64().unwrap() >= 1.0);
        assert!(output["metrics"]["rejected"].as_f64().unwrap() >= 1.0);
    }
}

#[tokio::test]
async fn disconnect_with_publish_outstanding_terminates_and_invalidates() {
    for output in for_both(FaultAction::DisconnectPublisher, Duration::ZERO).await {
        assert_eq!(output["quality"]["valid"], false);
        assert!(
            output["metrics"]["publish_failures"].as_f64().unwrap()
                + output["metrics"]["publish_timeouts"].as_f64().unwrap()
                >= 1.0
        );
    }
}

#[tokio::test]
async fn withheld_completion_times_out_and_invalidates_both_adapters() {
    for output in for_both(FaultAction::WithholdPuback, Duration::ZERO).await {
        assert_eq!(output["quality"]["valid"], false);
        assert!(output["metrics"]["publish_timeouts"].as_f64().unwrap() >= 1.0);
    }
}
