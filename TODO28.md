# C Wrapper MQTT 5 Manual Acknowledgement Content

## Goal

Allow applications to choose legal PUBACK/PUBREC reason codes, Reason String,
and User Properties when acknowledging an incoming MQTT 5 publication.
Retain the existing safe acknowledgement token and flush completion model.

## Current foundation and feasibility

Rust `ManualAck` exposes packet contents in `rumqttc-v5/src/client.rs`.
Wrapper-core stores a generated `PreparedAck` behind an opaque, client- and
generation-bound token. The C acknowledgement calls currently have no options.

This is feasible by customizing the reserved native packet before admission.
Do not replace the token with an application-supplied packet identifier.
Consult `docs/spec/mqtt-v5.0.md`, sections 3.4 and 3.5, and its requirement index
for legal reason codes, properties, size limits, and negative QoS 2 handling.

## Implementation requirements

- [ ] Add acknowledgement option types to wrapper-core, with MQTT 5 content
  separate from the version-neutral default success acknowledgement.
- [ ] Add nonblocking and tracked C acknowledgement-with-options functions
  using size-versioned records and copied string/property views.
- [ ] Accept only codes legal for the token's PUBACK or PUBREC kind. Preserve
  ordered duplicate User Properties and absent versus present-empty fields.
- [ ] Validate UTF-8, lengths, property counts, reserved fields, and outbound
  packet size before consuming the token or admitting an operation.
- [ ] Reject MQTT 5 options on v4, including explicitly selected default-valued
  v5 options. Keep the existing no-options behavior unchanged.
- [ ] Apply validated content to the native `ManualAck` and submit through the
  existing client; native state remains responsible for QoS transitions.
- [ ] Restore the token on failed admission. Prevent duplicate, stale, or
  cross-client acknowledgements and preserve reconnect invalidation.
- [ ] Audit acknowledgement completion matching: negative PUBREC must terminate
  the exchange correctly and resolve its local flush completion exactly once.
  If the native state path needs a fix, implement it in the client first.
- [ ] Treat local ACK flush as distinct from proof of application processing or
  broker receipt. A dropped observer never cancels an admitted acknowledgement.
- [ ] Keep incoming publications readable after acknowledgement; retain the
  existing event and client lifetime rules.

## Verification and completion

- [ ] Verify exact wire reason and properties for success, alternative success,
  and legal rejection on QoS 1 and QoS 2 publications.
- [ ] Cover malformed options without token consumption, backpressure retry,
  reconnect, duplicate acknowledgement, and event/client destruction races.
- [ ] Verify rejected QoS 2 has no later successful handshake or stale state.
- [ ] Update the C header/exports, example, README, root `CHANGELOG.md`, and
  `PARITY.md` so manual-ACK support names both timing and content explicitly.

Complete when native C callers can send the same legal manual acknowledgement
contents as Rust callers without assuming ownership of protocol identifiers.
