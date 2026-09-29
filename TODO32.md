# C Wrapper MQTT 5 Publish Admission Policy

## Goal

Make MQTT 5 publish admission configurable between native event-loop validation
with offline queueing and strict validation against negotiated capabilities.
Provide bounded offline admission without inventing an application outbox.

## Current foundation and feasibility

The client builder accepts `PublishAdmissionPolicy` in
`rumqttc-v5/src/client.rs`; the enum lives in `src/publish_admission.rs`.
Wrapper-core hardcodes `RequireNegotiatedCapabilities` in `backend/v5.rs`.

This is a direct configuration addition. The wrapper must audit alias admission
and error mapping for both policies rather than assuming the strict path's
behavior is universal. Keep the wrapper's existing strict default.

## Implementation requirements

- [ ] Add an explicit v5 configuration selector in wrapper-core and an additive
  C setter. Map it directly to the native builder policy.
- [ ] Preserve strict behavior before CONNACK and while reconnecting: QoS 1/2,
  retained, and alias-bearing publishes require known capabilities; ordinary
  alias-free non-retained QoS 0 remains admissible.
- [ ] In event-loop-validated mode, admit eligible offline publishes into the
  existing bounded native channel and report later negotiated rejection through
  tracked completion. Admission must not be reported as broker acceptance.
- [ ] Keep intrinsic packet validation eager in both modes. Clearly distinguish
  permanent local validation, negotiated rejection, temporary unknown
  capabilities, and channel capacity exhaustion.
- [ ] Preserve the native topic-alias mapping and replay contract. Audit
  alias-only publishes before connection and across cleanup boundaries; reject
  unrecoverable replay with the native reason instead of repairing it in C.
- [ ] Audit `try_*` and tracked admission for both policies so callers have a
  reliable retry signal and dropped observers never cancel admitted work.
- [ ] Keep memory bounded and forbid policy changes with a live client until a
  native transition contract exists. Do not implement a hidden secondary queue.
- [ ] Reject the v5 policy selector on v4 rather than ignoring it.
- [ ] Document that offline admission is process-local queueing, not a guarantee
  of durable persistence before protocol processing. Session checkpoints remain
  recovery state rather than an application outbox.

## Verification and completion

- [ ] Before first CONNACK and during reconnect, compare both policies using
  QoS 0/1/2, retained packets, topic aliases, and full channels in native C.
- [ ] Verify later rejection when the broker advertises lower QoS or no retain,
  and exact terminal outcomes after session loss or alias replay failure.
- [ ] Demonstrate offline queueing in an example and document its memory and
  durability boundaries alongside strict retry behavior.
- [ ] Update C header/exports, configuration docs, `PARITY.md`, and root
  `CHANGELOG.md` without changing existing defaults.

Complete when C callers can select either native admission policy and observe
the correct distinction between local admission and MQTT completion.
