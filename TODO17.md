# Python Wrapper Feature Parity

## Goal and source of truth

Expose the supported `rumqttc-wrapper-core` capabilities through the typed
`rumqttc-next` CPython `asyncio` package. Use
`native-wrappers/wrapper-core/PARITY.md` for the current supported and
intentionally omitted decisions. This plan concerns the Python API, its
private PyO3 extension, and its packaging. It does not require a C ABI.

Keep the existing `MqttClient`, `MqttClientOptions`, `events()` iterator,
tracked operations, and `async with` behavior. The Python facade remains
asynchronous and bound to one `asyncio` loop. All new settings are immutable
typed values copied into wrapper-core before native startup. Reject protocol
mismatches and unsupported build/platform capabilities before opening a socket.
Do not bypass wrapper-core by constructing `rumqttc-v4-next` or
`rumqttc-v5-next` values. If a needed contract is missing, establish and test
it in wrapper-core, then update its parity matrix before binding it.

## Shared Python API rules

- Add focused frozen dataclasses and enums in `native-wrappers/python/python/rumqttc/_types.py`;
  use optional nested records in `MqttClientOptions` for larger feature
  families. Keep existing constructor defaults and names working. Preserve
  `None` versus present-empty/zero where MQTT distinguishes them, and preserve
  ordered, duplicate User Properties.
- Validate Python types, MQTT UTF-8 and binary lengths, integer ranges,
  finite durations, protocol selectors, and mutually exclusive choices before
  starting the driver. Reject `bool` where an integer is required. Use seconds
  for public durations, as the current Python API does. Copy mutable byte
  buffers and sequences before admission or retention.
- Extend the private PyO3 bridge with typed conversion where practical. If an
  internal JSON message remains, make it versioned and lossless for bytes,
  ordered properties, optional values, and structured errors; never expose it
  as the public API. Preserve Python exception classes and delivery status.
- Public callback protocols use `async def` when host work may wait. Dispatch
  from the native driver to the client's owning `asyncio` loop without holding
  core or Python locks. Give each invocation one completion, bounded pending
  work, a timeout/cancellation path, and an exactly-once terminal result.
  Specify whether callbacks can overlap and whether they may call client
  methods. A cancelled Python waiter does not revoke already admitted MQTT
  work; callback cancellation follows the core operation's contract.
- Keep strong references to callback owners until no native invocation can use
  them. Close, failed start, loop closure, interpreter shutdown, and finalizer
  abandonment must release them without scheduling onto a dead loop or
  executing Python code from an unsafe native thread. Document any host work
  that cannot be interrupted. Never log passwords, private keys, auth data,
  proxy credentials, cookies, or payloads through exceptions or debug output.
- Expose a typed capability query for optional compiled features and platform
  controls. Unsupported selections fail predictably before network access.

## Feature work

### PY-WC-01: Last Will and Testament

Add a `LastWill` value with topic, bytes payload, QoS, retain, and optional
MQTT 5 will properties. Validate and copy it at client construction; distinguish
absent properties from present-empty data. Test exact v4/v5 broker-observed
will packets, graceful close, abrupt termination, and invalid combinations.

### PY-WC-02: Durable session storage

Define an async Python store protocol with load, save, and clear; load returns
an explicit not-found result distinct from failure. Carry protocol/checkpoint
version metadata and an explicit stable scope. Expose checkpoint size and the
MQTT 5 broker-session-resume policy. Specify one-active-client-per-key,
per-key serialization, atomic-save requirements, callback timeout, and close
behavior. A file adapter may be offered separately; do not make a path option
the only store interface. Test v4/v5 restart recovery, corrupt and oversized
checkpoints, cancellation, callback exceptions, and loop shutdown.

### PY-WC-03: Packet and runtime limits

Expose request/read batch settings, pending throttle, v4 incoming/outgoing
packet and inflight limits, and the distinct MQTT 5 local input limit,
advertised Maximum Packet Size, and outgoing inflight upper limit. Use named
limit modes where default, unlimited, and a numeric bound differ. Test wire
limits, checked conversions, defaults, and resource-bounded behavior.

### PY-WC-04: MQTT 5 CONNECT properties and aliases

Add a typed `V5ConnectProperties` value for all supported fields and an
outgoing topic-alias policy. Keep explicit PUBLISH aliases separate. Preserve
presence and User Property order, validate authentication method/data pairing,
and reject these options on MQTT 3.1.1. Test exact CONNECT packets and alias
reset/replay across reconnects.

