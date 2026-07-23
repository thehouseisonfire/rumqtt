## [Unreleased]

### Added
- `rumqttc` v4/v5: Add first-party SOCKS5 proxy support, including proxy-side
  DNS resolution and RFC 1929 username/password authentication. Add independent
  `http-proxy` and `socks-proxy` features while retaining `proxy` as an umbrella
  that enables both.
- `rumqttc` v4/v5: Add Linux-only `NetworkOptions::set_mptcp(...)` for opt-in
  Multipath TCP connections, with regular TCP fallback when the local kernel
  reports MPTCP as unavailable or disabled.
### Changed
- `rumqttc` v4/v5: Consolidate the optional file-backed session adapters into
  `rumqttc-session-store-file-next`, with independent `v4` and `v5` features.
- `rumqttc` v4/v5 (Breaking Change): Replace public-field `Proxy`, `ProxyType`,
  and `ProxyAuth` configuration with `Proxy::http(...)`, `Proxy::https(...)`,
  `Proxy::socks5(...)`, and `.with_credentials(...)`; proxy debug output now
  redacts passwords.
- `rumqttc` v4/v5: Remove the redundant flush-only checkpoint while keeping the uninterrupted
  first wire QoS 1/2 PUBLISH at `DUP=0` and storing/restoring its recovery representation with
  `DUP=1`. This reduces save count and submitted checkpoint bytes. A crash after the admission
  checkpoint but before transport submission can make the broker's first observed copy carry
  `DUP=1`; this is a documented conservative recovery trade-off, and strict conformance is not
  claimed for that exact first-transmission edge case.
### Deprecated
### Removed
- `rumqttc` v4/v5 (Breaking Change): Remove the obsolete outgoing publish flush-attempt
  diagnostic field and the v5 `MqttState::mark_outgoing_publishes_flush_attempted()` method;
  reconnect cleanup now always prepares admitted QoS 1/2 publishes as retransmissions.
### Fixed
- `mqttbytes-core` and `rumqttc` v4/v5: Remove avoidable library panic paths
  from fixed-header/filter helpers, WebSocket subprotocol setup, and TLS backend
  dispatch; invalid TLS backend combinations now return
  `UnsupportedBackendConfiguration`.
### Security

---

## [rumqttc-next 0.34.0-alpha] - 19-07-2026


### Added
- `rumqttc` v4/v5: Add opt-in `tracing` lifecycle events for connection attempts,
  establishment and loss, session restoration, replay preparation, and typed
  protocol violations. Add a separate `tracing-log-compat` feature for textual
  fallback through `log` when no tracing subscriber is active.
- `rumqttc` v4/v5: Add an opt-in `stream` feature with `EventLoop::into_stream()` for driving async event loops through `futures_core::Stream`.
- `rumqttc` v4/v5: Add structured runtime diagnostics via `EventLoop::diagnostics()` and
  `MqttState::outbound_diagnostics()` for observing connection, queue, session, batching, and outbound protocol state.
- `rumqttc` v4/v5: Add fallible TLS defaults, MQTT option validation, fallible option/client builders, and v4 fallible setters for configuration paths that can fail before connecting.
- `rumqttc` v4/v5: Add `ValidatedTopicFilter`, `TopicFilter`, and `SubscribeFilterInput` for reusable validated MQTT topic filters in subscribe and unsubscribe APIs, plus high-level multi-unsubscribe methods.
- `rumqttc` v4: Add `is_mqtt_minimum_client_id(...)`, an advisory helper for checking whether a ClientId fits the MQTT 3.1.1 1-23 byte ASCII alphanumeric profile that every compliant server must accept.
- `rumqttc` v4: Add opt-in client-side persistent session storage via `SessionStore`, `SessionStoreKey`,
  `PersistedSession`, `PersistedSession::encode`/`decode`, `MqttOptions::set_session_store(...)`,
  `MqttOptions::set_session_store_scope(...)`, and builder `.session_store(...)`. When configured with
  `clean_session(false)`, the event loop can restore local QoS/session state across newly constructed clients or
  process restarts, including in-flight publishes, `PUBREL`, `SUBSCRIBE`, and `UNSUBSCRIBE` requests. This is a
  storage API and backend-neutral checkpoint model; applications provide durable storage layout, and rumqttc does
  not include a built-in file store. `SessionStore` persists MQTT protocol recovery state, not a durable application
  outbox for every submitted request. Exactly one active event loop may own a session-store key at a time.
