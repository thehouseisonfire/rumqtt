# mqttbytes-core-next

[![crates.io page](https://img.shields.io/crates/v/mqttbytes-core-next.svg)](https://crates.io/crates/mqttbytes-core-next)
[![docs.rs page](https://docs.rs/mqttbytes-core-next/badge.svg)](https://docs.rs/mqttbytes-core-next)

`mqttbytes-core-next` contains shared MQTT packet primitives used by the `rumqttc-next` protocol crates.

## Scope

- Shared QoS, topic, ping, and primitive byte helpers.
- Reused by both `rumqttc-v4-next` and `rumqttc-v5-next`.
- Intended mainly for the rumqtt client family and advanced integrations that need low-level MQTT building blocks.

This crate is not a full MQTT client and does not provide networking, event loops, or broker session management.
