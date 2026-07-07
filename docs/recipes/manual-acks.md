# Manual ACK Recipes

Manual ACK mode lets the application decide when incoming QoS 1 and QoS 2
publishes are acknowledged.

Compile-checked examples:

- v4: `rumqttc-v4/examples/async_manual_acks.rs`
- v5: `rumqttc-v5/examples/async_manual_acks_v5.rs`

## Basic Pattern

Configure manual ACK mode before building the client:

```rust,no_run
use rumqttc::{AckMode, MqttOptions};

let mut options = MqttOptions::new("client-id", ("localhost", 1883));
options.set_ack_mode(AckMode::Manual);
```

When a publish arrives, prepare the matching ACK and send it after application
processing succeeds:

```rust,no_run
# use rumqttc::{AsyncClient, ManualAck, Publish};
# async fn ack_publish(client: AsyncClient, publish: Publish) {
if let Some(ack) = client.prepare_ack(&publish) {
    client.manual_ack(ack).await.expect("ACK should be queued");
}
# }
```

## Deadlock Avoidance

Do not block the event loop while waiting for work that itself needs the event
loop to make progress. For bounded clients, send manual ACKs from another task
or make sure the event loop continues polling.

## MQTT 5 Reason Codes

MQTT 5 manual ACKs can include reason codes and properties. Use these only when
the broker and downstream consumers expect them; many deployments only need the
default success ACK.