- `rumqttc` v5: Add `MqttStateBuilder::client_topic_alias_max(u16)` builder method and `MqttState::set_client_topic_alias_max(Option<u16>)` to configure the incoming Topic Alias Maximum, propagated from `MqttOptions::topic_alias_max()` at connect time.
- `rumqttc` v5: Add opt-in client-side persistent session storage via `SessionStore`, `SessionStoreKey`,
  `PersistedSession`, `PersistedSession::encode`/`decode`, `MqttOptions::set_session_store(...)`,
  `MqttOptions::set_session_store_scope(...)`, and builder `.session_store(...)`. When configured with
  `clean_start(false)`, the event loop can restore local QoS/session state across newly constructed clients or
  process restarts, including in-flight publishes, `PUBREL`, `SUBSCRIBE`, and `UNSUBSCRIBE` requests. This is a
  storage API and backend-neutral checkpoint model; applications provide durable storage layout, and rumqttc does
  not include a built-in file store. `SessionStore` persists MQTT protocol recovery state, not a durable application
  outbox for every submitted request. Exactly one active event loop may own a session-store key at a time.
### Changed
- `rumqttc` v4/v5: Clarify that graceful disconnect uses the control-request lane,
  drains protocol-admitted work rather than every channel-accepted request, and
  that `disconnect_now()` is priority-scheduled rather than preemptive.
- `rumqttc-core` (Breaking Change): Change `validate_response_headers(...)` to borrow its WebSocket response instead of consuming it.
- `rumqttc` v5 (Breaking Change): Flatten redundant `mqttbytes::v5` module nesting. `PacketType`, `FixedHeader`, `Error`, `check()`, `qos()`, and codec helper functions now live at `mqttbytes::` instead of `mqttbytes::v5::`, matching the v4 crate's layout. `mqttbytes::v5::` still exists and re-exports packet-level types (`Packet`, `Connect`, `Publish`, etc.).
- `rumqttc` v4/v5 (Breaking Change): Normalize `PingReq`/`PingResp` enum variants to unit variants in both `Packet` and `Request` enums across both crates. `Packet::PingReq`, `Packet::PingResp`, `Request::PingReq`, and `Request::PingResp` are now unit variants consistently in v4 and v5. The `PingReq` and `PingResp` structs are preserved for codec use.
- `rumqttc` v5 (Breaking Change): Rename `Filter` to `SubscribeFilter` for parity with the v4 crate.
- `rumqttc` v4/v5 (Breaking Change): Unify publish API with `PublishOptions` and `IntoPublishPayload`. Replace scattered publish method variants (`publish`, `publish_bytes`, `publish_with_properties`, `publish_bytes_with_properties` and their tracked/`try_` counterparts) with a single set of publish methods that accept a `PublishOptions` struct and a generic payload implementing `IntoPublishPayload`. `PublishOptions` is a builder-style struct that bundles QoS, retain flag, and optional `PublishProperties`; use `.retained()` for the constant retained case or `.retain(bool)` for dynamic retain policies. `IntoPublishPayload` allows callers to pass `&str`, `&[u8]`, `[u8; N]`, `Bytes`, `Vec<u8>`, and `String` directly.
- `rumqttc` v4 (Breaking Change): Make `EventLoop::network` private for v5 parity, and so downstream code can no longer access the live transport and inject arbitrary MQTT packets, including a second `CONNECT`, on an existing connection.
- `rumqttc` v4 (Breaking Change): Remove the public `Protocol` enum and `Connect::protocol` field. The v4 CONNECT codec now always emits MQTT protocol level `0x04` and rejects other protocol levels on decode.
- `rumqttc` v4 (Breaking Change): Add state-level `ProtocolViolation`, remove `StateError::WrongPacket`, and report duplicate or post-handshake invalid inbound packets as protocol-state errors instead of codec format errors.
- `rumqttc` v5 (Breaking Change): Add state-level `ProtocolViolation`, remove `StateError::WrongPacket`, report duplicate or post-handshake invalid inbound packets as protocol-state errors, and avoid queueing rejected inbound packets as successful events.
- `mqttbytes-core` (Breaking Change): Change `write_mqtt_bytes(...)` and `write_mqtt_string(...)` to return `Result<(), Error>` so oversized MQTT two-byte length-prefixed fields report `Error::PayloadTooLong` instead of panicking.
- `rumqttc` v5 (Breaking Change): Validate incoming topic aliases against the client's advertised Topic Alias Maximum per [MQTT-3.1.2-26]/[MQTT-3.1.2-27]. Servers sending aliases exceeding the limit (or any alias when the maximum is 0/absent) now trigger a `DISCONNECT(TopicAliasInvalid)` and close the connection. Previously, any incoming alias was accepted without validation.
- `rumqttc` v4/v5 (Breaking Change): Replace boolean manual acknowledgement configuration with `AckMode::{Automatic, Manual}` via `MqttOptions::set_ack_mode(...)`, `MqttOptions::ack_mode()`, and builder `.ack_mode(...)`.
- `rumqttc` v5 (Breaking Change): Replace broker-only session resume and automatic topic-alias booleans with explicit policies: `BrokerSessionResumePolicy::{Strict, AllowBrokerOnly}` and `TopicAliasPolicy::{Disabled, Monotonic, Lru}`.
- `rumqttc` v4/v5 (Breaking Change): Replace generic `ClientError::Request` and
  `ClientError::TryRequest` with `RequestChannelFull`, `RequestChannelDisconnected`, and
  `InvalidRequest` so callers can distinguish backpressure, closed event loops, and locally rejected
  MQTT requests while still recovering the original request.
