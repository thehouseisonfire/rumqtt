# Native JavaScript Wrapper Feature Parity

## Goal and source of truth

Expose the supported `rumqttc-wrapper-core` capabilities through the
`@rumqtt-next/rumqttc` Node-API JavaScript/TypeScript package. Use
`native-wrappers/wrapper-core/PARITY.md` for current support and intentionally
omitted decisions.

Keep the current `MqttClient`, promise-based operations, single-consumer
`events()` iterator, and Node.js/local Deno/Bun package contract. Add
capabilities through typed option records and methods rather than exposing
Rust or C layouts. Construction validates and copies options; the first
`connect()` remains the sole native startup boundary. Never bypass wrapper-core
by constructing protocol-client values directly in the addon. If a binding
needs missing core behavior, establish and test the core contract first and
update its parity matrix.

## Shared JavaScript and TypeScript API rules

- Update `native-wrappers/js/index.d.ts` and runtime validation together.
  Keep the protocol-discriminated option types so MQTT 5 settings cannot be
  selected on a typed MQTT 3.1.1 client. Runtime checks still reject plain
  JavaScript misuse before startup. Preserve current defaults and method names.
- Use `Uint8Array` for binary input/output and copy sliced views correctly;
  continue accepting `Buffer` as a `Uint8Array`. Use `bigint` where a core
  counter or identifier can exceed JavaScript's safe integer range. Check
  finite numeric durations, integer ranges, null/undefined/absent distinctions,
  MQTT lengths, and ordered duplicate User Properties. Keep secrets out of
  exception messages, object inspection, and native diagnostics.
- Extend the private addon conversion without losing bytes, optional values,
  ordering, or structured error and delivery fields. If internal JSON remains,
  version its shape and keep it private. Copies made for native admission must
  outlive caller mutation of the original arrays.
- Host callbacks return promises (or a value for immediate completion) and run
  on the owning JavaScript environment, never on a Rust driver thread. Use a
  bounded, thread-safe dispatch path. Specify callback overlap, reentry,
  timeout, cancellation notification, duplicate completion, and rejected
  promise behavior. Promise abandonment does not cancel admitted MQTT work.
- Retain callback and environment references until native work is quiescent.
  Closing, failed start, worker termination, environment teardown, and addon
  unload must not call JavaScript after its environment is gone. Do not block
  the JavaScript thread while waiting for host callbacks or native close.
  Test cleanup when callbacks never settle and when a worker exits mid-call.
- Expose a runtime capability query for compiled features and platform
  controls. Unsupported selections fail before opening a socket; package
  declarations must agree with the loaded native artifact.

## Feature work

### JS-WC-01: Last Will and Testament

Add a typed last-will option with topic, `Uint8Array` or string payload, QoS,
retain, and MQTT 5 will properties. Copy the payload and preserve absent versus
present-empty values. Test exact v4/v5 broker-observed packets, graceful close,
abrupt worker/process termination, and invalid options.

### JS-WC-02: Durable session storage

Define a promise-based store interface with load, save, and clear. Distinguish
not found from failure; include protocol/checkpoint version metadata, stable
scope, maximum checkpoint size, and MQTT 5 broker-session-resume policy.
Specify per-key serialization, atomic save, one-active-client-per-key,
callback timeout, and environment teardown. A built-in file adapter can be
separate; a path setting must not be the only persistence interface. Test
v4/v5 restart recovery, corrupt/oversized checkpoints, rejection, unresolved
promises, close, and worker termination.

### JS-WC-03: Packet and runtime limits

Expose request/read batch sizes, pending throttle with explicit time units,
v4 packet and inflight limits, and separate MQTT 5 local input, advertised
Maximum Packet Size, and outgoing inflight controls. Represent default,
unlimited, and numeric limits distinctly. Validate JavaScript numeric safety
before conversion; test packet boundaries, defaults, and resource limits.

### JS-WC-04: MQTT 5 CONNECT properties and aliases

Add a typed MQTT 5 CONNECT-properties option and automatic outgoing alias
policy. Preserve absent versus zero/empty and User Property order. Keep
explicit PUBLISH aliases separate. Reject invalid auth method/data pairing
and v5 options on v4. Test exact CONNECT packets and alias reset/replay after
reconnect.

### JS-WC-05: Enhanced authentication

