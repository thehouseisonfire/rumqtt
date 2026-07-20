# rumqttc-v5-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-v5-next.svg)](https://crates.io/crates/rumqttc-v5-next)
[![docs.rs page](https://docs.rs/rumqttc-v5-next/badge.svg)](https://docs.rs/rumqttc-v5-next)

`rumqttc-v5-next` is the explicit MQTT 5 client crate in the rumqtt family.
The `rumqttc-next` package is a facade that re-exports this crate unchanged.

This crate keeps the library target name as `rumqttc` so application code can stay familiar after migrating package names.

Existing upstream `rumqttc` users should see the workspace [migration guide](https://github.com/thehouseisonfire/rumqtt/blob/main/MIGRATION.md) for package names,
API changes, and porting recipes.

For production deployment patterns covering TLS, `WebSockets`, proxies,
persistent sessions, reconnect handling, bounded channels, manual ACKs, and
common brokers, see the workspace [recipe guide](https://github.com/thehouseisonfire/rumqtt/blob/main/docs/recipes/README.md).

## Installation

Use the facade package when you want the default MQTT 5 client:

```bash
cargo add rumqttc-next@0.34.0-alpha
```

Use this package directly when you want the protocol-scoped crate name:

```bash
cargo add rumqttc-v5-next@0.34.0-alpha
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

Reusable publish options
------------------------

Name options when the same publish policy is used for multiple messages. The
constructor is non-retained by default, `retained()` is the readable constant
case, and `retain(bool)` supports a runtime choice. MQTT 5 properties can be
added to the same builder:

```rust,ignore
let telemetry = PublishOptions::new(QoS::AtLeastOnce);
let state = PublishOptions::new(QoS::AtLeastOnce).retained();
let dynamic = PublishOptions::new(QoS::AtLeastOnce).retain(is_retained);

client.publish("telemetry/temp", temp, telemetry).await?;
client.publish("state/online", "true", state).await?;
client.publish("dynamic/topic", payload, dynamic).await?;
```

Async stream adapter
------------------------------

Enable the `stream` feature to drive the event loop through
`futures_core::Stream` while keeping the same externally-polled behavior.
This is an adapter for APIs and codebases that work with streams; direct
`eventloop.poll()` calls can still be used with `tokio::select!`.
Stream errors other than `ConnectionError::RequestsDone` are non-terminal;
continue polling after an error to allow reconnects. Use fail-fast
`TryStreamExt` combinators only when stopping on the first connection error is
intended.

```rust,ignore
use futures_util::{StreamExt, pin_mut};
use rumqttc::{AsyncClient, ClientError, Event, MqttOptions, Outgoing, QoS};

#[tokio::main(flavor = "current_thread")]
async fn main() {
let mqttoptions = MqttOptions::new("rumqtt-stream", "test.mosquitto.org");
let (client, eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

client.subscribe("hello/rumqtt", QoS::AtMostOnce).await.unwrap();

let stream = eventloop.into_stream();
pin_mut!(stream);
let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
pin_mut!(shutdown_rx);
let mut shutdown_requested = false;
let mut disconnecting = false;

loop {
    if shutdown_requested && !disconnecting {
        match client.try_disconnect() {
            Ok(()) => disconnecting = true,
            Err(ClientError::RequestChannelFull(_)) => {}
            Err(error) => panic!("failed to request disconnect: {error}"),
        }
    }

    tokio::select! {
        event = stream.next() => match event {
            Some(Ok(Event::Outgoing(Outgoing::Disconnect))) => break,
            Some(Ok(notification)) => println!("Received = {:?}", notification),
            Some(Err(error)) => eprintln!("Event loop error = {error}"),
            None => break,
        },
        _ = &mut shutdown_rx, if !shutdown_requested => {
            shutdown_requested = true;
        }
    }
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

Structured lifecycle tracing
----------------------------

Enable the `tracing` feature to emit low-frequency structured lifecycle events
under the `rumqttc::lifecycle` target. The crate emits events for connection
attempts, failed attempts, established and lost connections, restored sessions,
prepared replay state, and typed MQTT protocol violations. It does not install a
subscriber; applications remain responsible for choosing and configuring one.

```toml
[dependencies]
rumqttc-v5-next = { version = "0.34.0-alpha", features = ["tracing"] }
```

Enable `tracing-log-compat` instead when tracing events should fall back to
`log::Record`s if no tracing subscriber is active. This enables `tracing/log` on
Cargo's unified dependency graph, and fallback fields are textual rather than
typed. Existing applications that only need the current `log` statements do not
need either feature.

The event schema uses stable primitive classifications such as `protocol`,
`attempt_id`, `connection_generation`, `attempt_in_generation`, `reconnect`,
`phase`, `error_kind`, and `violation_kind`. Human-readable errors are included
separately. Client IDs, broker URLs, topics, payloads, credentials, and user
properties are not recorded. A cancelled `EventLoop::poll()` may leave a
`mqtt.connection_attempt` without a matching terminal event; the next attempt
receives a new `attempt_id`.

Quick overview of features
- Eventloop orchestrates outgoing/incoming packets concurrently and handles the state
- Pings the broker when necessary and detects client side half open connections as well
- Protocol-aware outgoing flow control with configurable in-flight limits
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
outside the library and `EventLoop` is accessible, users can
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
  backend-neutral `SessionStore` trait, scoped `SessionStoreKey`,
  `PersistedSession` data model, and canonical `PersistedSession::encode`/
  `decode` helpers; the application supplies the storage backend and layout. No
  file or database adapter is built into the client crate. Configure
  `set_session_store_scope(...)` when one store is shared across brokers,
  tenants, environments, or connection profiles. Store implementations should
  make `save(...)` and `clear(...)` atomic. Tracked publish, subscribe, and unsubscribe
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

- Exactly one active `EventLoop` may own and modify a session-store key at a
  time. `SessionStore` does not provide leases, fencing, compare-and-swap, or
  active/passive failover coordination. A later load must never observe a torn
  checkpoint, even if cancellation leaves `save` or `clear` completion status
  indeterminate.

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
  Once the event loop processes the barrier from the control-request lane, it
  flushes `QoS` 0 publishes already admitted to protocol processing and drains
  `QoS` 1/ `QoS` 2 publish and tracked subscribe/unsubscribe state already in
  progress (`SUBACK`/`UNSUBACK`), then sends terminal DISCONNECT. Queued but
  unsent flow-controlled publishes are not part of that drain. Use
  `disconnect_with_timeout()` to bound this drain; if the timeout expires first,
  polling returns `ConnectionError::DisconnectTimeout` and DISCONNECT is not
  sent. Use `disconnect_now()` to prioritize DISCONNECT without draining
  unresolved in-flight work. This priority is observed at event-loop scheduling
  points; it does not interrupt connection setup, a write/flush already in
  progress, buffered events, or blocked event-loop polling. Dropping or aborting
  the transport closes locally without sending DISCONNECT and may trigger Will
  publication.

- Builder-created clients route graceful disconnect through the bounded
  control-request lane, so it may pass an earlier flow-controlled publish that
  is not currently sendable. The drain timeout starts only after the event loop
  processes the barrier, not when the client queues it. A successful client call
  confirms channel admission only; cloned clients can still enqueue work before
  they observe terminal shutdown, and that work will not run after the barrier.
  `disconnect_now()` uses a separate priority lane but is not an out-of-band
  transport abort.

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
- Prefer `MqttOptions::websocket_with_tls_config("client-id", "wss://...", tls_config)` for secure websockets
- For lower-level overrides, `Broker::websocket("ws://...")` plus `MqttOptions::set_transport(Transport::wss_with_config(...))` remains supported

Secure websocket connections upgrade the TCP stream using the selected `TlsConfiguration` before the websocket handshake, so backend selection follows the provided TLS configuration.

In dual-backend dependency graphs, avoid relying on `TlsConfiguration::default()`, because default backend selection must be explicit.
Use `TlsConfiguration::default_rustls()` or `TlsConfiguration::default_native()` (when available) or pass an explicit configuration to
`Transport::tls_with_config(...)` / `Transport::wss_with_config(...)`.

Native-tls WSS can use platform roots via `TlsConfiguration::default_native()` or a custom CA / identity via
`TlsConfiguration::simple_native(...)`.
