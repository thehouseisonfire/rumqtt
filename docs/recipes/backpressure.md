# Backpressure Recipes

Client builders use a bounded request channel when `.capacity(n)` is set. This
is the normal production mode because it limits memory growth and applies
backpressure to publishers.

## Dedicated Event Loop Task

Drive `eventloop.poll()` in its own task. Send publishes, subscribes, and ACKs
from other tasks using cloned clients. The polling task must remain independent:
moving `publish()` to another task does not help if the polling task then awaits
that task or blocks while handing work to it.

```rust,no_run
use rumqttc::{AsyncClient, MqttOptions};

# async fn run() {
let options = MqttOptions::new("client-id", ("localhost", 1883));
let (client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();

tokio::spawn(async move {
    while let Ok(event) = eventloop.poll().await {
        println!("{event:?}");
    }
});

tokio::spawn(async move {
    // Request-producing application work is independent of event-loop polling.
    let _ = client;
});
# }
```

Do not await a request-sending API from the same task that must poll the event
loop when the bounded channel might be full. That can self-block because the
event loop is the consumer that frees channel capacity.

## Dropping Under Overload

Use `try_publish()` when dropping data is the intended overload policy. Treat a
full-channel error as the drop signal and record application metrics outside
rumqttc.

For a forwarding application, keep both overload decisions explicit. A bounded
application queue prevents an event-loop task from creating an unbounded work
backlog, and `try_send` keeps that task pollable. The publisher can then await
rumqttc channel capacity without owning event-loop progress:

```rust,no_run
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, PublishOptions, QoS};
use tokio::sync::mpsc;

# async fn run() {
let options = MqttOptions::new("forwarder", ("localhost", 1883));
let (client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();
let (work_tx, mut work_rx) = mpsc::channel::<(String, Vec<u8>)>(100);

tokio::spawn(async move {
    while let Ok(event) = eventloop.poll().await {
        let Event::Incoming(Packet::Publish(publish)) = event else {
            continue;
        };
        let work = (
            String::from_utf8_lossy(publish.topic.as_ref()).into_owned(),
            publish.payload.to_vec(),
        );

        if work_tx.try_send(work).is_err() {
            // Record the deliberate application-level drop.
        }
    }
});

tokio::spawn(async move {
    while let Some((topic, payload)) = work_rx.recv().await {
        if client
            .publish(topic, payload, PublishOptions::new(QoS::AtMostOnce))
            .await
            .is_err()
        {
            break;
        }
    }
});
# }
```

Awaiting `work_tx.send(...)` in the polling task would create the same kind of
progress dependency at the application queue. If forwarding must be lossless,
use an independently driven durable queue or another bounded design whose
consumer cannot depend on the event-loop task.

## Capacity Choice

Start with a bounded capacity sized for short bursts, not sustained backlog. If
the queue stays full, fix the broker throughput, network, QoS/in-flight settings,
or application publish rate rather than only increasing capacity.

Request-channel capacity bounds channel admission, not all event-loop memory.
Decoded events, protocol state, replay work, and requests already admitted to
the internal scheduler are separate. Configuring the network read batch no
larger than request capacity can reduce the chance of a clean one-request-per-
event forwarding burst filling the channel, but it is not a progress guarantee:
other producers, zero-capacity channels, in-flight limits, and network stalls
can still fill or block admission.

See the
[event-loop/request-channel deadlock investigation](../event-loop-request-channel-deadlock-investigation-2026-07-27.md)
for the verified scope and design tradeoffs.
