# rumqttc Production Recipes

These recipes show common production deployment patterns for `rumqttc-v4-next`,
`rumqttc-v5-next`, and the MQTT 5 facade package `rumqttc-next`.

The snippets prefer APIs that are compile-checked in crate examples. Replace
hosts, topics, credentials, certificates, and proxy details with values from your
deployment.

## Feature Matrix

| Capability | v4 package | v5 package | Required features | Recipe |
| --- | --- | --- | --- | --- |
| TLS with platform roots | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | default `use-rustls` or `use-native-tls` | [TLS](./tls.md) |
| TLS with custom CA/client cert | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `use-rustls` or `use-native-tls` | [TLS](./tls.md) |
| WebSocket transport | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `websocket` | [WebSockets](./websockets.md) |
| Secure WebSockets | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `websocket` plus TLS feature | [WebSockets](./websockets.md) |
| WebSocket headers | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `websocket` | [WebSockets](./websockets.md) |
| HTTP/HTTPS proxy | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `proxy`, plus TLS feature for HTTPS proxy | [Proxies](./proxies.md) |
| Persistent sessions | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Sessions](./sessions.md) |
| Reconnect resubscribe | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Sessions](./sessions.md) |
| Bounded client channels | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Backpressure](./backpressure.md) |
| Manual ACKs | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Manual ACKs](./manual-acks.md) |
| Broker-specific setup notes | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | varies | [Broker notes](./brokers.md) |

## Compile-Checked Examples

Most recipes link to examples under `rumqttc-v4/examples/` or
`rumqttc-v5/examples/`. Run an example with the package name and required
features, for example:

```bash
cargo run -p rumqttc-v5-next --features websocket --example websocket_v5
cargo run -p rumqttc-v5-next --features websocket,proxy --example websocket_proxy_v5
cargo run -p rumqttc-v5-next --features use-rustls --example tls_v5
```

Use package names such as `rumqttc-v5-next` in Cargo commands. Application code
continues to import the library as `rumqttc`.
