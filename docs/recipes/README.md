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
| HTTP/HTTPS/SOCKS5 proxy | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `http-proxy` or `socks-proxy`; `proxy` enables both; add a TLS feature for HTTPS | [Proxies](./proxies.md) |
| Multipath TCP | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none; Linux only | [Multipath TCP](./mptcp.md) |
| Persistent sessions | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Sessions](./sessions.md) |
| Reconnect resubscribe | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Sessions](./sessions.md) |
| Bounded client channels | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Backpressure](./backpressure.md) |
| Manual ACKs | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Manual ACKs](./manual-acks.md) |
| Structured lifecycle tracing | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | `tracing` | [Tracing](./tracing.md) |
| Runtime diagnostics snapshots | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | none | [Diagnostics](./diagnostics.md) |
| Broker-specific setup notes | `rumqttc-v4-next` | `rumqttc-v5-next` / `rumqttc-next` | varies | [Broker notes](./brokers.md) |

## Compile-Checked Examples

Every Rust block in this guide is compiled against both protocol crates unless
the block is explicitly marked v4- or v5-only. Every linked example and Cargo
command is also checked by `python3 scripts/check-recipes.py` in CI. The
following matrix covers the production-sensitive feature combinations:

```bash
cargo run -p rumqttc-v4-next --features use-rustls --example tls2
cargo run -p rumqttc-v5-next --features use-rustls --example tls_client_auth_v5
cargo run -p rumqttc-v4-next --features websocket,use-rustls --example wss
cargo run -p rumqttc-v5-next --features websocket,use-rustls --example wss_v5
cargo run -p rumqttc-v4-next --features websocket,http-proxy --example websocket_proxy
cargo run -p rumqttc-v5-next --features websocket,http-proxy --example websocket_proxy_v5
cargo run -p rumqttc-v4-next --features socks-proxy --example socks5_proxy
cargo run -p rumqttc-v5-next --features socks-proxy --example socks5_proxy_v5
cargo run -p rumqttc-v4-next --example resubscribe_on_reconnect
cargo run -p rumqttc-v5-next --example resubscribe_on_reconnect_v5
cargo run -p rumqttc-v4-next --example async_manual_acks
cargo run -p rumqttc-v5-next --example async_manual_acks_v5
cargo run -p rumqttc-v4-next --features tracing --example lifecycle_tracing
cargo run -p rumqttc-v5-next --features tracing --example lifecycle_tracing_v5
cargo run -p rumqttc-v4-next --example diagnostics_snapshot
cargo run -p rumqttc-v5-next --example diagnostics_snapshot_v5
```

These are runnable examples and therefore try to connect to their documented
endpoint. CI substitutes `cargo check` to validate them without contacting
external services; the local Mosquitto recipe has a separate end-to-end smoke
test.

Use package names such as `rumqttc-v5-next` in Cargo commands. Application code
continues to import the library as `rumqttc`.
