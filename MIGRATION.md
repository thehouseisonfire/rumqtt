# Migrating from upstream rumqttc

This guide is for applications that already use upstream `rumqttc` and want to
port to this fork. It focuses on intentional public API differences that affect
normal client code.

This is not a compatibility promise for every upstream release. The examples
below were checked against this repository's public API. Old snippets are shown
only to identify common upstream patterns; use the replacement snippets as the
compile-checked fork style.

## Packages and MQTT versions

The published package names are suffixed because the unsuffixed names are not
owned by this fork:

```toml
# MQTT 5 facade; re-exports rumqttc-v5-next
rumqttc = { package = "rumqttc-next", version = "0.34.0-alpha" }

# MQTT 5 explicit package
rumqttc = { package = "rumqttc-v5-next", version = "0.34.0-alpha" }

# MQTT 3.1.1 explicit package
rumqttc = { package = "rumqttc-v4-next", version = "0.34.0-alpha" }
```

Each package's library target is still named `rumqttc`, so Rust imports remain
familiar:

```rust
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, QoS};
```

Use `rumqttc-v4-next` for MQTT 3.1.1. Use `rumqttc-v5-next` or the
`rumqttc-next` facade for MQTT 5. MQTT 5-only APIs such as properties, enhanced
auth, topic aliases, and broker session resume policies are not available in the
v4 crate.

## Client construction

Upstream code commonly constructs clients directly:

```rust,ignore
let (client, eventloop) = AsyncClient::new(options, 10);
let (client, connection) = Client::new(options, 10);
```

This fork uses builders. `build_async()` was removed; use `build()` for both
sync and async builders.

```rust
use rumqttc::{AsyncClient, Client, MqttOptions};

let options = MqttOptions::new("client-id", "localhost");
let (_client, _eventloop) = AsyncClient::builder(options).capacity(10).build();

let options = MqttOptions::new("client-id-sync", "localhost");
let (_client, _connection) = Client::builder(options).capacity(10).build();
```

If an unbounded request channel is intentional, use `.unbounded()` explicitly.

## Publishing

Publish methods now take the payload before a `PublishOptions` value. The
separate byte/property variants were folded into one API.

Old upstream style:

```rust,ignore
client.publish("devices/1/status", QoS::AtLeastOnce, false, "online").await?;
client.publish_bytes("devices/1/status", QoS::AtLeastOnce, true, bytes).await?;
client.publish_with_properties(topic, QoS::AtLeastOnce, false, payload, properties).await?;
```

New fork style:

```rust
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, QoS};

async fn publish_examples() -> Result<(), rumqttc::ClientError> {
    let options = MqttOptions::new("publisher", "localhost");
    let (client, _eventloop) = AsyncClient::builder(options).capacity(10).build();

    client
        .publish(
            "devices/1/status",
            "online",
            PublishOptions::new(QoS::AtLeastOnce),
        )
        .await?;

    client
        .publish(
            "devices/1/status",
            vec![1, 2, 3],
            PublishOptions::exactly_once().retained(),
        )
        .await?;

    Ok(())
}
```

`PublishOptions` has convenience constructors:

```rust
use rumqttc::{PublishOptions, QoS};

let qos0 = PublishOptions::at_most_once();
let qos1 = PublishOptions::at_least_once();
let qos2_retained = PublishOptions::new(QoS::ExactlyOnce).retained();
```

Options can be named and reused as publish policies. Use `retained()` when the
policy is always retained and `retain(bool)` when it is selected dynamically:

```rust,ignore
let telemetry = PublishOptions::new(QoS::AtLeastOnce);
let state = PublishOptions::new(QoS::AtLeastOnce).retained();
let dynamic = PublishOptions::new(QoS::AtLeastOnce).retain(is_retained);

client.publish("telemetry/temp", temp, telemetry).await?;
client.publish("state/online", "true", state).await?;
client.publish("dynamic/topic", payload, dynamic).await?;
```

Accepted payloads include `&str`, `String`, `&[u8]`, `[u8; N]`, `Vec<u8>`, and
`bytes::Bytes`.

## MQTT 5 publish properties