- `rumqttc` v4/v5: Clarify that `AsyncClient::from_senders(...)` and `Client::from_sender(...)`
  create low-level Flume request-sink clients without tracked operations, priority routing, or
  automatic event-loop and protocol behavior.
### Deprecated
### Removed
- `rumqttc` v4/v5 (Breaking Change): Remove public conversions from
  `flume::SendError<Request>` and `flume::TrySendError<Request>` to `ClientError`. Client request-channel failures
  continue to be reported through crate-owned `ClientError` variants.
### Fixed
- `rumqttc` v5: Record a graceful client DISCONNECT Session Expiry Interval override in the persisted
  checkpoint, preserve it across transient post-flush store failures, and clear stale checkpoints after
  abrupt connection loss or graceful-disconnect timeout when the effective expiry is zero. Nonzero-expiry
  disconnect timeouts preserve complete replay state before replacing the durable checkpoint.
- `rumqttc` v5: Keep the application-configured CONNECT Session Expiry Interval as the reconnect baseline
  after a broker CONNACK override while tracking the broker value separately for current-session persistence.
- `rumqttc` v4/v5: Return a TLS error from fallible rustls default configuration helpers when no `CryptoProvider` is available, instead of panicking in `ClientConfig::builder()`.
- `rumqttc` v5: Complete tracked publish/subscribe/unsubscribe notices only after updated persistent session state is saved. If the configured `SessionStore` fails at this durability barrier, notices now report `SessionPersistence(...)` errors instead of success.
- `rumqttc` v4/v5: Include the underlying WebSocket response validation reason in
  `ConnectionError::ResponseValidation` display output.
