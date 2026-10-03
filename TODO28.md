# C Wrapper MQTT 5 Manual Acknowledgement Content

## Goal

Allow applications to choose legal PUBACK/PUBREC reason codes, Reason String,
and User Properties when acknowledging an incoming MQTT 5 publication.
Retain the existing safe acknowledgement token and flush completion model.

## Current foundation and feasibility

Rust `ManualAck` exposes packet contents in `rumqttc-v5/src/client.rs`.
Wrapper-core stores a generated `PreparedAck` behind an opaque, client- and
generation-bound token. The C acknowledgement calls currently have no options.

This is feasible by customizing a copy of the reserved native packet before
admission, leaving the stored default acknowledgement unchanged on failure.
Do not replace the token with an application-supplied packet identifier.
Native `try_manual_ack` currently queues the packet without a pre-admission
packet-size check, so that validation must be added rather than assumed.
Consult `docs/spec/mqtt-v5.0.md`, sections 3.4, 3.5, 3.2.2.3.6, 4.3.3, and
4.8.2, and its requirement index for legal client-originated reason codes,
properties, negotiated size limits, and rejection handling.

## Implementation requirements

- [x] Add acknowledgement option types to wrapper-core, with MQTT 5 content
  separate from the version-neutral default success acknowledgement.
- [x] Add nonblocking and tracked C acknowledgement-with-options functions
  using size-versioned records and copied string/property views.
- [x] Accept only codes legal for both the token's PUBACK or PUBREC kind and
  the client sender role. Reject `0x10` (`NoMatchingSubscribers`), which is
  server-only for both packet kinds; client success is `0x00`. Audit and correct
  the native Rust manual-ACK example that currently uses `0x10`. Rust API
  acceptance alone is not evidence that a code is legal for a client to send.
- [x] Preserve ordered duplicate User Properties and absent versus
  present-empty fields.
- [x] Validate UTF-8, lengths, property counts, and reserved fields before
  consuming the token or admitting an operation.
- [x] Validate the complete encoded ACK size, including fixed header and
  length fields, against MQTT encoding limits and the broker's negotiated
  Maximum Packet Size. Retain the negotiated limit with the connection
  generation and check it under the existing admission gate together with
  token validity. Never validate a token against a replacement connection's
  limit. Reject oversized options without consuming the token or allocating an
  operation; do not silently remove caller-supplied properties to make them fit.
- [x] Reject MQTT 5 options on v4, including explicitly selected default-valued
  v5 options. Keep the existing no-options behavior unchanged.
- [x] Apply validated content to a copy of the reserved native `ManualAck` and
  submit through the existing client; native state remains responsible for QoS
  transitions. Keep the original prepared default packet available for rollback.
- [x] Restore the token and its original default packet on failed admission,
  including backpressure. A retry may choose different options or the existing
  no-options API without inheriting the failed attempt's reason or properties.
  Prevent duplicate, stale, or cross-client acknowledgements and preserve
  reconnect invalidation.
- [x] Audit acknowledgement completion matching: negative PUBREC must terminate
  the exchange correctly and resolve its local flush completion exactly once.
  If the native state path needs a fix, implement it in the client first.
- [x] Treat local ACK flush as distinct from proof of application processing or
  broker receipt. A dropped observer never cancels an admitted acknowledgement.
- [x] Document that a negative PUBACK/PUBREC terminates that delivery; it is not
  a request for retry or redelivery. For shared subscriptions, a negative ACK
  requires the broker to discard that message rather than assign it to another
  subscriber. Explain this for temporary overload and `QuotaExceeded` examples;
  Reason String and User Properties are broker-facing diagnostics, not an
  application response to the original publisher.
- [x] Keep incoming publications readable after acknowledgement; retain the
  existing event and client lifetime rules.

## Verification and completion

- [x] Verify exact wire reason and properties for client success (`0x00`) and
  legal rejection on QoS 1 and QoS 2 publications. Verify server-only `0x10`
  and unknown codes are rejected without consuming the token or sending an ACK.
- [x] Cover malformed options without token consumption, backpressure retry,
  reconnect, duplicate acknowledgement, and event/client destruction races.
- [x] Verify full encoded size at and above a negotiated limit, including
  length-field growth. An oversized attempt must leave the token usable for a
  smaller ACK. Reconnect with a different limit must invalidate old tokens and
  enforce the new limit only for new-generation tokens.
- [x] After failed admission, verify retry with different options and retry via
  the no-options API both send their requested content without retained fields
  from the failed attempt.
- [x] Verify rejected QoS 2 releases receive quota, allows the packet identifier
  to identify a new publication, has no later successful handshake or stale
  state, and resolves its local flush completion exactly once.
- [x] Update the C header/exports, example, README, root `CHANGELOG.md`, and
  `PARITY.md` so manual-ACK support names both timing and content explicitly.

Complete when native C callers can send legal client-originated manual
acknowledgement contents available through Rust without assuming ownership of
protocol identifiers or inheriting permissive native input validation.

## Implementation evidence

Wrapper-core exposes `AcknowledgementProtocolOptions` and
`V5AcknowledgementOptions` through `Command::AcknowledgeWithOptions`. The C API
adds `rumqttc_client_try_acknowledge_with_options` and
`rumqttc_client_acknowledge_with_options_tracked`, with initializer macros and
copied option records. Existing ACK entry points share the same admission path.

Validation and rollback coverage lives in the acknowledgement and handle unit
tests; `wrapper-core/tests/manual_ack.rs` verifies exact wire contents, receive
quota release, identifier reuse, changed reconnect limits, and delayed/failed
flush completion. C parser, FFI race, and native consumer tests verify record
validation, token preservation, input copying, and event lifetime after client
destruction. Native negative-PUBREC handling required no state-machine change.

Verified with full wrapper-core/C tests using default and no-default features,
full v4/v5 client tests and doctests, root and wrapper workspace checks, Clippy
with warnings denied, C ABI/header/export checks, and all 53 portable C tests.
