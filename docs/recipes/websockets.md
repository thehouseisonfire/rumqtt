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

## Native wrapper token refresh

Wrapper-core supports an owned asynchronous `WebSocketHandshake` authority;
the C API exposes a retained registration and owned response builder. Static
headers are applied before the callback on every prepared connection attempt.
Responses may change the path/query, explicit HTTP authority, and unprotected
headers. `WebSocketHandshakeResponse::set_authority()` and C
`rumqttc_websocket_response_set_authority()` update URI authority and `Host`
together, accepting a host with an optional numeric port or bracketed IPv6.
For example, dial `gateway.example` and send `Host: mqtt.customer.example` for
virtual-host routing. TLS still verifies `gateway.example`. GET, HTTP/1.1,
scheme, and upgrade fields remain fixed, and request edits do not alter
the configured dial target or TLS authority.

Return promptly and complete deferred work from a host worker within the original
connection deadline. Retain the completion before returning from a C callback
and copy any borrowed request data needed later. The
[C token refresh example](../../native-wrappers/c/examples/websocket_tokens.c)
and [C API guide](../../native-wrappers/c/README.md) describe ownership and limits.
Wrapper diagnostics redact handshake data. Upstream WebSocket dependency TRACE
logging can include outgoing requests; configure those log targets accordingly.
