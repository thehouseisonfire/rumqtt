# Native Wrapper-Core Feature Parity

## Goal

Extend `rumqttc-wrapper-core-next` so native-language wrappers can use the
application-facing capabilities of `rumqttc-v4-next` and `rumqttc-v5-next`
without importing protocol crate types or taking ownership of a Rust event
loop.

The target is **native-client feature parity**, not a mechanical export of
every public Rust item. Packet codec internals, event-loop mutation, custom
Flume senders, benchmark hooks, and APIs whose only purpose is composing Rust
futures remain out of scope unless a native consumer needs their behavior.
Where the Rust API accepts a closure or trait object, wrapper-core must define
an owned, host-neutral interface with explicit concurrency, cancellation, and
lifetime semantics rather than exposing the Rust type.

This document is the source plan for wrapper-core. `TODO16.md` uses the feature
identifiers below to carry the same capabilities through the C ABI.

## Existing baseline that must remain intact

Preserve the current contracts for:

- explicit, immutable MQTT 3.1.1 or MQTT 5 selection per client;
- TCP, rustls TLS, WebSocket, and secure WebSocket transports;
- username/password CONNECT authentication;
- publish, multi-filter subscribe, and multi-filter unsubscribe operations;
- MQTT 5 PUBLISH, SUBSCRIBE, per-filter subscription, and UNSUBSCRIBE
  properties already represented by the wrapper;
- tracked completions, broker reason codes, manual acknowledgement, bounded
  event delivery, diagnostics, overload behavior, reconnect, and bounded
  graceful/immediate shutdown; and
- rejection of protocol-incompatible options before request admission.

New work must not weaken the distinction between local admission and MQTT
completion, expose borrowed protocol objects across threads, silently discard
properties, or make event consumption optional when bounded delivery requires
it.

## Cross-cutting design requirements

### Owned and protocol-explicit types

All public configuration, commands, events, callback inputs, callback outputs,
and errors must own their data. Protocol-neutral fields belong in common types
only when their semantics match. MQTT 5-only behavior must remain in explicit
`V5` variants and must fail on a v4 client before admission.

Do not publicly re-export `rumqttc_v4` or `rumqttc_v5` packet, client, event,
session-store, authentication, redirect, proxy, or transport types. Conversion
belongs in `wrapper-core/src/backend/v4.rs`, `backend/v5.rs`, or a small private
backend module dedicated to the feature.

### Configuration lifecycle

Configuration is mutable only before `NativeClient::start`. Starting a client
must validate the complete configuration and eagerly construct every resource
that can fail without network access. A failure must not start a driver thread
or partially consume callback owners.

Options that affect a connection generation must be snapshotted before that
generation begins. Runtime-changing policies require an explicit command and
completion; they must not be implemented by mutating shared configuration
behind the event loop.

### Callback contract

Host-neutral extension points introduced below must be `Send + Sync + 'static`
and receive owned request/context values. Each interface must document:

- whether calls are serialized or may overlap;
- which wrapper thread invokes it and whether blocking is permitted;
- its deadline and cancellation behavior;
- whether completion after cancellation is ignored;
- whether it may call the same `ClientHandle` reentrantly;
- how panics are contained and translated; and
- when its owner is released during failed start, normal close, immediate
  close, abandonment, and driver failure.

Prefer an asynchronous method returning a boxed, sendable future for storage,
DNS, socket, and authentication work. Never hold wrapper lifecycle locks while
invoking application code. Catch unwinds at every wrapper-owned callback
boundary and turn them into a typed terminal error.

### Errors and completions

Extend `ErrorKind` only for stable caller decisions. Preserve structured
context for protocol version, connection phase/generation, operation ID,
broker reason code, callback class, and delivery status where applicable.
Formatted source chains remain diagnostic and must not become the only way to
distinguish timeout, unsupported configuration, callback failure, persistence
failure, authentication rejection, redirect rejection, or transport failure.