Expose a promise-based authenticator over owned challenge/action objects with
exchange kind, method, data, reason, and User Properties. Add a tracked
`reauthenticate()` operation. Offer SCRAM configuration when built while
retaining raw callback support. Test initial/multi-step/reauth flows,
overlap, timeout, rejection, late promises, reconnect, close, environment
teardown, and secret redaction.

### JS-WC-06: Redirect and DNS SRV

Expose the core's fixed redirect policy and a promise-based SRV resolver
returning priority, weight, port, and target records. Surface typed redirect
events and chosen endpoints. Gate system SRV on build capability. Test all
reference forms, weighted and failing DNS, redirect loops, shutdown, and
session-store scope isolation. An application redirect-policy callback needs
a supported wrapper-core contract before adding a JS binding.

### JS-WC-07: Proxy transports

Add typed HTTP CONNECT, HTTPS proxy, and SOCKS5 settings with separate broker
and proxy endpoints, DNS policy, credentials, and independent proxy TLS when
supported. Unsupported kinds must fail before network access, never fall back
to a direct connection. Test auth, DNS, TLS/WSS composition, reconnect,
failure, and redaction on both protocols.

### JS-WC-08: Unix sockets

Add an explicit Unix broker target with documented path encoding and
unsupported-platform behavior. Test path failure, reconnect, and close.
Custom socket connectors are intentionally omitted in the current core parity
matrix. Revisit only after core support and native partial-I/O, wakeup, and
destruction tests exist.

### JS-WC-09: WebSocket handshake edits

Expose ordered declarative header add/replace/remove edits. Validate protected
headers and sensitive values; test redirects, reconnects, WSS/proxy
composition, and disabled features. Dynamic handshake callbacks are
intentionally omitted in the current core parity matrix; add a JS callback
only after a tested core contract exists.

### JS-WC-10: MQTT 5 DISCONNECT options

Extend `close()` and `closeNow()` with optional typed v5 reason/properties
while preserving existing calls. Honor first-admitted-options-wins and reject
conflicting concurrent options. Test exact packets, v4 mismatch, escalation,
timeouts, and repeatable completion.

### JS-WC-11: Rich events

Audit the existing `MqttEvent` union against supported core connection,
broker-disconnect, authentication, redirect, and outgoing fields. Add missing
typed details and operation/packet IDs while preserving the bounded single
iterator. Test present-empty fields, property ordering, byte ownership,
wrong-protocol cases, and backpressure. Keep internal ACK packets represented
through tracked completions, as the core matrix decides.

### JS-WC-12: Network options and observability

Add portable network controls and supported platform settings with runtime
validation and capability reporting. Document integration with Rust
tracing/log output without implicitly installing a process-global subscriber.
Test socket effects, platform guards, worker diagnostics, and redaction.

### JS-WC-13: TLS backend and credentials

Extend TLS/WSS options with explicit rustls/native backend, platform/custom
roots, PEM or PKCS#12 client identity, and ordered ALPN. Preserve current PEM
defaults. Reject unavailable backends before connecting, copy secrets, and
test trust policy, mutual TLS, malformed input, hostname failure, mixed
feature builds, and redaction in packaged artifacts.

## Delivery and definition of done

Implement value-only options first (JS-WC-01, 03, 04, 07, Unix 08,
declarative 09, 10, 12, 13), then the callback dispatch foundation and
storage/auth/SRV (JS-WC-02, 05, 06), then event/observability gaps
(JS-WC-11). Keep callbacks that are omitted in core out of the required path.

For each slice, update runtime code, `index.d.ts`, type tests, Node.js tests,
examples when ordinary use changes, `native-wrappers/js/README.md`, and the
repository `CHANGELOG.md`. Run focused broker and failure fixtures, the Node
test and TypeScript suites, native-wrapper workspace tests, and installed
package consumers. Verify the same prebuilt addon under supported Node.js,
local Deno, and Bun versions on published platforms. Test both MQTT protocols,
invalid input, reconnect, cancellation, worker exit, and environment teardown
where applicable.

Native JavaScript parity is complete when every supported core capability in
JS-WC-01 through JS-WC-13 is bound and tested or has an explicit reviewed
JavaScript-only omission. Carry the core matrix's intentionally omitted
decisions forward; do not describe core parity as JavaScript API parity.