In the MQTT 5 crate, publish properties are attached to `PublishOptions`.

```rust
use bytes::Bytes;
use rumqttc::mqttbytes::v5::PublishProperties;
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, QoS};

async fn publish_with_properties() -> Result<(), rumqttc::ClientError> {
    let options = MqttOptions::new("v5-publisher", "localhost");
    let (client, _eventloop) = AsyncClient::builder(options).capacity(10).build();

    let properties = PublishProperties {
        payload_format_indicator: Some(1),
        content_type: Some("application/json".into()),
        correlation_data: Some(Bytes::from_static(b"request-7")),
        user_properties: vec![("source".into(), "migration-guide".into())],
        ..Default::default()
    };

    client
        .publish(
            "devices/1/status",
            br#"{"online":true}"#,
            PublishOptions::at_least_once().properties(properties),
        )
        .await?;

    Ok(())
}
```

## Authentication

Authentication is represented by version-specific `ConnectAuth` enums rather
than an `Option<Login>`-style accessor. Empty strings or empty byte buffers are
not used to infer field presence.

Username/password:

```rust
use rumqttc::{ConnectAuth, MqttOptions};

let mut options = MqttOptions::new("auth-client", "localhost");
options.set_credentials("user", "password");

assert!(matches!(
    options.auth(),
    ConnectAuth::UsernamePassword { username, .. } if username == "user"
));
```

Username only:

```rust
use rumqttc::{ConnectAuth, MqttOptions};

let mut options = MqttOptions::new("auth-client", "localhost");
options.set_username("user");

assert_eq!(
    options.auth(),
    &ConnectAuth::Username {
        username: "user".into(),
    }
);
```

MQTT 5 also allows password-only CONNECT authentication:

```rust
use rumqttc::{ConnectAuth, MqttOptions};

let mut options = MqttOptions::new("auth-client", "localhost");
options.set_password("token");

assert!(matches!(options.auth(), ConnectAuth::Password { .. }));
```

Use `set_auth(...)` when constructing the enum directly and `clear_auth()` to
remove CONNECT auth fields.

## TLS and secure transports

Secure URL schemes are not implicit transport selectors in this fork.
`mqtts://`, `ssl://`, and `wss://` URLs are rejected by URL parsing. Configure
the broker address and transport separately.

TLS over TCP:

```rust,ignore
use rumqttc::{MqttOptions, TlsConfiguration, Transport};

let mut options = MqttOptions::new("tls-client", ("broker.example.com", 8883));
options.set_transport(Transport::tls_with_config(
    TlsConfiguration::default_rustls(),
));
```

When native TLS is enabled:

```rust,ignore
use rumqttc::{MqttOptions, TlsConfiguration, Transport};

let mut options = MqttOptions::new("tls-client", ("broker.example.com", 8883));
options.set_transport(Transport::tls_with_config(
    TlsConfiguration::default_native(),
));
```

If both rustls and native-tls are enabled in a dependency graph, choose the
backend explicitly with `TlsConfiguration::default_rustls()`,
`TlsConfiguration::default_native()`, or a concrete custom configuration.

## WebSocket and WSS

Plain WebSocket uses a websocket broker URL:

```rust,ignore
use rumqttc::{Broker, MqttOptions, Transport};

let mut options = MqttOptions::new(
    "ws-client",
    Broker::websocket("ws://broker.example.com:8080/mqtt")?,
);
options.set_transport(Transport::ws());
```

Secure WebSocket still uses `Broker::websocket("ws://...")`; TLS is selected by
the transport:

```rust,ignore
use rumqttc::{Broker, MqttOptions, TlsConfiguration, Transport};

let mut options = MqttOptions::new(
    "wss-client",
    Broker::websocket("ws://broker.example.com:443/mqtt")?,
);
options.set_transport(Transport::wss_with_config(
    TlsConfiguration::default_rustls(),
));
```

`Broker::websocket("wss://...")` returns an error so callers do not silently get
an unintended TLS backend.

## Proxy and custom sockets

Enable `http-proxy` or `socks-proxy` for only the required protocol, or `proxy`
as a compatibility umbrella that enables both. Then configure the proxy on
`MqttOptions`:

