# rumqttc-v4-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-v4-next.svg)](https://crates.io/crates/rumqttc-v4-next)
[![docs.rs page](https://docs.rs/rumqttc-v4-next/badge.svg)](https://docs.rs/rumqttc-v4-next)

`rumqttc-v4-next` is the explicit MQTT 3.1.1 client crate in the rumqtt family.
Use it when you need MQTT 3.1.1 specifically. If you want the default MQTT 5 client, use `rumqttc-next` or `rumqttc-v5-next` instead.

This crate keeps the library target name as `rumqttc` so application code can stay familiar after migrating package names.

Existing upstream `rumqttc` users should see the workspace [migration guide](../MIGRATION.md) for package names,
API changes, and porting recipes.

## Installation

```bash
cargo add rumqttc-v4-next
```

## Typed Topics

For applications that want typed topic declarations, generated publish/subscribe
helpers, and payload serialization on top of this client, use the companion
[`mqtt-typed-client-next`](https://github.com/thehouseisonfire/mqtt-typed-client-next)
crate with its `rumqttc-v4` feature enabled. See the `rumqttc_v4_typed_client`
example in that repo for a runnable integration sample.

## Examples

A simple synchronous publish and subscribe
----------------------------

```rust,no_run
use rumqttc::{MqttOptions, Client, PublishOptions, QoS};
use std::time::Duration;
use std::thread;

let mut mqttoptions = MqttOptions::new("rumqtt-sync", "test.mosquitto.org");
mqttoptions.set_keep_alive(5);

let (mut client, mut connection) = Client::builder(mqttoptions).capacity(10).build();
client.subscribe("hello/rumqtt", QoS::AtMostOnce).unwrap();
thread::spawn(move || for i in 0..10 {
   client.publish("hello/rumqtt", vec![i; i as usize], PublishOptions::new(QoS::AtLeastOnce)).unwrap();
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
use rumqttc::{MqttOptions, AsyncClient, PublishOptions, QoS};
use tokio::{task, time};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
let mut mqttoptions = MqttOptions::new("rumqtt-async", "test.mosquitto.org");
mqttoptions.set_keep_alive(5);

let (mut client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();
client.subscribe("hello/rumqtt", QoS::AtMostOnce).await.unwrap();

task::spawn(async move {
    for i in 0..10 {
        client.publish("hello/rumqtt", vec![i; i as usize], PublishOptions::new(QoS::AtLeastOnce)).await.unwrap();
        time::sleep(Duration::from_millis(100)).await;
    }
});

while let Ok(notification) = eventloop.poll().await {
    println!("Received = {:?}", notification);
}
}
```

Tracked protocol results
------------------------------

Tracked APIs return notices that resolve with MQTT protocol responses:
`publish_tracked(...).wait_async()` returns `PublishResult`,
`subscribe_tracked(...).wait_async()` returns `SubAck`, and
`unsubscribe_tracked(...).wait_async()` returns `UnsubAck`. Use
`wait_completion_async()` when only completion/failure status is needed.

See `examples/tracked_notices.rs` for a runnable async example.

Resubscribing after reconnect
------------------------------

The event loop yields `Event::Incoming(Packet::ConnAck(_))` after every
successful connection, including automatic reconnects. If `session_present` is
false, the broker has a fresh session and no longer has the client's
subscriptions. Keep the desired subscription set in the application and reissue
it on those `CONNACK`s.

See `examples/resubscribe_on_reconnect.rs` for a runnable async example.

Quick overview of features
- Eventloop orchestrates outgoing/incoming packets concurrently and handles the state
- Pings the broker when necessary and detects client side half open connections as well
- Throttling of outgoing packets (todo)
- Queue size based flow control on outgoing packets
- Automatic reconnections by just continuing the `eventloop.poll()/connection.iter()` loop
- Natural backpressure to client APIs during bad network
- Support for `WebSockets`
- Secure transport using TLS
- Unix domain sockets on Unix targets
- Strict MQTT 3.1.1 packet validation on the codec path

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

- Bounded clients apply backpressure through the client request channel. If the
  same task that drives `eventloop.poll()` awaits `publish()`, `subscribe()`,
  `unsubscribe()`, `ack()`, or another request-sending API while that bounded
  channel is full, it can self-block: the request is waiting for the event loop
  to read from the channel, but the event loop cannot make progress until that
  task polls it again. For bounded async clients, prefer driving
  `eventloop.poll()` in a dedicated task and dispatch application work to other
  tasks. When dropping outgoing publishes under overload is intended, use
  `try_publish()` and treat a full-channel error as the drop signal.

- Use `client.disconnect()`/`try_disconnect()` for MQTT-level graceful shutdown.
  The event loop first flushes previously accepted `QoS` 0 publishes and drains
  previously accepted `QoS` 1/ `QoS` 2 publish and tracked subscribe/unsubscribe
  state (`SUBACK`/`UNSUBACK`), then sends terminal DISCONNECT. Use
  `disconnect_with_timeout()` to bound this drain; if the timeout expires first,
  polling returns `ConnectionError::DisconnectTimeout` and DISCONNECT is not
  sent. Use `disconnect_now()` to send DISCONNECT immediately and abandon
  unresolved in-flight work. Dropping or aborting the transport closes locally
  without sending DISCONNECT and may trigger Will publication.

- Disconnect requests use the normal client request channel. If the event loop
  is not currently reading new requests because the outbound `QoS` 1/ `QoS` 2
  publish inflight window is full or a packet-id collision is pending, a queued
  `disconnect_with_timeout()` timeout starts only after the event loop observes
  the disconnect request, and `disconnect_now()` is not an out-of-band transport
  abort.

- For restart-safe MQTT 3.1.1 persistent sessions, configure
  `MqttOptions::set_session_store(...)` or builder `.session_store(...)`
  together with `clean_session(false)`. rumqttc provides the backend-neutral
  `SessionStore` trait and `PersistedSession` data model; the application owns
  serialization and durable storage. The store is saved before tracked
  publish/subscribe/unsubscribe notices report completion when losing the saved
  state would make recovery ambiguous. A save failure is reported through the
  corresponding `SessionPersistence(...)` notice error and
  `ConnectionError::SessionStore`.

- MQTT 3.1.1 `CleanSession = 0` expects both client and server to keep session
  state for reconnect. Without a `SessionStore`, rumqttc can still resume within
  the same live `EventLoop`, but a new process cannot strictly continue a
  broker-retained session unless it restores the local client session state.
  See `examples/persistent_session_file_store.rs` for a complete file-backed
  `SessionStore` implementation owned by application code.

- This crate is intentionally protocol-specific. It does not expose MQTT 5
  features such as AUTH packets, topic aliases, or MQTT 5 property handling.

- On Unix targets, local broker sockets are supported via
  `MqttOptions::new(..., Broker::unix(...))`. When the `url` feature is enabled,
  `MqttOptions::parse_url("unix:///tmp/mqtt.sock?client_id=...")` is also
  supported.

## TLS Support

rumqttc supports two TLS backends:

- **`use-rustls`** (default): Uses [rustls](https://github.com/rustls/rustls) with `aws-lc` as the crypto provider and native platform certificates
- **`use-native-tls`**: Uses the platform's native TLS implementation (Secure Transport on macOS, `SChannel` on Windows, OpenSSL on Linux)

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

- Configure TLS explicitly with `MqttOptions::set_transport(Transport::tls_with_config(...))`
- Configure secure websockets explicitly with `Broker::websocket("ws://...")` plus `MqttOptions::set_transport(Transport::wss_with_config(...))`

Secure websocket connections upgrade the TCP stream using the selected `TlsConfiguration` before the websocket handshake, so backend selection follows the provided TLS configuration.

In dual-backend dependency graphs, avoid relying on `TlsConfiguration::default()`, because default backend selection must be explicit.
Use `TlsConfiguration::default_rustls()` or `TlsConfiguration::default_native()` (when available) or pass an explicit configuration to
`Transport::tls_with_config(...)` / `Transport::wss_with_config(...)`.

Native-tls WSS can use platform roots via `TlsConfiguration::default_native()` or a custom CA / identity via
`TlsConfiguration::simple_native(...)`.
