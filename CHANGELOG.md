# CHANGELOG

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `rumqttc` v4/v5: Add `set_max_request_batch(usize)` and `max_request_batch()` on `MqttOptions` to control how many queued requests are processed per eventloop iteration (higher values can improve throughput by batching writes/flushes).
### Changed
- `rumqttc` v5: Change connect timeout API from seconds-based `connection_timeout()`/`set_connection_timeout(u64)` to `Duration`-based `connect_timeout()`/`set_connect_timeout(Duration)`, and update internal connect timeout handling accordingly.
- `rumqttc` v4: Change `mqttbytes::v4::Publish.topic` from `String` to `bytes::Bytes`, reducing topic allocation/copy overhead in packet decode/encode paths (topic UTF-8 validation is still enforced).
### Deprecated
### Removed
### Fixed
- `rumqttc` v4/v5: Avoid panicking when applying TCP socket send/recv buffer sizes; these configuration failures now return an error from connect setup.
### Security


---

## [rumqttc 0.27.0] - 19-02-2026

### Added
- `rumqttc`: Add `use-rustls-aws-lc` and `use-rustls-ring` feature flags for explicit rustls crypto provider selection
(see security note)
### Changed
- `rumqttc`: Make `use-rustls` default to the `aws-lc` rustls provider.
- `rumqttc`: Replace docs.rs `all-features` configuration with an explicit non-conflicting feature list.
- `rumqttc` WebSocket transport: Replaced `ws_stream_tungstenite` with `async-tungstenite` native `ByteReader`/`ByteWriter` via `WsAdapter`; `ws_stream_tungstenite` is no longer a dependency and public websocket APIs remain unchanged.
- `rumqttc`: Migrated workspace and member crates to Rust Edition 2024
- `rumqttc`: Bumped MSRV to 1.85 (2024 Edition)
- `rumqttc (dev)`: WSS integration tests
- `rumqttc (dev)`: Added `rcgen` development dependency
- `rumqttc (dev)`: bumped `rand` to 0.10
### Deprecated
### Removed
### Fixed
- `rumqttc`: Add a compile-time guard that rejects enabling both `use-rustls-aws-lc` and `use-rustls-ring`.
- `rumqttc`: Renamed crate from `rumqttc_next` back to `rumqttc`.
### Security
- In `0.26.1`, the crate user would have to specify `ring` or `aws-lc-rs` explicitly as a dependency for `rustls` to use them.


---

## [rumqttc 0.26.1] - 16-02-2026

### Added
### Changed
- Migrated from deprecated `rustls-pemfile` to `rustls-pki-types` PEM parsing API
### Deprecated
### Removed
### Fixed
### Security


---

## [rumqttc 0.26.0] - 16-02-2026

### Added
* Add `v5::ValidatedTopic` and `v5::InvalidTopic` for one-time topic validation and reuse across publish APIs.
* Add AsyncReadWrite trait with conditional compilation for websocket feature to allow proper trait bounds depending on websocket feature usage.
### Changed
* Make v5 publish APIs accept `v5::Topic` and support skipping repeated validation when using `v5::ValidatedTopic`.
* Reduce intermediate topic conversions/copies in v4/v5 publish client paths by passing owned topic strings directly into publish packet constructors.
* Add `v5::MqttOptions::set_incoming_packet_size_limit` and `v5::MqttOptions::set_unlimited_incoming_packet_size` as the preferred v5 APIs for incoming packet size behavior (planned for `0.25.2`).
* Apply broad Clippy `pedantic`/`nursery` cleanup across `rumqttc` internals with targeted refactors in v4/v5 packet encoding, eventloop setup, and state-machine handlers.
* Make `v5::ClientError` store boxed requests to reduce `Result<_, ClientError>` footprint across publish/subscribe APIs.
* Change v4/v5 `MqttOptions::set_keep_alive` to accept `u16` seconds (MQTT wire-level keepalive units), including `0` to disable automatic keep-alive pings.
* Update v5 disconnect request plumbing to use `Request::Disconnect(Disconnect)` so disconnect reason code/properties are propagated end-to-end.
### Deprecated
### Removed
### Fixed 
* Derive `Eq` and `PartialEq` for `client::ClientError` to make downstream error assertions easier in tests.
* Harden integer conversion paths used in keepalive and packet serialization to avoid silent truncation.
* Fix v5 auth continuation lock-scrutinee lifetime pattern to avoid holding lock guard longer than necessary.
* Tighten helper signatures/ownership in state handlers and packet helpers (fewer unnecessary mutable/value parameters).
* Improve debug output behavior for `MqttOptions` manual `Debug` impls via non-exhaustive finishing.
* Clear collision state on reconnection with clean session.
* Preserve MQTT v5 DISCONNECT properties on the wire for `disconnect_with_properties` and `try_disconnect_with_properties`.
* Restore `EventLoop::new` API compatibility while allowing `AsyncClient::new` event loops to terminate with `ConnectionError::RequestsDone` when all client handles are dropped.
* Clarify shutdown semantics: `disconnect()`/`try_disconnect()` performs MQTT graceful shutdown (sends DISCONNECT), while dropping all clients ends polling with `ConnectionError::RequestsDone` without implicit DISCONNECT.
### Security


