# Broker-Acknowledged Sequential Publishing

Use a tracked publish and wait for its completion before submitting the next
message. Drive the event loop in a separate task so that it can receive the
broker acknowledgement that completes the notice.

This pattern is the same for the MQTT 3.1.1 (v4) and MQTT 5 clients:

```rust,no_run
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, QoS};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let options = MqttOptions::new("sequential-publisher", ("localhost", 1883));
let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();

tokio::spawn(async move {
    while let Ok(event) = eventloop.poll().await {
        println!("{event:?}");
    }
});

for payload in ["first", "second", "third"] {
    client
        .publish_tracked(
            "events/ordered",
            payload,
            PublishOptions::new(QoS::AtLeastOnce),
        )
        .await?
        .wait_completion_async()
        .await?;
}
# Ok(())
# }
```

For QoS 1, completion means that `PUBACK` was received. For QoS 2, it means
that the handshake finished with `PUBCOMP`. MQTT defines no broker
acknowledgement for QoS 0, so a QoS 0 tracked notice completes when the publish
is flushed to the network instead.

This serializes only publishers that follow the pattern. Other client clones
can still submit publishes concurrently.

For complete tracked publish, subscribe, and unsubscribe examples, see:

- v4: `rumqttc-v4/examples/tracked_notices.rs`
- v5: `rumqttc-v5/examples/tracked_notices_v5.rs`

See the [backpressure recipe](./backpressure.md) for request-channel sizing and
event-loop progress considerations when using bounded clients.
