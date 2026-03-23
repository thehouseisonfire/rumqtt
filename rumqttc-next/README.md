# rumqttc-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-next.svg)](https://crates.io/crates/rumqttc-next)
[![docs.rs page](https://docs.rs/rumqttc-next/badge.svg)](https://docs.rs/rumqttc-next)

`rumqttc-next` is the default MQTT 5 client crate in the rumqtt family.
It is a facade over `rumqttc-v5-next` and re-exports that crate's public API unchanged.

## Scope

- Use `rumqttc-next` when you want the current MQTT 5 client under the traditional `rumqttc` API surface.
- Use `rumqttc-v5-next` when you want the explicit protocol-scoped package name.
- Use `rumqttc-v4-next` when you need MQTT 3.1.1 instead of MQTT 5.

## Installation

```bash
cargo add rumqttc-next
```

## Features

This crate forwards the MQTT 5 client features from `rumqttc-v5-next`, including:

- `use-rustls` / `use-rustls-no-provider` / `use-rustls-aws-lc` / `use-rustls-ring`
- `use-native-tls`
- `websocket`
- `proxy`
- `auth-scram`
- `url`

See the `rumqttc-v5-next` documentation for the full API and examples.
