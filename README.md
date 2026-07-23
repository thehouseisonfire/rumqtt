<div align="center">
  <img alt="rumqtt Logo" src="docs/rumqtt.png" width="60%" />
</div>
<div align="center">
  <a href="https://coveralls.io/github/thehouseisonfire/rumqtt?branch=main">
    <img src="https://coveralls.io/repos/github/thehouseisonfire/rumqtt/badge.svg?branch=main" alt="Coverage Status" />
  </a>
</div>
<br/>

## What is rumqtt?

rumqtt is an open source set of Rust libraries for MQTT clients and packet handling, built to stay simple,
robust, and performant.

| Package | Description | Version |
| -- | -- | -- |
| [rumqttc-next](./rumqttc-next/) | Facade crate that re-exports the MQTT 5 client API | [![crates.io page](https://img.shields.io/crates/v/rumqttc-next.svg)](https://crates.io/crates/rumqttc-next) |
| [rumqttc-v5-next](./rumqttc-v5/) | Explicit MQTT 5 client crate | [![crates.io page](https://img.shields.io/crates/v/rumqttc-v5-next.svg)](https://crates.io/crates/rumqttc-v5-next) |
| [rumqttc-v4-next](./rumqttc-v4/) | Explicit MQTT 3.1.1 client crate | [![crates.io page](https://img.shields.io/crates/v/rumqttc-v4-next.svg)](https://crates.io/crates/rumqttc-v4-next) |
| [rumqttc-core-next](./rumqttc-core/) | Shared transport and connection plumbing for the client crates | [![crates.io page](https://img.shields.io/crates/v/rumqttc-core-next.svg)](https://crates.io/crates/rumqttc-core-next) |
| [mqttbytes-core-next](./mqttbytes-core/) | Shared MQTT packet primitives for the client crates | [![crates.io page](https://img.shields.io/crates/v/mqttbytes-core-next.svg)](https://crates.io/crates/mqttbytes-core-next) |

Optional file-backed implementations of the client-owned `SessionStore` APIs
are developed in the independent
[`session-store-file` workspace](./session-store-file/README.md). That workspace
contains a protocol-neutral filesystem core and one feature-gated MQTT 3.1.1
and MQTT 5 adapter package; it is not part of the client dependency graph.

rumqttc-next is a maintained fork of rumqtt with a variety of extra features.

The client crates are published with `*-next` package names on crates.io, but their Rust library target is
still named `rumqttc`. After adding a dependency, application code can use familiar imports such as
`use rumqttc::MqttOptions;`.

## Installation and Usage

### Choose a client package

| Use case | Package |
| -- | -- |
| Default MQTT 5 client | `rumqttc-next` |
| Explicit MQTT 5 package name | `rumqttc-v5-next` |
| MQTT 3.1.1 client | `rumqttc-v4-next` |

Run one of these, depending on the protocol package you want:

```bash
# Default MQTT 5 client
cargo add rumqttc-next@0.34.0-alpha

# Explicit MQTT 5 package
cargo add rumqttc-v5-next@0.34.0-alpha

# MQTT 3.1.1 package
cargo add rumqttc-v4-next@0.34.0-alpha
```

For more details, see [rumqttc-next/README.md](./rumqttc-next/README.md), [rumqttc-v5/README.md](./rumqttc-v5/README.md), and [rumqttc-v4/README.md](./rumqttc-v4/README.md).

Existing upstream `rumqttc` users should start with the [migration guide](./MIGRATION.md) for package names,
API changes, and porting recipes.

Production deployment examples for TLS, WebSockets, proxies, persistent
sessions, reconnect handling, bounded channels, manual ACKs, and broker-specific
setup notes are in the [recipe guide](./docs/recipes/README.md).

On Linux, both clients support opt-in Multipath TCP through
`NetworkOptions::set_mptcp`; see the [MPTCP recipe](./docs/recipes/mptcp.md).

## Features

### rumqttc-next / rumqttc-v5-next
- [x] MQTT 5
- [x] WebSocket transport
- [x] TLS via rustls or native-tls
- [x] MQTT 5 properties, reason codes, topic aliases, and enhanced auth hooks

### rumqttc-v4-next
- [x] MQTT 3.1.1
- [x] WebSocket transport
- [x] TLS via rustls or native-tls
- [x] Strict MQTT 3.1.1 codec validation

## Spec Compliance Docs

- [MQTT 3.1.1 spec notes](./docs/spec/mqtt-v3.1.1.md) and [requirement index (full compliance verification ongoing)](./docs/spec/mqtt-v3.1.1.requirements.json)
- [MQTT 5.0 spec notes](./docs/spec/mqtt-v5.0.md) and [requirement index (ongoing)](./docs/spec/mqtt-v5.0.requirements.json)

## License

This project is released under The Apache License, Version 2.0 ([LICENSE](./LICENSE) or http://www.apache.org/licenses/LICENSE-2.0)