Tracked operations must resolve exactly once. Dropping a completion observer
must never cancel admitted work. Connection loss, redirect, immediate shutdown,
and callback failure must explicitly resolve affected operations rather than
leaving a dropped-channel error.

### Feature selection

Add wrapper-core Cargo features that forward underlying capabilities without
allowing v4 and v5 to select conflicting TLS backends. Define and document at
least:

- `use-rustls` as the default TLS backend;
- `use-native-tls` as an alternative native backend;
- `websocket`;
- `http-proxy` and `socks-proxy`, with `proxy` as their union;
- `system-srv-resolver` for v5;
- `auth-scram` for v5; and
- `tracing` and `tracing-log-compat`.

Use `default-features = false` for both protocol dependencies and forward each
feature deliberately. Reject or prevent unsupported combinations at compile
time. The public wrapper types should remain available when practical even if
a backend is disabled, with start returning a clear unsupported-feature error;
compile-time omission is acceptable only when it materially reduces mandatory
dependencies and is documented.

## Feature work

### WC-01: Last Will and Testament

#### Requirements

Add an owned common will containing topic, payload, QoS, and retain. Add an
explicit protocol options enum whose MQTT 5 variant carries Will Delay
Interval, Payload Format Indicator, Message Expiry Interval, Content Type,
Response Topic, Correlation Data, and ordered User Properties.

Validate topic-name syntax, MQTT UTF-8 fields, binary/string two-byte limits,
payload-format values, nonzero requirements, property multiplicity, and
protocol compatibility before driver start. Preserve absent versus present
empty binary/string fields.

#### Design

Place the will on `CommonConfig` as `Option<LastWillConfig>` and use
`LastWillProtocolOptions::{VersionNeutral, V5(...)}`. Conversion must call the
corresponding v4/v5 `MqttOptions::set_last_will` without dropping properties.
Do not reuse outgoing PUBLISH properties: MQTT 5 Will Properties are a
different legal property set.

#### Acceptance criteria

- Unit tests cover valid v4 and v5 wills and every property boundary.
- A v5 will on a v4 client is rejected before a socket is opened.
- Integration brokers observe the exact will after an ungraceful disconnect
  and observe no will after a successful graceful disconnect.
- Binary payload, correlation data, duplicate user properties, and empty
  optional values round-trip without loss.

### WC-02: Durable client-session storage

#### Requirements

Expose persistent session storage and a stable store scope for both protocols,
including load, save/checkpoint, and clear behavior with the same semantics as
the underlying `SessionStore` traits. Preserve strict v5 broker-session resume
behavior and allow the explicit v5 `AllowBrokerOnly` policy.

#### Design

Define a protocol-neutral wrapper `SessionStore` trait whose operations use an
owned `SessionStoreKey`, a protocol-tagged opaque checkpoint byte sequence,
and asynchronous results. Put encoding/decoding and conversion to each native
`PersistedSession` in wrapper-core so foreign stores never need Rust protocol
types. Include a checkpoint format version, protocol identifier, and maximum
accepted checkpoint size. A v4 checkpoint must never be accepted as v5 or vice
versa.

Also provide adapters that accept the native v4/v5 session-store traits for
Rust wrapper authors, but keep them out of the protocol-neutral public
contract. Decide and document whether wrapper-core's byte format is stable
across patch/minor releases; if it is not, require an explicit migration or
invalidation error rather than decoding arbitrary old bytes.

`V4Config` and `V5Config` gain store, scope, and applicable resume-policy
settings. Validate clean-session/clean-start/expiry combinations and the
one-active-event-loop-per-store-key rule.

#### Acceptance criteria

- Restart tests recover in-flight QoS 1/2, packet identifiers,
  subscribe/unsubscribe state, and incoming QoS 2 state for v4 and v5.
- Save failure, corrupt data, version mismatch, protocol mismatch, oversized
  data, and clear failure are typed and terminate or reset exactly as the
  documented policy requires.