---

## [rumqttc 0.25.1] - 21-11-2025

### Added
* `use-rustls-no-provider` feature flag to allow choosing crypto backend without being forced to compile `aws_lc_rs`

### Changed
### Deprecated
### Removed
### Fixed 
* Fixed broken websocket feature in rumqttc-0.25.0

### Security


---

## [rumqttc 0.25.0] - 09-10-2025

### Added

* `size()` method on `Packet` calculates size once serialized.
* `read()` and `write()` methods on `Packet`.
* `ConnectionAborted` variant on `StateError` type to denote abrupt end to a connection
* `set_session_expiry_interval` and `session_expiry_interval` methods on `MqttOptions`.
* `Auth` packet as per MQTT5 standards
* Allow configuring  the `nodelay` property of underlying TCP client with the `tcp_nodelay` field in `NetworkOptions`
* `set_client_id` method on `MqttOptions`

### Changed

* rename `N` as `AsyncReadWrite` to describe usage.
* use `Framed` to encode/decode MQTT packets.
* use `Login` to store credentials
* Made `DisconnectProperties` struct public.
* Replace `Vec<Option<u16>>` with `FixedBitSet` for managing packet ids of released QoS 2 publishes and incoming QoS 2 publishes in `MqttState`.
* Accept `native_tls::TlsConnector` as input for `Transport::tls_with_config`.
* Update `thiserror` to `2.0.8`, `tokio-rustls` to `0.26.0`, `rustls-webpki` to `0.102.8`, `rustls-pemfile` to `2.2.0`, `rustls-native-certs` to `0.8.1`, `async-tungstenite` to `0.28.0`, `ws_stream_tungstenite` to `0.14.0`, `native-tls` to `0.2.12` and `tokio-stream` to `0.1.16`.
* Make error types returned by `rumqttc::v5::Connection` public

### Deprecated

### Removed

### Fixed

* Validate filters while creating subscription requests.
* Make v4::Connect::write return correct value
* Ordering of `State.events` related to `QoS > 0` publishes
* Filter PUBACK in pending save requests to fix unexpected PUBACK sent to reconnected broker.
* Resume session only if broker sends `CONNACK` with `session_present == 1`.
* Remove v5 PubAck/PubRec/PubRel/PubComp/Sub/Unsub failures from `StateError` and log warnings on these failures.
* MQTTv5: Allow keep alive values in `0..=65535` seconds (including `0`)

### Security

---

## [rumqttc 0.24.0] - 27-02-2024

### Added
- Expose `EventLoop::clean` to allow triggering shutdown and subsequent storage of pending requests
- Support for all variants of TLS key formats currently supported by Rustls: `PKCS#1`, `PKCS#8`, `RFC5915`. In practice we should now support all RSA keys and ECC keys in `DER` and `SEC1` encoding. Previously only `PKCS#1` and `PKCS#8` where supported.
- TLS Error variants: `NoValidClientCertInChain`, `NoValidKeyInChain`.
- Drain `Request`s, which weren't received by eventloop, from channel and put them in pending while doing cleanup to prevent data loss.
- websocket request modifier for v4 client
- Surfaced `AsyncClient`'s `from_senders` method to the `Client` as `from_sender`