The previously stable `proxy` feature enabled only HTTP(S) proxying. It now also
enables SOCKS5 proxying and therefore pulls in the dependencies required by
both implementations. Existing users that only need HTTP(S) proxy support
should replace `proxy` with `http-proxy` in their Cargo features to retain the
previous dependency scope. Use `socks-proxy` when only SOCKS5 support is needed.

```rust,ignore
use rumqttc::{MqttOptions, Proxy};

let mut options = MqttOptions::new("proxy-client", "broker.example.com");
options.set_proxy(
    Proxy::http("127.0.0.1", 8080)
        .with_credentials("proxy-user", "proxy-password"),
);
```

For custom base socket creation, use `set_socket_connector`. The connector runs
before optional proxy, TLS, or WebSocket layers managed by `MqttOptions`.

```rust
use rumqttc::MqttOptions;

let mut options = MqttOptions::new("socket-client", "localhost");
options.set_socket_connector(|host, network_options| async move {
    rumqttc::default_socket_connect(host, network_options).await
});
```

If your connector already performs TLS or proxy work, configure `MqttOptions` so
those layers are not applied twice.

## Manual acknowledgement

Manual acknowledgement is configured through `AckMode`.

Old upstream style:

```rust,ignore
options.set_manual_acks(true);
```

New fork style:

```rust
use rumqttc::{AckMode, MqttOptions};

let mut options = MqttOptions::new("manual-ack-client", "localhost");
options.set_ack_mode(AckMode::Manual);
assert_eq!(options.ack_mode(), AckMode::Manual);
```

In manual mode, applications must acknowledge every incoming QoS 1 or QoS 2
publish. The simple path is `client.ack(&publish)`. Use `prepare_ack(...)` plus
`manual_ack(...)` when the ACK must be deferred or customized. MQTT 5 ACK
packets can carry reason codes and properties before `manual_ack(...)`.

## Topic aliases in MQTT 5

Automatic outgoing topic aliases are disabled by default. The old boolean-style
automatic alias setting was replaced by `TopicAliasPolicy`.

```rust
use rumqttc::{MqttOptions, TopicAliasPolicy};

let mut options = MqttOptions::new("alias-client", "localhost");
options.set_topic_alias_policy(TopicAliasPolicy::Monotonic);
options.set_topic_alias_max(Some(16));
```

`TopicAliasPolicy::Monotonic` assigns aliases until the broker-supported limit
is reached. `TopicAliasPolicy::Lru` can recycle automatically assigned aliases.
Manual aliases remain possible through MQTT 5 `PublishProperties`:

```rust
use rumqttc::mqttbytes::v5::PublishProperties;
use rumqttc::{PublishOptions, QoS};

let properties = PublishProperties {
    topic_alias: Some(1),
    ..Default::default()
};

let options = PublishOptions::at_most_once().properties(properties);
```

## MQTT 5 connect and session properties

The v5 crate exposes common CONNECT properties directly on `MqttOptions`:

```rust
use rumqttc::MqttOptions;

let mut options = MqttOptions::new("v5-client", "localhost");
options.set_clean_start(false);
options.set_session_expiry_interval(Some(3600));
options.set_receive_maximum(Some(32));
options.set_topic_alias_max(Some(16));
options.set_user_properties(vec![("app".into(), "bridge".into())]);
```

For complete control, build and set `ConnectProperties` with
`set_connect_properties(...)`.

## Persistent sessions and reconnect behavior

The event loop still makes progress only while `connection.iter()` or
`eventloop.poll()` is driven. Automatic reconnect happens by continuing to poll
after connection errors that are recoverable.

For MQTT 3.1.1 persistent sessions, use a stable client ID and
`clean_session(false)`:

```rust
use rumqttc::{MqttOptions, SessionMode};

let mut options = MqttOptions::new("stable-v4-client", "localhost");
options.set_clean_session(false);

let mut equivalent = MqttOptions::new("stable-v4-client", "localhost");
equivalent.set_session_mode(SessionMode::Persistent);
```