- A callback may complete during shutdown without use-after-free or a hung
  join; timeout and abandonment behavior are covered.
- The optional `session-store-file` adapters can implement the interface
  without copying protocol logic into the C crate.

### WC-03: Packet limits, batching, inflight limits, and throttling

#### Requirements

Expose the underlying operational controls that materially affect resource
usage and throughput:

- maximum request batch and network read batch size for v4/v5;
- pending retransmission throttle for v4/v5;
- v4 maximum outgoing packet size and inflight limit;
- v5 local incoming limit modes (`Default`, `Bytes`, `Unlimited`), CONNECT
  Maximum Packet Size, and outgoing inflight upper limit; and
- existing request capacity and connection timeout without conflicting
  duplicate sources of truth.

#### Design

Use semantic types such as `IncomingPacketLimit` rather than sentinel integers.
Keep local decoder limits separate from MQTT 5 advertised CONNECT limits.
Validate nonzero limits where required, integer conversions, duration range,
and contradictory values. Wrapper event-buffer tuning remains wrapper-owned
and separate from rumqttc request/read batching.

#### Acceptance criteria

- Backend-construction tests assert every value reaches both `MqttOptions`
  objects exactly.
- Boundary tests cover zero, maximum widths, unlimited/default modes, and
  duration conversion without truncation.
- Broker tests demonstrate v4 inflight enforcement, v5 negotiated/effective
  inflight behavior, and incoming/outgoing oversized-packet failures.
- Defaults remain behaviorally compatible with the current wrapper.

### WC-04: MQTT 5 CONNECT properties and topic-alias policy

#### Requirements

Represent all client-configurable MQTT 5 CONNECT properties: Session Expiry,
Receive Maximum, Maximum Packet Size, Topic Alias Maximum, Request Response
Information, Request Problem Information, User Properties, Authentication
Method, and Authentication Data. Expose the underlying automatic outgoing
topic-alias policies independently of explicit per-PUBLISH Topic Alias.

#### Design

Replace the growing flat `V5Config` with a nested owned
`V5ConnectProperties` plus `TopicAliasPolicy`. Preserve presence separately
from zero for optional scalar properties. Authentication Method/Data must be
validated together according to the MQTT 5 rules and coordinated with WC-05.
The wrapper must continue using
`PublishAdmissionPolicy::RequireNegotiatedCapabilities`; policy configuration
must not duplicate the negotiated alias map or bypass its admission gate.

#### Acceptance criteria

- Encode-level tests compare the emitted CONNECT with every property set,
  including ordered duplicate User Properties.
- Invalid singleton values and inconsistent authentication inputs fail before
  connection.
- Reconnect tests prove alias maps reset per generation and automatic/manual
  aliases follow the underlying replay rules.
- All fields are observable in diagnostics or error context where the
  underlying library reports a negotiated rejection.

### WC-05: MQTT 5 enhanced authentication and reauthentication

#### Requirements

Expose initial enhanced authentication, broker challenges, successful and
failed exchanges, application responses, and client-initiated tracked
reauthentication. Support pluggable authenticators and the optional built-in
SCRAM implementation without embedding a particular credential mechanism in
the core API.

#### Design

Define owned `AuthContext`, `AuthChallenge`, `AuthAction`, `AuthOutcome`, and
`AuthFailure` types mirroring stable protocol semantics. Add an
`Authenticator` callback interface to `V5Config`, an explicit
`Command::Reauthenticate`, an authentication completion, and authentication
events. Preserve authentication method, binary data, reason code, reason
string, and User Properties where legal.

Choose one authority for processing each challenge: either the configured
authenticator handles it internally or the event consumer receives a token and
submits a response command. Do not enable both for one exchange. Token designs
must be client- and generation-bound like acknowledgement tokens, reject reuse,
and have a defined timeout/disconnect result.

#### Acceptance criteria

