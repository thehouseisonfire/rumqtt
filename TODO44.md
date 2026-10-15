# Opt-In Native Protocol Observation Events

Status: planned.

## Goal

Expose precise native packet activity and intermediate incoming control packets
for diagnostics, while keeping MQTT state transitions and completion authoritative.

## Foundation and scope

Wrapper-core filters incoming control packets and maps outgoing PUBACK, PUBREC,
PUBREL, and PUBCOMP into one activity. PINGREQ/PINGRESP also share a category.
Terminal ACK details from [TODO29.md](TODO29.md) are already implemented; this
task adds optional event observations, including successful intermediate QoS 2
PUBREC/PUBREL and incoming PINGRESP. Incoming publish identifiers are covered by
[TODO43.md](TODO43.md).

## Implementation requirements

- [ ] Inventory the native events actually emitted by both clients and define
  a typed observation model: direction, exact packet kind, optional packet ID,
  connection generation, reason code, and legal packet-level properties. Preserve
  ordered duplicate User Properties and absent versus present-empty values.
- [ ] Preserve existing coarse outgoing events and accessors. Add precise detail
  through additive accessors and an explicit startup observation selector; do not
  change numeric constants or make existing consumers receive a new high-volume
  stream by default. Separate selection from `emit_outgoing_events` coherently.
- [ ] Map actual native observations before their information is discarded.
  Incoming events describe native processing; outgoing events describe the native
  event's existing scheduling boundary, not necessarily a successful socket flush
  or peer receipt. Do not invent CONNECT/AUTH/other history that Rust did not emit.
- [ ] Keep AUTH response authority with the configured authenticator. AUTH
  lifecycle observations remain supported; adding raw AUTH contents requires an
  explicit selection and must never create competing challenge-response tokens.
- [ ] Prefer delivery through the existing bounded event channel with its
  explicit overflow failure contract. Define ordering relative to existing events
  and completions, avoid duplicate native polling or completed-packet history,
  and do not promise cross-channel total ordering. If another channel is required,
  specify bounded loss/accounting and teardown before implementation.
- [ ] Retain owned immutable packet details only for delivered event lifetimes,
  sharing storage with terminal outcomes when practical. Keep the disabled path
  free of added property cloning and history allocation. Expose no mutable packets,
  raw ACK submission, or exact original wire encoding promise.
- [ ] Redact packet properties and AUTH data from Debug, formatted errors, and
  automatic logs. Explicit accessors may return event-owned borrowed data.

## Verification and completion

- [ ] Consult both spec digests in `docs/spec/`; verify exact QoS 1/2 incoming and
  outgoing packet kinds, intermediate success and rejection, QoS 2 replay, and
  incoming PINGRESP with deterministic native C consumers.
- [ ] Verify existing default event sequences and terminal ACK results, opt-in
  filtering, retained views, reconnect generation, full queues, shutdown, and
  shared-worker progress. Do not infer flush timing from outgoing events.
- [ ] Measure disabled and enabled allocation/retention costs without adding
  unbounded history. Exercise property redaction and event ownership after close.
- [ ] Update header/exports, C examples, READMEs, `PARITY.md`, and `CHANGELOG.md`;
  run core/C feature-sensitive suites and ABI compatibility checks.

Complete when C can opt into native protocol observations with exact kinds and
available contents, without changing normal traffic, protocol ownership, or ACK results.
