# WebSocket Recipes

Enable the `websocket` feature for `ws://` and `wss://` broker transports.

```toml
rumqttc-v5-next = { version = "0.33", features = ["websocket"] }
```

## Plain WebSockets

Use `Broker::websocket("ws://...")` when the broker exposes MQTT over
WebSockets.

Compile-checked examples:

- v4: `rumqttc-v4/examples/websocket.rs`
- v5: `rumqttc-v5/examples/websocket_v5.rs`

## Secure WebSockets

Secure WebSockets use a websocket broker URL plus explicit WSS transport. Use a
`ws://` URL in `Broker::websocket(...)`; the TLS upgrade is selected by
`Transport::wss_with_config(...)`.

Compile-checked example:

- v4: `rumqttc-v4/examples/wss.rs`
- v5: `rumqttc-v5/examples/wss_v5.rs`

```rust,no_run
use rumqttc::{Broker, MqttOptions, TlsConfiguration, Transport};

let mut options = MqttOptions::new(
    "client-id",
    Broker::websocket("ws://broker.example.com:443/mqtt").expect("valid websocket URL"),
);
options.set_transport(Transport::wss_with_config(TlsConfiguration::default_rustls()));
```

## Custom Headers

Use request modifiers for broker-specific headers, corporate gateway headers, or
short-lived authorization values that must be sent in the WebSocket handshake.

Compile-checked example:

- v4: `rumqttc-v4/examples/websocket_headers.rs`
- v5: `rumqttc-v5/examples/websocket_headers_v5.rs`

Do not log secrets from request modifiers.