- Tests cover successful initial exchange, multi-step challenge, reauth,
  broker rejection, malformed method changes, overlapping exchanges,
  callback failure/panic, timeout, reconnect, and immediate shutdown.
- Tracked reauthentication distinguishes admission from terminal outcome and
  carries broker reason information.
- SCRAM feature tests run against a deterministic broker fixture and secret
  values never appear in `Debug`, display errors, or tracing output.

### WC-06: MQTT 5 redirect and DNS SRV discovery

#### Requirements

Expose redirect policy, accepted/rejected redirect outcomes, loop/attempt
limits, Server Reference parsing, transport transitions, and optional DNS SRV
resolution. Preserve the distinction between CONNACK and DISCONNECT redirect
sources.

#### Design

Use an owned policy enum for common fixed policies and an optional asynchronous
policy callback for application decisions. Define a host-neutral `SrvResolver`
returning owned priority/weight/port/target records. Forward the system resolver
feature when selected. Redirect and SRV callbacks must obey total connection
deadlines, must not run under event/lifecycle locks, and must be cancellation
safe.

Emit a `WrapperEvent::Redirect` for accepted and rejected decisions with the
advertised reference and selected endpoint. Define whether TLS credentials,
WebSocket paths, authentication state, and session-store keys are retained or
recomputed for a redirected target; default to the underlying library's safe
policy and document it.

#### Acceptance criteria

- Tests cover authority, URI, IPv4/IPv6, WebSocket, secure transport, and SRV
  references; malformed/unsupported references; weighted records; empty DNS
  answers; loops; attempt exhaustion; and cancellation during shutdown.
- Redirect events preserve reason, source, advertised reference, and chosen
  target.
- A redirect cannot reset operation completions silently or cross a session
  store scope without the documented policy.

### WC-07: Proxy transports

#### Requirements

Support HTTP CONNECT and SOCKS proxy configuration for both protocol clients,
including optional proxy authentication, remote/local DNS selection where the
underlying proxy supports it, and TCP/TLS/WebSocket/WSS composition.

#### Design

Add protocol-neutral `ProxyConfig::{Http, Socks4, Socks5}` variants containing
owned endpoint and credential values. Keep proxy TLS, broker TLS, and
WebSocket URLs distinct. Redact passwords/tokens from `Debug` and error source
chains. Cargo features must forward the exact underlying proxy feature.

#### Acceptance criteria

- Deterministic proxy fixtures cover each enabled protocol, authentication
  success/failure, DNS mode, TLS layering, reconnect, timeout, and shutdown.
- Disabled proxy features produce a configuration/unsupported-feature error,
  not silent direct connection.
- Credentials are absent from diagnostics, logs, and panic messages.

### WC-08: Unix-domain sockets and custom socket connectors

#### Requirements

On supported platforms, add Unix-domain broker targets. Also provide a
host-neutral asynchronous socket-connector interface for runtimes that supply
their own connected byte stream.

#### Design

Refactor broker configuration into `BrokerTarget::{Tcp, Unix, WebSocket}` so a
Unix target cannot carry a meaningless host/port. For custom connectors, define
a wrapper-owned asynchronous byte-stream trait with read, write, flush, and
shutdown behavior, or explicitly defer it if a sound cross-language stream
contract cannot be guaranteed. The contract must specify partial I/O, wakeup,
cancellation, concurrent read/write, and ownership transfer.

Unix support should be implemented before arbitrary connectors. Do not block
the simpler Unix value feature on the callback-stream design.

#### Acceptance criteria

- Unix tests cover connect, reconnect, permissions/not-found errors, and
  shutdown on every supported Unix CI target.
- Unsupported platforms reject Unix targets before starting the driver.
- If custom connectors ship, stress tests cover partial I/O, concurrent
  shutdown, callback panic/failure, cancellation, and owner release.

### WC-09: WebSocket handshake customization

#### Requirements

Allow callers to add, replace, or remove permitted WebSocket request headers
and to reject a handshake construction. Preserve the final URI, required
upgrade headers, and security constraints.

