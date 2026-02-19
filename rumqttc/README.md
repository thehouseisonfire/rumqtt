# rumqttc-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-next.svg)](https://crates.io/crates/rumqttc-next)
[![docs.rs page](https://docs.rs/rumqttc-next/badge.svg)](https://docs.rs/rumqttc-next)

A pure rust MQTT client which strives to be robust, efficient and easy to use. This library is backed by an async(using tokio) eventloop which enables users to send and receive MQTT messages in correspondence with a broker.

## Examples

A simple synchronous publish and subscribe
----------------------------

```rust
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

```rust
use rumqttc::{MqttOptions, AsyncClient, QoS};
use tokio::{task, time};
use std::time::Duration;
use std::error::Error;

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
- **Secure WebSockets** (`wss://`): Only support native-tls `TlsConfiguration` variants (`Native`, `NativeConnector`, `SimpleNative`)

This is because `async-tungstenite` uses native-tls for websockets when both TLS features are enabled. Attempting to use a rustls `TlsConfiguration` variant with secure websockets will return `tls::Error::TlsBackendConflict`.

**Recommendation**: If you need secure websockets with rustls, disable the `use-native-tls` feature. If you need both TLS backends, use native-tls `TlsConfiguration` variants for secure websocket connections.
