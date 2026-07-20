# Proxy Recipes

Enable the `proxy` feature when the client must connect through an HTTP proxy.

```toml
rumqttc-v5-next = { version = "0.34.0-alpha", features = ["proxy"] }
```

## HTTP Proxy

Configure `MqttOptions::set_proxy(...)` with the proxy host, port, type, and
optional basic authentication.

Compile-checked examples:

- v4: `rumqttc-v4/examples/websocket_proxy.rs`
- v5: `rumqttc-v5/examples/websocket_proxy_v5.rs`

```rust,no_run
use rumqttc::{MqttOptions, Proxy, ProxyAuth, ProxyType};

let mut options = MqttOptions::new("client-id", ("broker.example.com", 1883));
options.set_proxy(Proxy {
    ty: ProxyType::Http,
    auth: ProxyAuth::Basic {
        username: "proxy-user".into(),
        password: "proxy-password".into(),
    },
    addr: "proxy.corp.example".into(),
    port: 8080,
});
```

## HTTPS Proxy

Use `ProxyType::Https(...)` when the connection to the proxy itself must be TLS.
This requires a TLS feature in addition to `proxy`.

Proxy TLS and broker TLS are separate layers:

- `ProxyType::Https(...)` protects the client-to-proxy hop.
- `Transport::tls_with_config(...)` or `Transport::wss_with_config(...)`
  protects the MQTT broker hop after CONNECT tunneling.

## Corporate Environments

Prefer explicit proxy settings from deployment configuration. Avoid silently
reading environment proxy variables unless the application owns that policy.