#### Design

Prefer a declarative owned header list and validation policy for common use.
Add a fallible callback only for dynamic handshakes. Forbid modification of
protocol-critical headers unless the underlying library explicitly supports
it. Validate header names/values eagerly, redact authorization/cookie values,
and document redirect behavior.

#### Acceptance criteria

- Tests cover custom headers, duplicate ordering, invalid bytes, protected
  headers, callback rejection/panic, WSS, proxy composition, and reconnect.
- Disabled WebSocket builds reject this configuration explicitly.
- Sensitive headers never appear in diagnostics or logs.

### WC-10: MQTT 5 DISCONNECT reason and properties

#### Requirements

Allow graceful and immediate client-originated disconnect operations to carry
an MQTT 5 reason code and legal properties: Session Expiry Interval, Reason
String, User Properties, and Server Reference where permitted by the
underlying API. Preserve the current version-neutral default.

#### Design

Add `DisconnectProtocolOptions::{VersionNeutral, V5(...)}` to the disconnect
commands without changing their ordering, timeout, idempotence, or completion
semantics. Coalesced close callers must have a deterministic rule: the first
successfully admitted disconnect payload wins; later incompatible payloads get
an explicit state/conflict error rather than being ignored.

#### Acceptance criteria

- Broker fixtures observe exact reason/property encoding for graceful and
  immediate paths supported by rumqttc.
- v5 options on v4 fail before admission.
- Concurrent close tests prove payload selection, idempotence, escalation, and
  timeout behavior.

### WC-11: Rich connection, packet, authentication, and redirect events

#### Requirements

Expose application-relevant information currently lost when backend events are
collapsed: complete CONNACK outcome/properties, broker DISCONNECT reason and
properties, authentication events, redirect outcomes, and sufficiently
detailed outgoing activity to correlate packet identifier and operation where
available.

Do not expose every raw packet by default. ACK packets already represented by
tracked completions should not create a second contradictory completion model.
If raw packet observation is later required, make it an opt-in diagnostic event
stream with explicit stability and backpressure semantics.

#### Design

Add owned protocol-specific detail records nested in stable wrapper events.
Keep the current small common fields for convenient consumers. All event data
must remain valid after the backend poll returns. Update event sizing and
delivery tests because richer events increase queue memory pressure.

#### Acceptance criteria

- Every underlying `Event` variant is either mapped to a documented wrapper
  event or explicitly classified as internal with a test asserting that choice.
- CONNACK and broker DISCONNECT tests cover all exposed v5 properties and
  repeated User Properties without loss.
- Existing consumers can continue matching coarse event categories; Rust API
  breaking changes, if unavoidable, are documented in `CHANGELOG.md`.

### WC-12: Network options, observability, and remaining extension hooks

#### Requirements

Expose value-based network controls supported by `rumqttc-core::NetworkOptions`
where portable: TCP send/receive buffer sizes, TCP_NODELAY, local bind address,
connection timeout, Linux/Android/Fuchsia bind-device selection, and Linux
MPTCP. Forward tracing features and add wrapper lifecycle/operation spans that
do not contain payloads, passwords, authentication data, or sensitive headers.

Request modifiers, custom DNS, custom socket connectors, and other Rust
closures are covered by WC-06, WC-08, and WC-09. URL parsing, stream adapters,
low-level `MqttState`, direct packet injection, custom client senders, and
benchmark instrumentation are explicitly not parity requirements for native
clients.

#### Acceptance criteria

- Every supported network option is mapped in v4 and v5 or documented with a
  concrete platform reason for omission.
- Platform-specific options have compile and behavior coverage on relevant CI
  targets and fail early elsewhere.
- Tracing and log-compat feature matrices compile independently, and redaction
  tests inspect captured output.

### WC-13: TLS backend and credential policy

#### Requirements

