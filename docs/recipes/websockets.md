# WebSocket Recipes

Enable the `websocket` feature for `ws://` and `wss://` broker transports.

```toml
rumqttc-v5-next = { version = "0.34.0-alpha", features = ["websocket"] }
```

## Plain WebSockets

Use `Broker::websocket("ws://...")` when the broker exposes MQTT over
WebSockets.

Compile-checked examples:

- v4: `rumqttc-v4/examples/websocket.rs`
- v5: `rumqttc-v5/examples/websocket_v5.rs`

## Secure WebSockets

Secure WebSockets can be configured directly from a `wss://` endpoint when the
TLS configuration is known up front.

Compile-checked example:

- v4: `rumqttc-v4/examples/wss.rs`
- v5: `rumqttc-v5/examples/wss_v5.rs`

```rust,no_run
use rumqttc::{MqttOptions, TlsConfiguration};

let options = MqttOptions::websocket_with_tls_config(
    "client-id",
    "wss://broker.example.com/mqtt",
    TlsConfiguration::default_rustls(),
)
.expect("valid secure websocket options");
```

For lower-level transport overrides, `Broker::websocket("ws://...")` plus
`MqttOptions::set_transport(Transport::wss_with_config(...))` remains supported.

## Custom Headers

Use request modifiers for broker-specific headers, corporate gateway headers, or
short-lived authorization values that must be sent in the WebSocket handshake.

Compile-checked example:

- v4: `rumqttc-v4/examples/websocket_headers.rs`
- v5: `rumqttc-v5/examples/websocket_headers_v5.rs`

Do not log secrets from request modifiers.
