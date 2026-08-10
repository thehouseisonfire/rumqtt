# mqttbytes-v5-next

[![crates.io page](https://img.shields.io/crates/v/mqttbytes-v5-next.svg)](https://crates.io/crates/mqttbytes-v5-next)
[![docs.rs page](https://docs.rs/mqttbytes-v5-next/badge.svg)](https://docs.rs/mqttbytes-v5-next)

`mqttbytes-v5-next` provides the MQTT 5 packet types and byte codec used by
`rumqttc-v5-next`.

The packet codec supports `no_std` environments with an allocator when default
features are disabled. The default `std` feature enables standard-library
integration without requiring Tokio. Enable the opt-in `codec` feature to
expose the `tokio-util`-based framed `Codec`; `codec` implies `std`.

This crate does not provide networking, an event loop, or broker session
management.