- `rumqttc` v5: Enforce CONNACK `Retain Available = 0` per MQTT-3.2.2-14 by rejecting retained outbound publishes, including reconnect replays, without dropping the active connection; tracked publishes report `PublishNoticeError::RetainNotSupported`.
- `rumqttc` v4/v5: Fix graceful disconnect after subscribe/unsubscribe packet-id gaps so completed publishes do not leave stale outbound drain tracking and prevent MQTT `DISCONNECT`.
- `rumqttc` v4/v5: Reset retained local session state before reconnecting with a changed ClientId so pending state from one MQTT identity is not reused under another.
- `rumqttc` v4/v5 codecs: Return `PayloadTooLong` instead of panicking when encoding MQTT strings or binary fields that exceed the MQTT two-byte length prefix limit.
- `rumqttc` v4/v5 codecs: Enforce MQTT UTF-8 string validation on read/write paths, rejecting malformed UTF-8 and U+0000 in MQTT strings, including publish topics, will topics, subscribe filters, and unsubscribe filters.
- `rumqttc` v4/v5: Prevent packet identifier reuse across publish, `PUBREL`, subscribe, and unsubscribe flows, returning state errors instead of silently colliding identifiers.
- `rumqttc` v4/v5: Reject zero packet identifiers and unsolicited ACK/`PUBREL` packets in codec/state handling, including tracked `SUBACK`/`UNSUBACK` and manual `PUBACK`/`PUBREC` acknowledgement flows.
- `rumqttc` v4: Reset the keepalive timeout after inbound traffic that produces response writes, QoS 0 flushes, and graceful disconnect draining, preventing premature keepalive-triggered disconnects on otherwise active connections.
- `rumqttc` v5: Reset the keepalive timeout after outbound request flushes and automatic protocol responses, preventing unnecessary `PINGREQ`s while other MQTT Control Packets keep the connection active.
- `rumqttc` v5 codecs: Reject `RequestResponseInformation` and `RequestProblemInformation` CONNECT property values > 1 and duplicate instances per [MQTT-3.1.2-28]/[MQTT-3.1.2-29], returning `Error::ProtocolError` instead of silently accepting or encoding spec-invalid values.
- `rumqttc` v5: Reject CONNACK `Response Information` when `Request Response Information` is 0 or absent per [MQTT-3.1.2-28], sending `DISCONNECT(ProtocolError)` instead of silently accepting the spec-violating server response.
- `rumqttc` v4: Panic in `MqttOptions::set_client_id(...)` when `client_id` is empty and `clean_session` is false (MQTT-3.1.3-7), closing the gap left by the 0.24.0 `set_clean_session` panic that only covered the reverse call order.
- `rumqttc` v4: Return `Error::IncorrectPacketFormat` from `Connect::write(...)` when `client_id` is empty and `clean_session` is false (MQTT-3.1.3-7), instead of silently encoding a spec-invalid CONNECT packet.
- `rumqttc` v5: Handle the CONNACK `AssignedClientIdentifier` property per MQTT-3.2.2-10. When the client sent an empty ClientId, adopt the server-assigned value into `MqttOptions::client_id` and reuse it on reconnect; reject successful CONNACK that omits or sends an empty assignment after an empty ClientId; reject the property when the client sent an explicit ClientId. Violations send `DISCONNECT(ProtocolError)` and return an error.
- `rumqttc` v5: Reject duplicate `AssignedClientIdentifier` CONNACK properties as `ProtocolError` (MQTT-3.2.2-10), instead of silently overwriting the first value.
- `rumqttc` v5: Reject outgoing `PublishProperties` with topic alias 0, returning `Error::ProtocolViolation(TopicAliasInvalid)` instead of silently encoding an invalid alias.
- `rumqttc` v5: Fix `check()` to compare total packet size (including fixed header) against `max_packet_size` instead of only the remaining length, matching the MQTT v5 spec definition of Maximum Packet Size.
- `rumqttc` v5: Fix DISCONNECT encoding for non-normal reason codes without properties to include an explicit zero Property Length byte, instead of omitting it. Only `NormalDisconnection` without properties may use the compact 2-byte form per the MQTT v5 spec.

### Security

---

## [rumqttc-next 0.33.2] - 23-05-2026


### Added
### Changed
### Deprecated
### Removed
### Fixed
- `rumqttc` v4/v5: Fix graceful disconnect drain handling so queued but unsent flow-controlled publishes do not prevent MQTT `DISCONNECT` after actual outbound protocol state has completed.
### Security

---

## [rumqttc-next 0.33.1] - 16-05-2026


### Added
### Changed
### Deprecated
### Removed
### Fixed
- `rumqttc` v4/v5: Fix Issue #6 - `disconnect_with_properties_timeout()` times out although all visible outbound QoS state is completed
### Security

---

## [rumqttc-next 0.33.0] - 03-05-2026


### Added
### Changed
- `rumqttc` v4/v5: Replace broad request-channel blocking under publish flow-control pressure with protocol-aware outbound scheduling. QoS 1/ QoS 2 publishes preserve publish ordering and wait for inflight quota plus packet-id availability, QoS 0 publishes do not bypass earlier blocked publishes, and non-`PUBLISH` control packets may pass blocked QoS 1/ QoS 2 publishes when protocol-valid.
- `rumqttc` v4/v5: Move `disconnect_now()` / `try_disconnect_now()` to a dedicated immediate shutdown channel so immediate disconnect is not blocked behind queued application work or a full application request channel.
### Deprecated
### Removed
### Fixed
- `rumqttc` v4/v5: Avoid entering packet-id collision timeout for unsent publishes. Queued publishes now wait until the next candidate packet id is free, while keepalive and eligible control traffic can continue.
### Security

