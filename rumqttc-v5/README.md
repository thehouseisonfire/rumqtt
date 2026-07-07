# rumqttc-v5-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-v5-next.svg)](https://crates.io/crates/rumqttc-v5-next)
[![docs.rs page](https://docs.rs/rumqttc-v5-next/badge.svg)](https://docs.rs/rumqttc-v5-next)

`rumqttc-v5-next` is the explicit MQTT 5 client crate in the rumqtt family.
The `rumqttc-next` package is a facade that re-exports this crate unchanged.

This crate keeps the library target name as `rumqttc` so application code can stay familiar after migrating package names.

Existing upstream `rumqttc` users should see the workspace [migration guide](../MIGRATION.md) for package names,
API changes, and porting recipes.

## Installation

Use the facade package when you want the default MQTT 5 client:

```bash
cargo add rumqttc-next
```

Use this package directly when you want the protocol-scoped crate name:

```bash
cargo add rumqttc-v5-next
```

## Typed Topics

For applications that want typed topic declarations, generated publish/subscribe
helpers, payload serialization, and MQTT 5 property pass-through on top of this
client, use the companion [`mqtt-typed-client-next`](https://github.com/thehouseisonfire/mqtt-typed-client-next)
crate with its `rumqttc-v5` feature enabled. See the `rumqttc_v5_connect_properties`
and `rumqttc_v5_publish_properties` examples in that repo for runnable integration
samples.

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
`unsubscribe_tracked(...).wait_async()` returns `UnsubAck`. MQTT 5 broker ACKs
are returned as `Ok(...)` even when their reason code reports failure or
rejection, so applications can inspect reason codes and properties. Use
`wait_completion_async()` when only completion/failure status is needed.

See `examples/tracked_notices_v5.rs` for a runnable async example.

Resubscribing after reconnect
------------------------------

The event loop yields `Event::Incoming(Packet::ConnAck(_))` after every
successful connection, including automatic reconnects. If `session_present` is
false, the broker has a fresh session and no longer has the client's
subscriptions. Keep the desired subscription set in the application and reissue
it on those `CONNACK`s.

See `examples/resubscribe_on_reconnect_v5.rs` for a runnable async example.

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

- With `clean_start(false)`, rumqttc keeps MQTT session state in memory across
  automatic reconnects made by the same `EventLoop`. This lets a transient
  network reconnect resume in-flight `QoS` 1/2 packets and pending operations when
  the broker returns `Session Present = 1`. A new process or newly constructed
  `EventLoop` does not have that local state; in strict MQTT 5 mode, rumqttc
  rejects `Session Present = 1` in that case with
  `ConnectionError::SessionStateMismatch`.

- To resume local MQTT session state across newly constructed clients or process
  restarts, configure `MqttOptions::set_session_store(...)` or builder
  `.session_store(...)` together with `clean_start(false)`. rumqttc provides the
  backend-neutral `SessionStore` trait and `PersistedSession` data model; the
  application supplies the storage backend and serialization format. No file or
  database adapter is built into the client crate. Store implementations should
  make `save(...)` crash-consistent. Tracked publish, subscribe, and unsubscribe
  notices are completed only after the updated session checkpoint is saved; a
  save failure is reported through the corresponding `SessionPersistence(...)`
  notice error. rumqttc clears the configured store when the broker starts a
  fresh session (`Session Present = 0`), when local session state is explicitly
  reset, or when the effective session expiry is zero at disconnect. See
  `examples/persistent_session_file_store_v5.rs` for a complete file-backed
  `SessionStore` implementation owned by application code.

- For strict MQTT 5 restart-safe session resume, configure a `SessionStore`.
  MQTT-3.2.2-4 requires a client that lacks Session State to close a connection
  that returns `Session Present = 1`; MQTT-3.1.2-23 requires Client and Server
  Session State to be stored after a connection closes when the Session Expiry
  Interval is greater than zero. Without a `SessionStore`, rumqttc can still be
  strict by rejecting broker-only resume after a new process or newly constructed
  `EventLoop`, but it cannot strictly continue that resumed session because the
  local client state was not restored.

- Applications that intentionally rely on broker-retained messages after local
  state loss can opt into non-strict MQTT 5 compatibility mode with
  `MqttOptions::set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly)`
  or `MqttOptions::builder(...).broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly)`.
  This accepts broker-only session reuse for a stable `ClientID`, but the client
  cannot reconcile `QoS` 1/2 packets or other in-flight operations that were lost
  with the previous process or `EventLoop`.

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

- This crate is MQTT 5-specific. If you need MQTT 3.1.1 instead, use
  `rumqttc-v4-next`.

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
