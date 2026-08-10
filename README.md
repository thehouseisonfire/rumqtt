<div align="center">
  <img alt="rumqtt logo" src="docs/rumqtt-next.png" width="60%" />
</div>

<div align="center">
  <a href="https://crates.io/crates/rumqttc-next">
    <img src="https://img.shields.io/crates/v/mqttbytes-core-next.svg" alt="crates.io version" />
  </a>
  <a href="https://crates.io/crates/rumqttc-next">
    <img src="https://img.shields.io/crates/d/mqttbytes-core-next.svg" alt="crates.io downloads" />
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
  <img src="https://img.shields.io/badge/rustc-1.88%2B-blue" alt="rustc 1.88 or newer" />
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0 license" /></a>
</div>

## Reliable MQTT clients for Rust

Rumqttc-next provides asynchronous and synchronous MQTT clients with a small, explicit
API and close control over connection behavior. It supports MQTT 3.1.1 and MQTT
5, TLS, WebSockets, proxies, tracking notice API, persistent sessions, manual
acknowledgements, request and network-read batching, graceful disconnect, and
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
- client operations can be tracked to their actual protocol outcome;
- manual and customizable acknowledgements;
- stricter packet and protocol-state validation, backed by
  [spec-compliance references](./docs/spec/);
- persistent session APIs, structured diagnostics, lifecycle tracing, and
  explicit reconnect, acknowledgement, and topic-alias policies;
- Configurable request and network-read batching, which can substantially
  improve throughput under sustained load;
- HTTP and SOCKS5 proxies, TLS backends, and WebSocket transports, and
  opt-in Linux Multipath TCP;
- updated dependencies to address vulnerabilities reported in RUSTSEC
  advisories.

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

### Using both MQTT versions

The explicit v4 and v5 packages both expose a library target named `rumqttc`.
When one crate depends on both packages, give each dependency a distinct name
in `Cargo.toml`:

```toml
[dependencies]
rumqttc_v4 = { package = "rumqttc-v4-next", version = "0.34.0-alpha" }
rumqttc_v5 = { package = "rumqttc-v5-next", version = "0.34.0-alpha" }
```

The dependency names become the paths used by Rust code:

```rust
use rumqttc_v4::{AsyncClient as V4Client, MqttOptions as V4Options};
use rumqttc_v5::{AsyncClient as V5Client, MqttOptions as V5Options};
```

Without distinct dependency names, both library targets would claim the
`rumqttc` extern-crate name in the same target.

Shared transport and codec code is published as
[`rumqttc-core-next`](./rumqttc-core/) and
[`mqttbytes-core-next`](./mqttbytes-core/). Standalone, `no_std`-capable packet
codecs are published as [`mqttbytes-v4-next`](./mqttbytes-v4/) and
[`mqttbytes-v5-next`](./mqttbytes-v5/). Optional file-backed session stores
live in the independent
[`session-store-file` workspace](./session-store-file/README.md) and do not add
filesystem dependencies to the clients.
Its protocol-neutral storage engine is maintained separately as
[`atomic-blob-store`](https://github.com/thehouseisonfire/atomic-blob-store).

## Guides and ecosystem

The [production recipes](./docs/recipes/README.md) cover TLS, WebSockets,
proxies, Notice API, persistent sessions, reconnect handling, bounded channels, manual
ACKs, broker-specific configuration, and others.

Projects integrating this fork include
[`mqtt-typed-client`](https://github.com/holovskyi/mqtt-typed-client), a
type-safe MQTT layer with an optional `backend-rumqttc-next` backend. See each
package's crates.io page for current download and reverse-dependency data.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