---

## [rumqttc-next 0.32.0] - 03-05-2026


### Added
- `rumqttc` v5: Add opt-in automatic client-side topic alias management via `MqttOptions::set_auto_topic_aliases(...)` / builder `.auto_topic_aliases(...)`, assigning broker-supported aliases immediately before publish while preserving reconnect and replay safety.
- `rumqttc` v5: Add `TopicAliasPolicy::Lru` for opt-in automatic topic alias recycling once broker-supported aliases are exhausted.
- `rumqttc` v5: Add first-class MQTT 5 enhanced-authentication lifecycle events and tracked re-authentication notices. `Event::Auth(AuthEvent)` reports authentication start/continue/success/failure, and `reauth_tracked` / `try_reauth_tracked` return `AuthNotice` handles that resolve to structured `AuthOutcome` / `AuthNoticeError` results.
- `rumqttc` v5: Add the context-aware `Authenticator` API for enhanced authentication callbacks, including exchange kind (`InitialConnect` vs `Reauthentication`) and explicit success/failure notifications.
- `rumqttc` v5: Add `MqttState::set_authentication_method()` for low-level MQTT 5 enhanced-authentication users that process AUTH packets through `MqttState` directly.
- `rumqttc` v4/v5: Add `MqttStateBuilder` for fluent low-level `MqttState` construction via `MqttState::builder(max_inflight).manual_acks(...).build()` (v5 also exposes `.authentication_method(...)` and `.auth_manager(...)`).
### Changed
- `rumqttc` v5 (Breaking Change): Replace the packet-shaped `AuthManager` abstraction with `Authenticator`, which receives authentication context, can start initial and re-auth exchanges, and is notified when an exchange succeeds, fails, or is aborted.
- `rumqttc` v5: Manage enhanced authentication as an explicit client-side lifecycle. The client now tracks initial CONNECT authentication separately from post-CONNACK re-authentication, prevents overlapping re-auth attempts, correlates `AUTH Success` with the active exchange, and resolves active re-auth notices on disconnect, session reset, connection failure, or protocol/authentication failure.
- `rumqttc` v4/v5 (Breaking Change): Change `disconnect()`/`try_disconnect()` into graceful barriers that flush previously accepted QoS 0 publishes and drain previously accepted QoS 1/ QoS 2 publish plus tracked subscribe/unsubscribe state (`SUBACK`/`UNSUBACK`) before sending terminal DISCONNECT. Use `disconnect_now()`/`try_disconnect_now()` to preserve immediate DISCONNECT behavior, or `disconnect_with_timeout()`/`try_disconnect_with_timeout()` to bound graceful draining; if the timeout expires first, polling returns `ConnectionError::DisconnectTimeout` and DISCONNECT is not sent.
- `rumqttc` v4/v5 (Breaking Change): Split the client builder API into `ClientBuilder` and `AsyncClientBuilder`. `AsyncClient::builder(...)` now returns `AsyncClientBuilder` and uses `.build()` to produce `(AsyncClient, EventLoop)`.
### Deprecated
### Removed
- `rumqttc` v5 (Breaking Change): Remove the public `AuthManager` API in favor of `Authenticator`.
- `rumqttc` v4/v5 (Breaking Change): Remove `AsyncClientBuilder::build_async()` in favor of `AsyncClientBuilder::build()`.
- `rumqttc` v4/v5 (Breaking Change): Remove public `MqttState::new()` / `MqttState::new_with_auth_method()` constructors in favor of `MqttState::builder(max_inflight)`.
### Fixed
- `rumqttc` v5: Accept zero-length MQTT v5 DISCONNECT packets in the packet reader.
- `rumqttc` v5: Validate CONNACK `Authentication Method` against the CONNECT `Authentication Method` (MQTT 5 §3.2.2.3.5); send `ProtocolError` DISCONNECT on mismatch or unexpected presence.
- `rumqttc` v5: Validate incoming AUTH `Authentication Method` against the CONNECT method; reject server-sent `ReAuthenticate` as `ProtocolError` (§3.15.2.1); normalize outgoing AUTH method (auto-fill from CONNECT, reject mismatch), and reject unsolicited or methodless `AUTH Success` packets.
- `rumqttc` v5: Avoid panics and invalid client output for AUTH packets without properties. Client-initiated re-authentication now synthesizes the configured CONNECT `Authentication Method` when possible and fails locally when no CONNECT method is available.
### Security

