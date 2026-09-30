# rumqttc-core-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-core-next.svg)](https://crates.io/crates/rumqttc-core-next)
[![docs.rs page](https://docs.rs/rumqttc-core-next/badge.svg)](https://docs.rs/rumqttc-core-next)

`rumqttc-core-next` contains the shared transport and connection plumbing used by the `rumqttc-next` protocol crates.

## Scope

- Shared TCP, TLS, WebSocket, and proxy integration code.
- Shared `NetworkOptions`, `TlsConfiguration`, socket connectors, and adapter traits.
- Internal support crate for `rumqttc-v4-next`, `rumqttc-v5-next`, and the `rumqttc-next` facade.

This crate is not a full MQTT client and does not expose the v4 or v5 protocol APIs by itself.

## Terminal transport failures

Socket connectors and custom streams can return
`TerminalTransportError::new(original_io_error).into_io()` to prevent MQTT 5 SRV
redirects from trying further candidates. The marker preserves the original
error as a source through transport composition; `TerminalTransportError::find`
recovers it from a connection error. Unmarked candidate errors retain the usual
SRV fallback behavior. The protocol crates also reexport this marker.

## Eager rustls configuration

With a rustls feature enabled, `TlsConfiguration::try_rustls_with_native_roots`
and `try_rustls_with_pem_roots` fully build the client configuration and
optionally install a PEM client certificate chain and private key. They return
`TlsError` for root-loading, PEM, key, provider, and certificate/key compatibility
failures before a connection attempt. `try_default_rustls()` delegates to the
native-root constructor; `default_rustls()` remains the panicking convenience
adapter.