Complete the gap between the current PEM-oriented rustls configuration and the
underlying TLS choices. Support explicit platform/native root use, supplied PEM
roots, rustls PEM client identity, native-tls PKCS#12 identity and password,
and ALPN lists where supported. Make the selected TLS backend and trust source
explicit and reject a configuration not supported by the built artifact.

Arbitrary injected Rust `rustls::ClientConfig` and
`native_tls::TlsConnector` objects are not part of the protocol-neutral parity
contract. Their native-language use case belongs under the custom connector
escape hatch in WC-08 unless wrapper-core later defines a safe host-neutral TLS
policy interface.

#### Design

Replace the current implicit `TlsConfig` interpretation with explicit
`TlsBackend`, `TlsRootPolicy`, and backend-specific client identity variants.
Do not infer the backend from certificate bytes. Preserve separate TLS policy
for a broker and for any secure proxy if WC-07 supports one. Validate PEM,
PKCS#12, key/password pairing, ALPN element lengths, and build capabilities
eagerly before the driver starts. Secret-bearing identity/password types need
redacted `Debug` implementations and zeroization where ownership permits it.

#### Acceptance criteria

- TCP TLS and WSS tests cover platform roots, custom roots, mutual TLS, ALPN,
  hostname failure, malformed credentials, and both enabled backends.
- Mixed-feature builds never choose a backend implicitly and disabled backends
  fail before opening a socket.
- Certificate/key/identity/password material is absent from errors and traces,
  and owner-release tests cover failed start and every shutdown path.
- Existing rustls PEM configuration has a documented, behavior-preserving
  migration path.

## Implementation order

Implement in reviewable, independently releasable slices:

1. **Foundation:** Cargo feature forwarding, semantic limit types, structured
   callback errors, secret-redacting wrappers, and backend-construction test
   helpers.
2. **Pure value configuration:** WC-01, WC-03, WC-04, WC-07, the Unix portion
   of WC-08, WC-09's declarative headers, WC-10, and value-based WC-12 options.
3. **Persistence:** WC-02, including checkpoint versioning and restart tests.
4. **Authentication:** WC-05, including event-driven and callback ownership
   decisions before exposing either publicly.
5. **Endpoint policy:** WC-06 plus any dynamic WebSocket or connector portions
   of WC-08/WC-09.
6. **Observation:** WC-11 and tracing, after new backend outcomes are stable.
7. **Transport security:** WC-13, including build matrices and migration from
   the existing `TlsConfig` representation.
8. **Parity closure:** audit every public `MqttOptions` setter, application
   client operation, and underlying event variant against an explicit matrix.

Do not combine all callback systems into one unreviewable change. Each slice
must update `CHANGELOG.md` when it changes user-facing wrapper behavior and
must keep v4/v5 behavior aligned where the underlying protocols allow it.

## Verification and definition of done

Maintain a checked-in parity matrix listing every application-facing v4/v5
option, client operation, and event as `supported`, `not applicable`, or
`intentionally omitted`, with a reason and test reference. CI should fail when
a newly added underlying setter/operation/event is absent from the matrix.

For every implementation slice, run at minimum:

```bash
cargo fmt --manifest-path native-wrappers/Cargo.toml --all --check
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next
```

Feature-sensitive changes must additionally run an each-feature matrix and
explicit supported combinations for both client dependencies. Callback and
lifecycle changes require Loom-style model tests where practical, Miri for
unsafe code if any is introduced, deterministic broker tests, and stress tests
covering shutdown while callbacks are pending.

Wrapper-core parity is complete only when:

- WC-01 through WC-13 satisfy their acceptance criteria or have a reviewed,
  documented `intentionally omitted` decision;
- the parity matrix contains no unexplained underlying capability;
- no backend conversion silently drops a supplied field;
- all callback owners have tested release behavior on every terminal path;
- defaults preserve existing wrapper behavior;
- public Rust documentation explains protocol, admission, completion,
  threading, cancellation, and security semantics; and
- `CHANGELOG.md` records every user-visible addition and any breaking change.