---

## [rumqttc-next 0.31.0] - 30-04-2026

### Added
- `rumqttc` v4/v5: Add `ClientBuilder` via `Client::builder(...)` and `AsyncClient::builder(...)`, with bounded default capacity from `MqttOptions::request_channel_capacity()`, `.capacity(n)` overrides, and explicit `.unbounded()` request-channel opt-in.
- `rumqttc` v4/v5: Add ACK-returning tracked notices. `publish_tracked` resolves to a crate-local `PublishResult`, `subscribe_tracked` resolves to `SubAck`, and `unsubscribe_tracked` resolves to `UnsubAck`. ACK packets are cloned into notices while remaining visible through `Event::Incoming(...)`.
- `rumqttc` v4/v5: Add operation-specific `SubscribeNotice` and `UnsubscribeNotice` types plus `wait_completion()` / `wait_completion_async()` helpers for callers that only need completion-style behavior.
### Changed
- `rumqttc` v4/v5 (Breaking Change): Change `PublishNotice::wait()` / `wait_async()` to return detailed publish protocol results instead of `Result<(), PublishNoticeError>`. Broker ACK packets with failure/rejection reason codes are returned as protocol results; notice errors are reserved for local lifecycle failures.
- `rumqttc` v4/v5 (Breaking Change): Change tracked subscribe/unsubscribe APIs to return operation-specific detailed notices instead of the broad completion-only `RequestNotice`.
### Deprecated
### Removed
- `rumqttc` v4/v5 (Breaking Change): Remove `Client::new(options, cap)` and `AsyncClient::new(options, cap)` in favor of the builder API.
- `rumqttc` v4/v5 (Breaking Change): Remove the public `RequestNotice` / `RequestNoticeError` API in favor of `SubscribeNotice` / `SubscribeNoticeError` and `UnsubscribeNotice` / `UnsubscribeNoticeError`.
### Fixed
### Security

---

## [rumqttc-next 0.30.1] - 30-04-2026

