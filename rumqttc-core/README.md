# rumqttc-core-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-core-next.svg)](https://crates.io/crates/rumqttc-core-next)
[![docs.rs page](https://docs.rs/rumqttc-core-next/badge.svg)](https://docs.rs/rumqttc-core-next)

`rumqttc-core-next` contains the shared transport and connection plumbing used by the `rumqttc-next` protocol crates.

## Scope

- Shared TCP, TLS, WebSocket, and proxy integration code.
- Shared `NetworkOptions`, `TlsConfiguration`, socket connectors, and adapter traits.
- Internal support crate for `rumqttc-v4-next`, `rumqttc-v5-next`, and the `rumqttc-next` facade.

This crate is not a full MQTT client and does not expose the v4 or v5 protocol APIs by itself.
