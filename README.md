<div align="center">
  <img alt="rumqtt logo" src="docs/rumqtt.png" width="60%" />
</div>

<div align="center">
  <a href="https://crates.io/crates/rumqttc-next">
    <img src="https://img.shields.io/crates/v/rumqttc-next.svg" alt="crates.io version" />
  </a>
  <a href="https://crates.io/crates/rumqttc-next">
    <img src="https://img.shields.io/crates/d/rumqttc-next.svg" alt="crates.io downloads" />
  </a>
  <a href="https://github.com/thehouseisonfire/rumqtt/commits/main">
    <img
      src="https://img.shields.io/github/commit-activity/m/thehouseisonfire/rumqtt"
      alt="monthly commit activity"
    />
  </a>
  <a href="https://coveralls.io/github/thehouseisonfire/rumqtt?branch=main">
    <img
      src="https://coveralls.io/repos/github/thehouseisonfire/rumqtt/badge.svg?branch=main"
      alt="coverage status"
    />
  </a>
  <img src="https://img.shields.io/badge/rustc-1.89%2B-blue" alt="rustc 1.89 or newer" />
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0 license" /></a>
</div>

## Reliable MQTT clients for Rust

rumqtt provides asynchronous and synchronous MQTT clients with a small, explicit
API and close control over connection behavior. It supports MQTT 3.1.1 and MQTT
5, TLS, WebSockets, proxies, persistent sessions, manual acknowledgements, and
reconnect handling.

This repository is an **actively maintained fork of
[`rumqttc`](https://github.com/bytebeamio/rumqtt)**, started in response to a
period of upstream inactivity. It preserves the original project's focus on
simplicity and performance while continuing protocol hardening, operational
tooling, and API development independently.

See the [migration and API differences guide](./MIGRATION.md) for a practical
comparison with upstream, and the [changelog](./CHANGELOG.md) for the complete
list of additions and fixes. Highlights include:

- separate, intentionally versioned MQTT 3.1.1 and MQTT 5 clients;
- stricter packet and protocol-state validation, backed by
  [spec-compliance references](./docs/spec/);
- persistent session APIs, structured diagnostics, lifecycle tracing, and
  explicit reconnect, acknowledgement, and topic-alias policies;
- HTTP and SOCKS5 proxies, TLS and WebSocket transports, and opt-in Linux
  Multipath TCP.

## Choose a client

| Use case | Cargo package | Rust crate |
| -- | -- | -- |
| MQTT 5 (recommended entry point) | [`rumqttc-next`](./rumqttc-next/) | `rumqttc` |
| MQTT 5 (explicit package) | [`rumqttc-v5-next`](./rumqttc-v5/) | `rumqttc` |
| MQTT 3.1.1 | [`rumqttc-v4-next`](./rumqttc-v4/) | `rumqttc` |

The `*-next` names are the packages published on crates.io; each library target
is still named `rumqttc`, so application imports remain familiar.

```bash
cargo add rumqttc-next@0.34.0-alpha
```

```rust
use rumqttc::{AsyncClient, MqttOptions};

let options = MqttOptions::new("client-id", "localhost");
let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
```

Use `rumqttc-v4-next` in the command above for MQTT 3.1.1. For complete setup
and usage, see the
[`rumqttc-next`](./rumqttc-next/README.md),
[`rumqttc-v5-next`](./rumqttc-v5/README.md), and
[`rumqttc-v4-next`](./rumqttc-v4/README.md) crate documentation.

Shared transport and codec code is published as
[`rumqttc-core-next`](./rumqttc-core/) and
[`mqttbytes-core-next`](./mqttbytes-core/). Optional file-backed session stores
live in the independent
[`session-store-file` workspace](./session-store-file/README.md) and do not add
filesystem dependencies to the clients.

## Guides and ecosystem

The [production recipes](./docs/recipes/README.md) cover TLS, WebSockets,
proxies, persistent sessions, reconnect handling, bounded channels, manual
ACKs, and broker-specific configuration.

Projects integrating this fork include
[`mqtt-typed-client`](https://github.com/holovskyi/mqtt-typed-client), a
type-safe MQTT layer with an optional `backend-rumqttc-next` backend. See each
package's crates.io page for current download and reverse-dependency data.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