### Added
- `rumqttc` v5: `PublishNoticeError::TopicAliasReplayUnavailable(alias)` for unreplayable alias-only publishes after reconnect.
### Changed
### Deprecated
### Removed
### Fixed
- `rumqttc` v5: Scope topic-alias mappings to the network connection lifetime (MQTT 5 §3.3.2.3.4). Split `topic_alises` into `incoming_topic_aliases`/`outgoing_topic_aliases`, clear both on CONNACK/clean/reset. Drop unreplayable alias-only publishes with `TopicAliasReplayUnavailable` instead of sending empty topics. DISCONNECT with `ProtocolError` on unknown incoming aliases. This change roughly maps to upstream [PR #1040](https://github.com/bytebeamio/rumqtt/pull/1040).
- `rumqttc` v4/v5 eventloops: Reuse a persistent never-firing placeholder sleep when keepalive is disabled, avoiding per-iteration zero-duration timer setup inside `select!`.
- `rumqttc` v5 codecs/network read path: Enforce MQTT 5 forbidden-zero property values for SUBSCRIBE `Subscription Identifier` (§3.8.2.1.2), PUBLISH `Topic Alias` (§3.3.2.3.4), and CONNECT/CONNACK `Receive Maximum` (§3.1.2.11.3, §3.2.2.3.3); preserve protocol-violation reason codes so read-side DISCONNECT uses the precise MQTT 5 reason. This change roughly maps to upstream [PR #1038](https://github.com/bytebeamio/rumqtt/pull/1038).
### Security

---

## [rumqttc-next 0.30.0] - 16-04-2026

### Added
- `rumqttc` v4/v5: Add `NetworkOptions::set_bind_addr(SocketAddr)` to bind outgoing TCP sockets to a specific local address before connect.
- `rumqttc` v4/v5: Add `TlsConfiguration::simple_native(...)` for native-tls client configuration, including secure websocket transports.
- `rumqttc` v4/v5: Add `MqttOptions::websocket_with_tls_config(...)` and `MqttOptions::try_websocket_with_default_tls(...)` so secure websocket clients can be created directly from `wss://` endpoints.
### Changed
- `rumqttc`: Bump workspace MSRV from Rust `1.85` to `1.89` and inherit `rust-version` from the workspace manifest so member crates stay aligned.
### Deprecated
### Removed
### Fixed
- `rumqttc` v4/v5: Add `SessionStateMismatch` error and `reconcile_connack_session()` validation to reject broker replies where `session_present` contradicts the client's `clean_session`/`clean_start` setting.
- `rumqttc` v4/v5: Default multi-address TCP dialing now starts resolved connection attempts with a small stagger, so a stalled first route no longer prevents later resolved addresses from being tried within the outer connect timeout.
- `rumqttc` v4/v5: Clarify that `NetworkOptions::set_bind_addr(...:fixed_port)` trades away same-family staggered fallback. With a fixed local port, the default dialer keeps one active candidate at a time until it completes or the overall connect timeout expires.
- `rumqttc` v4/v5: Secure websocket transport now defaults an implicit websocket URL port to `443` when `Transport::Wss(...)` is selected, while preserving explicit ports.
- `rumqttc` v5: Classify inbound malformed/protocol-invalid decode failures more precisely and attempt the corresponding MQTT 5 `DISCONNECT` reason code (`0x81`, `0x82`, `0x95`) before terminating. The outbound `DISCONNECT` is best-effort under write-side backpressure so protocol-error handling does not hang waiting for a non-reading peer.
### Security
- `rumqttc` v5 `auth-scram`: switch SCRAM backend dependency from `scram-2` to the maintained `scram-rs` crate.
- Address security audits from `cargo audit` related to `rustls-webpki` (`RUSTSEC-2026-0098`, `RUSTSEC-2026-0099`) and `rand` (`RUSTSEC-2026-0097`).

---

## [rumqttc-next 0.29.0] - 27-03-2026

### Added
- `rumqttc`: Add `Broker`-based first-class Unix domain socket and websocket construction, plus `unix:///...` URL parsing on Unix targets.
### Changed
- `rumqttc`: Secure endpoint schemes are no longer implicit transport selectors. Configure `mqtts://` / `ssl://` and secure websockets explicitly with `MqttOptions::set_transport(...)`; `Broker::websocket(...)` now accepts only `ws://...`.
- `rumqttc`: Refactor CONNECT authentication to version-specific public `ConnectAuth` enums and make auth field presence explicit instead of inferring it from empty strings or bytes.
  `MqttOptions` now stores/exposes auth via `set_auth(...)`, `auth()`, `clear_auth()`, `set_username(...)`, and `set_credentials(...)`.
  MQTT 5 also adds `set_password(...)` because password-only CONNECT auth is spec-valid there, while MQTT 3.1.1 keeps the stricter username/password dependency.
  CONNECT passwords continue to use `bytes::Bytes` / `Into<Bytes>` and now round-trip correctly as MQTT binary data, including embedded `NUL` bytes, non-UTF-8 bytes, and explicitly empty passwords.
  This is a user-visible API break: the old `Option<Login>` / `credentials()` shape has been replaced by `ConnectAuth`.
### Deprecated
### Removed
### Fixed
- `rumqttc`: Avoid eager default TLS/WSS initialization during option construction so manual-provider/custom-TLS setups under `use-rustls-no-provider` no longer fail before `set_transport(...)` can be applied.
### Security

---

## [rumqttc-next 0.28.0] - 23-03-2026

### Added
- `rumqttc` v4/v5: Add `set_read_batch_size(usize)` and `read_batch_size()` on `MqttOptions` to configure network read batching; default of `0` enables adaptive batching based on inflight/pending load.
- `rumqttc` v4/v5: Add `set_max_request_batch(usize)` and `max_request_batch()` on `MqttOptions` to control how many queued requests are processed per eventloop iteration (higher values can improve throughput by batching writes/flushes).
- `rumqttc` v4/v5 websocket transport: Add `set_fallible_request_modifier(...)` on `MqttOptions` to support request modifiers that return `Result<http::Request<()>, E>`; errors now surface as `ConnectionError::RequestModifier`.
- `rumqttc` v4/v5 async clients: Add opt-in tracked publish APIs (`publish_tracked` variants) that return `PublishNotice` and resolve on QoS milestones (`flush` for QoS0, `PUBACK` for QoS1, `PUBCOMP` for QoS2).
- `rumqttc` v4/v5 eventloops: Add public `EventLoop::reset_session_state()` and `EventLoop::drain_pending_as_failed(reason) -> usize` plus new `NoticeFailureReason` for controlled pending/session failure handling.
- `rumqttc` v4/v5 state: Add tracked request queue helpers `tracked_subscribe_len`, `tracked_unsubscribe_len`, `tracked_requests_is_empty`, and `drain_tracked_requests_as_failed(reason) -> usize`.
### Changed
- Split the client package into protocol-specific crates: `rumqttc-v4` (MQTT 3.1.1) and `rumqttc-v5` (MQTT 5), and removed the combined `rumqttc-next` package.
- Updated workspace tooling, CI, and benchmark dependencies to target the new protocol crates directly.
- `rumqttc` v5: Change connect timeout API from seconds-based `connection_timeout()`/`set_connection_timeout(u64)` to `Duration`-based `connect_timeout()`/`set_connect_timeout(Duration)`, and update internal connect timeout handling accordingly.
- `rumqttc` v4: Change `mqttbytes::v4::Publish.topic` from `String` to `bytes::Bytes`, reducing topic allocation/copy overhead in packet decode/encode paths (topic UTF-8 validation is still enforced).
- `rumqttc` v4/v5: Replace publish API bound `Topic` with `Into<PublishTopic>`, restoring support for common string inputs like `&String` and `Cow<'_, str>` while preserving `ValidatedTopic` fast-path behavior.
### Deprecated
### Removed
- `rumqttc` v4: Remove `Outgoing::Auth` from MQTT 3.1.1 outgoing events.
- `rumqttc-core`: Remove `Transport`; use `rumqttc-v4::Transport` for MQTT 3.1.1 and
  `rumqttc-v5::Transport` for MQTT 5.
### Fixed
- `rumqttc` v4/v5: Avoid panicking when applying TCP socket send/recv buffer sizes; these configuration failures now return an error from connect setup.
- `rumqttc` v4/v5: Restore cross-crate rustls provider isolation so mixed builds (`rumqttc-v4/use-rustls-ring` with `rumqttc-v5/use-rustls-aws-lc`, and vice versa) compile successfully.
- `rumqttc` v4/v5: Fix mixed-backend builds so WSS honors the selected `TlsConfiguration` backend and no longer fails when one crate uses rustls while the other enables native-tls.
- `rumqttc` v4/v5: Make `Transport::{tls_with_default_config,wss_with_default_config}` choose defaults from each crate's enabled TLS features instead of leaking shared-core defaults across crates.
- `rumqttc` transport core: Remove ambiguous dual-backend `TlsConfiguration::default()` behavior; mixed-backend callers now select backend explicitly via `default_rustls`/`default_native` or explicit `TlsConfiguration` variants.
- `rumqttc` v4: Reject client-side publish requests with empty topic names (MQTT 3.1.1 Topic Name must be at least one character).
- `rumqttc` v4 codec: Enforce strict MQTT 3.1.1 fixed-header flag validation while decoding packets (invalid reserved/required flag patterns are now rejected).
- `rumqttc` v4 CONNECT/CONNACK: Enforce stricter 3.1.1 packet validation (CONNECT reserved/password-username flag rules and CONNACK remaining-length/session-present flag rules).
- `rumqttc` v4 PUBLISH: Reject packets with empty Topic Name or wildcard characters in Topic Name during decode/encode.
- `rumqttc` v4 ACK codecs: Enforce strict decode/encode validation for PUBACK/PUBREC/PUBREL/PUBCOMP (`remaining_len == 2`, non-zero packet identifier).
- `rumqttc` v4 SUBSCRIBE: Reject payload entries with non-zero reserved subscription option bits during decode.
- `rumqttc` v4 PUBLISH: Reject QoS0 packets with DUP set during decode/encode.
- `rumqttc` v5: Reject client-side publish requests with an empty topic unless `PublishProperties.topic_alias` is present and non-zero.
### Security
- `rumqttc` `auth-scram`: switch SCRAM backend dependency from `scram` to `scram-2`, removing `ring` `<0.17` from that feature path.
- `cargo-audit`: add temporary ignore for `RUSTSEC-2026-0009` in `.cargo/audit.toml`; advisory is currently reachable via `rcgen` dev-dependency path and fixed `time` requires Rust `1.88+` (above current MSRV `1.85`).

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