For MQTT 5 persistent sessions, use `clean_start(false)` and a non-zero session
expiry interval, or use `SessionMode::Persistent`:

```rust
use rumqttc::{MqttOptions, SessionMode};

let mut options = MqttOptions::new("stable-v5-client", "localhost");
options.set_clean_start(false);
options.set_session_expiry_interval(Some(3600));

let mut persistent = MqttOptions::new("stable-v5-client", "localhost");
persistent.set_session_mode(SessionMode::Persistent);
```

For restart-safe local session resume, configure a `SessionStore` with
`set_session_store(...)` or builder `.session_store(...)`. The client crate
provides the trait, scoped `SessionStoreKey`, persisted data model, and
canonical `PersistedSession::encode`/`decode` helpers; application code owns
durable storage layout. Use `set_session_store_scope(...)` when one store is
shared across brokers, tenants, environments, or connection profiles that may
reuse the same MQTT Client Identifier. See:

- `session-store-file/v4/examples/persistent_session_file_store.rs`
- `session-store-file/v5/examples/persistent_session_file_store_v5.rs`

MQTT 5 defaults to strict broker session reconciliation. If a newly constructed
client has no matching local session state and the broker returns
`Session Present = 1`, strict mode rejects that broker-only resume. Applications
that intentionally accept broker-retained messages without local in-flight state
can opt in:

```rust
use rumqttc::{BrokerSessionResumePolicy, MqttOptions};

let mut options = MqttOptions::new("stable-v5-client", "localhost");
options.set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly);
```

## Removed or renamed APIs

| Upstream-style API or pattern | Fork replacement |
| --- | --- |
| `Client::new(options, cap)` | `Client::builder(options).capacity(cap).build()` |
| `AsyncClient::new(options, cap)` | `AsyncClient::builder(options).capacity(cap).build()` |
| `AsyncClientBuilder::build_async()` | `AsyncClientBuilder::build()` |
| `publish(topic, qos, retain, payload)` | `publish(topic, payload, PublishOptions::new(qos))` plus retain setters |
| `publish_bytes(...)` | `publish(..., bytes_or_vec, PublishOptions::...)` |
| v5 `publish_with_properties(...)` | `publish(..., payload, PublishOptions::new(qos).properties(properties))` |
| `set_manual_acks(bool)` | `set_ack_mode(AckMode::{Automatic, Manual})` |
| `manual_acks()` | `ack_mode()` |
| `credentials() -> Option<Login>` | `auth() -> &ConnectAuth` |
| implicit `mqtts://`, `ssl://`, `wss://` transport selection | explicit TLS or WSS `set_transport(...)` |
| v5 automatic topic alias boolean | `set_topic_alias_policy(TopicAliasPolicy::...)` |

## Common recipes

Basic async MQTT 5 client:

```rust
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, QoS};

async fn run_client() -> Result<(), rumqttc::ClientError> {
    let mut options = MqttOptions::new("device-1", "localhost");
    options.set_keep_alive(30);

    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    client.subscribe("devices/1/commands", QoS::AtLeastOnce).await?;
    client
        .publish(
            "devices/1/status",
            "online",
            PublishOptions::at_least_once(),
        )
        .await?;

    tokio::spawn(async move {
        while let Ok(event) = eventloop.poll().await {
            println!("{event:?}");
        }
    });

    Ok(())
}
```

Retained QoS 1 publish:

```rust
use rumqttc::{PublishOptions, QoS};

let options = PublishOptions::new(QoS::AtLeastOnce).retained();
```

MQTT 5 topic alias policy:

```rust
use rumqttc::{MqttOptions, TopicAliasPolicy};

let mut options = MqttOptions::new("alias-client", "localhost");
options.set_topic_alias_policy(TopicAliasPolicy::Lru);
```

Manual ACK setup:

```rust
use rumqttc::{AckMode, MqttOptions};

let mut options = MqttOptions::new("consumer", "localhost");
options.set_ack_mode(AckMode::Manual);
```

The compile-checked migration examples live in:

- `rumqttc-v4/examples/migration_v4.rs`
- `rumqttc-v5/examples/migration_v5.rs`