### Changed
- `MqttOptions::new` now accepts empty client id.
- `MqttOptions::set_clean_session` now panics if client ID is empty and `clean_session` flag is set to false.
- Synchronous client methods take `&self` instead of `&mut self` (#646)
- Removed the `Key` enum: users do not need to specify the TLS key variant in the `TlsConfiguration` anymore, this is inferred automatically.
To update your code simply remove `Key::ECC()` or `Key::RSA()` from the initialization.
- certificate for client authentication is now optional while using native-tls. `der` & `password` fields are replaced by `client_auth`.
- Make v5 `RetainForwardRule` public, in order to allow setting it when constructing `Filter` values.
- Use `VecDeque` instead of `IntoIter` to fix unintentional drop of pending requests on `EventLoop::clean` (#780)
- `StateError::IncommingPacketTooLarge` is now `StateError::IncomingPacketTooLarge`.
- Update `tokio-rustls` to `0.25.0`, `rustls-native-certs` to `0.7.0`, `rustls-webpki` to `0.102.1`,
  `rusttls-pemfile` to `2.0.0`, `async-tungstenite` to `0.24.0`, `ws_stream_tungstenite` to `0.12.0`
  and `http` to `1.0.0`. This is a breaking change as types from some of these crates are part of
  the public API.

### Deprecated

### Removed

### Fixed
- Lowered the MSRV to 1.64.0
- Request modifier function should be Send and Sync and removed unnecessary Box

### Security

---

## [rumqttc 0.23.0] - 10-10-2023

### Added
- Added `bind_device` to `NetworkOptions` to enable `TCPSocket.bind_device()`
- Added `MqttOptions::set_request_modifier` for setting a handler for modifying a websocket request before sending it.

### Changed

### Deprecated

### Removed

### Fixed
- Allow keep alive values <= 5 seconds (#643)
- Verify "mqtt" is present in websocket subprotocol header.

### Security
- Remove dependency on webpki. [CVE](https://rustsec.org/advisories/RUSTSEC-2023-0052)
- Removed dependency vulnerability, see [rustsec](https://rustsec.org/advisories/RUSTSEC-2023-0065). Update of `tungstenite` dependency.

---

## [rumqttc 0.22.0] - 07-06-2023

### Added
- Added `outgoing_inflight_upper_limit` to MQTT5 `MqttOptions`. This sets the upper bound for the number of outgoing publish messages (#615)
- Added support for HTTP(s) proxy (#608)
    - Added `proxy` feature gate
    - Refactored `eventloop::network_connect` to allow setting proxy
    - Added proxy options to `MqttOptions`
- Update `rustls` to `0.21` and `tokio-rustls` to `0.24` (#606)
    - Adds support for TLS certificates containing IP addresses
    - Adds support for RFC8446 C.4 client tracking prevention

### Changed
- `MqttState::new` takes `max_outgoing_packet_size` which was set in `MqttOptions` but not used (#622)

### Deprecated

### Removed

### Fixed
- Enforce `max_outgoing_packet_size` on v4 client (#622)

### Security

## [rumqttc 0.21.0] - 01-05-2023

### Added
- Added support for MQTT5 features to v5 client
    - Refactored v5::mqttbytes to use associated functions & include properties
    - Added new API's on v5 client for properties, eg `publish_with_props` etc
    - Refactored `MqttOptions` to use `ConnectProperties` for some fields
    - Other minor changes for MQTT5

### Changed
- Remove `Box` on `Event::Incoming`

### Deprecated

### Removed
- Removed dependency on pollster

### Fixed
- Fixed v5::mqttbytes `Connect` packet returning wrong size on `write()`
    - Added tests for packet length for all v5 packets

### Security


## [rumqttc 0.20.0] - 17-01-2023

### Added
- `NetworkOptions` added to provide a way to configure low level network configurations (#545)

### Changed
- `options` in `Eventloop` now is called `mqtt_options` (#545)
- `ConnectionError` now has specific variant for type of `Timeout`, `FlushTimeout` and `NetworkTimeout` instead of generic `Timeout` for both (#545)
- `conn_timeout` is moved into `NetworkOptions` (#545)

---

Old changelog entries can be found at [CHANGELOG.md](../CHANGELOG.md)
