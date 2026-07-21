# Proxy Recipes

Enable `http-proxy` for HTTP(S), `socks-proxy` for SOCKS5, or the `proxy`
compatibility umbrella to enable both protocols.

```toml
rumqttc-v5-next = { version = "0.34.0-alpha", features = ["socks-proxy"] }
```

## HTTP Proxy

Enable the `http-proxy` feature. Configure `MqttOptions::set_proxy(...)` with a
proxy constructor and optional credentials.

Compile-checked examples:

- v4: `rumqttc-v4/examples/websocket_proxy.rs`
- v5: `rumqttc-v5/examples/websocket_proxy_v5.rs`

```rust,no_run
use rumqttc::{MqttOptions, Proxy};

let mut options = MqttOptions::new("client-id", ("broker.example.com", 1883));
options.set_proxy(
    Proxy::http("proxy.corp.example", 8080)
        .with_credentials("proxy-user", "proxy-password"),
);
```

## HTTPS Proxy

Use `Proxy::https(...)` when the connection to the proxy itself must be TLS.
This requires a TLS feature in addition to `http-proxy`.

Proxy TLS and broker TLS are separate layers:

- `Proxy::https(...)` protects the client-to-proxy hop.
- `Transport::tls_with_config(...)` or `Transport::wss_with_config(...)`
  protects the MQTT broker hop after CONNECT tunneling.

## SOCKS5 Proxy

`Proxy::socks5(...)` supports no authentication or RFC 1929 username/password
authentication under the `socks-proxy` feature:

```rust,no_run
use rumqttc::{MqttOptions, Proxy};

let mut options = MqttOptions::new("client-id", ("broker.internal.example", 1883));
options.set_proxy(
    Proxy::socks5("127.0.0.1", 1080)
        .with_credentials("proxy-user", "proxy-password"),
);
```

Broker hostnames are resolved by the SOCKS5 proxy, which avoids local DNS leaks
and supports names visible only to the proxy. Pass an IP address as the broker
host when local resolution is required.

Compile-checked examples are `rumqttc-v4/examples/socks5_proxy.rs` and
`rumqttc-v5/examples/socks5_proxy_v5.rs`.

## Corporate Environments

Prefer explicit proxy settings from deployment configuration. Avoid silently
reading environment proxy variables unless the application owns that policy.
