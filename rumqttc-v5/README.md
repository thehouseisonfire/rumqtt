# rumqttc-v5-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-v5-next.svg)](https://crates.io/crates/rumqttc-v5-next)
[![docs.rs page](https://docs.rs/rumqttc-v5-next/badge.svg)](https://docs.rs/rumqttc-v5-next)

`rumqttc-v5-next` is the explicit MQTT 5 client crate in the rumqtt family.
The `rumqttc-next` package is a facade that re-exports this crate unchanged.

This crate keeps the library target name as `rumqttc` so application code can stay familiar after migrating package names.

## Installation

Use the facade package when you want the default MQTT 5 client:

```bash
cargo add rumqttc-next
```

Use this package directly when you want the protocol-scoped crate name:

```bash
cargo add rumqttc-v5-next
```

## Examples

A simple synchronous publish and subscribe
----------------------------

```rust,no_run
use rumqttc::{MqttOptions, Client, QoS};
use std::time::Duration;
use std::thread;

let mut mqttoptions = MqttOptions::new("rumqtt-sync", "test.mosquitto.org", 1883);
mqttoptions.set_keep_alive(5);

let (mut client, mut connection) = Client::new(mqttoptions, 10);
client.subscribe("hello/rumqtt", QoS::AtMostOnce).unwrap();
thread::spawn(move || for i in 0..10 {
   client.publish("hello/rumqtt", QoS::AtLeastOnce, false, vec![i; i as usize]).unwrap();
   thread::sleep(Duration::from_millis(100));
});

// Iterate to poll the eventloop for connection progress
for (i, notification) in connection.iter().enumerate() {
    println!("Notification = {:?}", notification);
}
```

A simple asynchronous publish and subscribe
------------------------------

```rust,no_run
use rumqttc::{MqttOptions, AsyncClient, QoS};
use tokio::{task, time};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
let mut mqttoptions = MqttOptions::new("rumqtt-async", "test.mosquitto.org", 1883);
mqttoptions.set_keep_alive(5);

let (mut client, mut eventloop) = AsyncClient::new(mqttoptions, 10);
client.subscribe("hello/rumqtt", QoS::AtMostOnce).await.unwrap();

task::spawn(async move {
    for i in 0..10 {
        client.publish("hello/rumqtt", QoS::AtLeastOnce, false, vec![i; i as usize]).await.unwrap();
        time::sleep(Duration::from_millis(100)).await;
    }
});

while let Ok(notification) = eventloop.poll().await {
    println!("Received = {:?}", notification);
}
}
```

Quick overview of features
- Eventloop orchestrates outgoing/incoming packets concurrently and handles the state
- Pings the broker when necessary and detects client side half open connections as well
- Throttling of outgoing packets (todo)
- Queue size based flow control on outgoing packets
- Automatic reconnections by just continuing the `eventloop.poll()/connection.iter()` loop
- Natural backpressure to client APIs during bad network
- Support for WebSockets
- Secure transport using TLS
- MQTT 5 properties, reason codes, session controls, and topic aliases
- Optional SCRAM-based enhanced authentication support via `auth-scram`

In short, everything necessary to maintain a robust connection

Since the eventloop is externally polled (with `iter()/poll()` in a loop)
out side the library and `Eventloop` is accessible, users can
- Distribute incoming messages based on topics
- Stop it when required
- Access internal state for use cases like graceful shutdown or to modify options before reconnection

### Important notes

- Looping on `connection.iter()`/`eventloop.poll()` is necessary to run the
  event loop and make progress. It yields incoming and outgoing activity
  notifications which allows customization as you see fit.

- Blocking inside the `connection.iter()`/`eventloop.poll()` loop will block
  connection progress.

- Use `client.disconnect()`/`try_disconnect()` for MQTT-level graceful shutdown
  (sends DISCONNECT). Dropping all client handles ends polling with
  `ConnectionError::RequestsDone` and closes locally without sending DISCONNECT.

- This crate is MQTT 5-specific. If you need MQTT 3.1.1 instead, use
  `rumqttc-v4-next`.

## TLS Support

rumqttc supports two TLS backends:

- **`use-rustls`** (default): Uses [rustls](https://github.com/rustls/rustls) with `aws-lc` as the crypto provider and native platform certificates
- **`use-native-tls`**: Uses the platform's native TLS implementation (Secure Transport on macOS, SChannel on Windows, OpenSSL on Linux)

### TLS Feature Flags

| Feature | Description |
|---------|-------------|
| `use-rustls` | Enable rustls with `aws-lc` provider (recommended default) |
| `use-rustls-no-provider` | Enable rustls without selecting a crypto provider |
| `use-rustls-aws-lc` | Enable rustls with `aws-lc` provider |
| `use-rustls-ring` | Enable rustls with `ring` provider |
| `use-native-tls` | Enable native-tls backend |

`use-rustls-aws-lc` and `use-rustls-ring` are mutually exclusive. Enabling both results in a compile error.

### Important: Using Both TLS Features

When both `use-rustls-no-provider` and `use-native-tls` features are enabled:

- **Direct TLS connections** (`mqtts://`, `ssl://`): Work with both rustls and native-tls `TlsConfiguration` variants
- **Secure WebSockets** (`wss://`): Also work with both rustls and native-tls `TlsConfiguration` variants

Secure websocket connections now upgrade the TCP stream using the selected `TlsConfiguration` before the websocket handshake, so backend selection follows the provided TLS configuration.

In dual-backend dependency graphs, avoid relying on `TlsConfiguration::default()`, because default backend selection must be explicit.
Use `TlsConfiguration::default_rustls()` or `TlsConfiguration::default_native()` (when available) or pass an explicit configuration to
`Transport::tls_with_config(...)` / `Transport::wss_with_config(...)`.
