# Backpressure Recipes

Client builders use a bounded request channel when `.capacity(n)` is set. This
is the normal production mode because it limits memory growth and applies
backpressure to publishers.

## Dedicated Event Loop Task

Drive `eventloop.poll()` in its own task. Send publishes, subscribes, and ACKs
from other tasks using cloned clients.

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

let publisher = client.clone();
tokio::spawn(async move {
    let _ = publisher;
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

## Capacity Choice

Start with a bounded capacity sized for short bursts, not sustained backlog. If
the queue stays full, fix the broker throughput, network, QoS/in-flight settings,
or application publish rate rather than only increasing capacity.
