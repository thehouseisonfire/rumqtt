# TLS Recipes

TLS is configured through `MqttOptions::set_transport(...)`. Secure URL schemes
such as `mqtts://`, `ssl://`, and `wss://` are intentionally not enough by
themselves; configure the TLS transport explicitly so the backend and roots are
unambiguous.

## Rustls With Platform Roots

Use the default `use-rustls` feature for most deployments:

```toml
rumqttc-v5-next = { version = "0.33", features = ["use-rustls"] }
```

The compile-checked examples are:

- v4: `rumqttc-v4/examples/tls.rs`
- v5: `rumqttc-v5/examples/tls_v5.rs`

Use this pattern for public CA certificates, managed brokers, and corporate
trust stores already installed on the host.

## Custom CA and Client Certificates

Use custom CA bytes when the broker presents a private CA certificate. Add client
certificate and key bytes when the broker requires mutual TLS, such as many AWS
IoT deployments.

The compile-checked examples are:

- v4: `rumqttc-v4/examples/tls2.rs`
- v5: `rumqttc-v5/examples/tls_client_auth_v5.rs`

For rustls, provide PEM bytes:

```rust,no_run
use rumqttc::{MqttOptions, TlsConfiguration, Transport};

let ca = include_bytes!("AmazonRootCA1.pem").to_vec();
let client_cert = include_bytes!("device-certificate.pem.crt").to_vec();
let client_key = include_bytes!("private.pem.key").to_vec();

let mut options = MqttOptions::new("device-id", ("example.iot.region.amazonaws.com", 8883));
options.set_transport(Transport::tls_with_config(TlsConfiguration::Simple {
    ca,
    alpn: None,
    client_auth: Some((client_cert, client_key)),
}));
```

For native-tls, use `TlsConfiguration::simple_native(...)` with a PEM CA and an
optional PKCS#12 identity.

## Backend Selection

- `use-rustls` is the default and selects the `aws-lc` provider.
- `use-rustls-ring` and `use-rustls-aws-lc` are mutually exclusive.
- `use-native-tls` uses the platform TLS stack.
- If both rustls and native-tls are enabled through a dependency graph, pass an
explicit `TlsConfiguration` instead of relying on `Default`.
