# rumqttc-core-next

[![crates.io page](https://img.shields.io/crates/v/rumqttc-core-next.svg)](https://crates.io/crates/rumqttc-core-next)
[![docs.rs page](https://docs.rs/rumqttc-core-next/badge.svg)](https://docs.rs/rumqttc-core-next)

`rumqttc-core-next` contains the shared transport and connection plumbing used by the `rumqttc-next` protocol crates.

## Scope

- Shared TCP, TLS, WebSocket, and proxy integration code.
- Shared `NetworkOptions`, `TlsConfiguration`, socket connectors, and adapter traits.
- Internal support crate for `rumqttc-v4-next`, `rumqttc-v5-next`, and the `rumqttc-next` facade.

This crate is not a full MQTT client and does not expose the v4 or v5 protocol APIs by itself.

The optional `session-recovery` feature provides internal managed-owner recovery
coordination. Protocol clients forward it, and native wrappers enable it explicitly.
It is disabled by default.

## TCP connection selection

The default connector and `default_socket_connect` use the same shared dialer.
After system hostname resolution completes, it alternates IPv4 and IPv6 addresses,
preserving the resolver's first address family and the order within each family.
It starts attempts 100 ms apart, returns the first successful TCP stream, and drops
all losing attempts before proxy, TLS, WebSocket or MQTT negotiation. Cancelling
the dial future also drops pending attempts.

A fixed nonzero `NetworkOptions::set_bind_addr` port uses sequential resolver order
to avoid overlapping source-port binds. An ephemeral port (`0`) allows racing.
Custom socket connectors own their dialing policy; delegating to
`default_socket_connect` retains the default policy and applies the supplied
network options. The MQTT event loop enforces the overall connection deadline;
standalone helper callers must apply their own timeout.

This provides partial Happy Eyeballs support. It does not implement RFC 8305's
independent A/AAAA resolution or start connecting before system resolution finishes.

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

## Connection-local TLS policies

`TlsConfiguration::Connector(Arc<dyn TlsHandshakeConnector>)` constructs TLS
on an already connected base stream. Both clients invoke it for broker TLS/WSS
and HTTPS proxies, forwarding the original absolute connection deadline. The
connector must return only after all authentication checks succeed and must
retain its owners until its future completes or is cancelled. Per-handshake
mutable state belongs in that future, allowing one configuration to be shared
safely across clients. Return `TerminalTransportError` for authentication
failures that must stop MQTT 5 SRV candidate fallback. Existing backend variants
and the deadline-free `tls_connect` helper remain available.
