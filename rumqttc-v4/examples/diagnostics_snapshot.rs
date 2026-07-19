use rumqttc::{AsyncClient, ConnectionError, MqttOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let options = MqttOptions::new("diagnostics-v4", ("localhost", 1883));
    let (_client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();

    loop {
        let poll_result = eventloop.poll().await;
        let snapshot = eventloop.diagnostics();

        println!(
            "connected={}, queued={}, channel={}, inflight={}/{}",
            snapshot.connected,
            snapshot.queues.pending_len,
            snapshot.queues.requests_rx_len,
            snapshot.outbound.inflight,
            snapshot.outbound.max_inflight,
        );

        match poll_result {
            Ok(_event) => {}
            Err(ConnectionError::RequestsDone) => break,
            Err(error) => eprintln!("connection error: {error}"),
        }
    }
}