### PY-WC-05: Enhanced authentication

Provide an async authenticator protocol over owned challenge/action values,
including exchange kind, method, reason, data, and User Properties. Expose
client-initiated reauthentication with a tracked result. Offer the core SCRAM
convenience configuration when built, without making it the only auth path.
Specify timeout, overlapping exchange, reconnect, callback failure, and close
semantics. Test multi-step and reauth broker flows, late replies, secret
redaction, and loop teardown.

### PY-WC-06: Redirect and DNS SRV

Expose the core's fixed reject/follow redirect policy and an async SRV resolver
protocol with priority, weight, port, and target records. Surface redirect
events and selected endpoints with typed values. Expose system SRV only when
built. Test all reference forms, weighted/empty/failing answers, loop limits,
shutdown, and isolation of session-store scope across redirects. An
application redirect-policy callback needs a core contract first.

### PY-WC-07: Proxy transports

Add typed HTTP CONNECT, HTTPS proxy, and SOCKS5 options with endpoint, DNS
policy, credentials, and independent proxy TLS where supported. Reject
unsupported proxy kinds before connecting; never silently connect directly.
Test authentication, remote/local DNS, TLS/WSS composition, reconnect, and
redaction for both MQTT versions.

### PY-WC-08: Unix sockets

Add an explicit Unix endpoint/transport option with documented path encoding
and early unsupported-platform failure. Test path validation, reconnect, and
both close modes. Custom socket connectors are intentionally omitted in the
current core parity matrix; revisit only after core support and native wakeup,
partial-I/O, and destruction tests exist.

### PY-WC-09: WebSocket handshake edits

Expose ordered declarative header add/replace/remove edits, with validation of
protected names and sensitive values. Test reconnect/redirect behavior and
WSS/proxy composition. Dynamic handshake callbacks are intentionally omitted
in the current core parity matrix; add a Python callback only after the core
contract is supported and tested.

### PY-WC-10: MQTT 5 DISCONNECT options

Extend graceful and immediate close with optional typed reason/properties
without changing calls that omit options. Preserve first-admitted-options-wins
on coalesced close; report incompatible later options. Reject v5 options on
v4. Test exact packets, concurrent callers, escalation, and timeouts.

### PY-WC-11: Rich events

Audit existing `Connected`, `ConnectionRejected`, `BrokerDisconnect`,
`Authentication`, `Redirect`, and `Outgoing` dataclasses against supported
core fields. Add missing typed details and operation/packet IDs without
converting events into mutable shared views. Test absent versus present-empty
properties, ordered lists, event lifetime, and bounded delivery. Keep protocol
ACK internals represented by tracked completions, as the core matrix decides.

### PY-WC-12: Network options and observability

Add portable socket options and supported platform controls with typed
validation and early unsupported-platform errors. Expose build capabilities.
Document how Python applications receive Rust tracing/log output without
installing or replacing process-global subscribers implicitly. Test socket
effects, platform guards, diagnostics, and captured-output redaction.

### PY-WC-13: TLS backend and credentials

Extend `TlsOptions` with explicit rustls/native backend, platform/custom roots,
PEM or PKCS#12 identity, and ordered ALPN protocols. Preserve today's PEM
defaults. Reject unavailable backends before connecting; copy and protect
secret input. Test TLS and WSS trust, mutual TLS, malformed input, hostname
failure, replacement, and redaction across supported wheel builds.

## Delivery and definition of done

Implement value-only configuration first (PY-WC-01, 03, 04, 07, Unix 08,
declarative 09, 10, 12, 13), then callback-backed storage, auth, and SRV
(PY-WC-02, 05, 06), then finish event/observability gaps (PY-WC-11). Shared
callback dispatch and capability reporting precede callback-backed features.

For each slice, update the typed facade, private bridge, tests, examples when
the normal client flow changes, `native-wrappers/python/README.md`, and the
repository `CHANGELOG.md`. Run focused `pytest` and broker fixtures during
development, then the Python suite and native-wrapper workspace tests. Verify
installed wheels on supported platforms and supported Python versions. Include
static type checks and tests for both MQTT protocols, failures, cancellation,
reconnect, and shutdown where applicable.

Python parity is complete when every supported core capability in PY-WC-01
through PY-WC-13 is bound and tested or has an explicit reviewed Python-only
omission. Carry the core matrix's intentionally omitted decisions forward;
do not describe core parity as Python API parity.
