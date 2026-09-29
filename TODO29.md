# C Wrapper Complete Broker Acknowledgement Results

## Goal

Preserve MQTT 5 broker acknowledgement contents in tracked operation results:
packet kind and identifier, exact reason codes, Reason String, User Properties,
and per-filter results. Expose recovery distinctions without changing success
or failure classification.

## Current foundation and feasibility

Rust notices in `rumqttc-v5/src/notice.rs` return complete PUBACK, PUBREC,
PUBCOMP, SUBACK, and UNSUBACK values. Wrapper-core's mappings in
`src/backend/v5.rs` reduce these to coarse completions or broker reason errors.
For example, successful PUBACK `NoMatchingSubscribers` loses its distinct code.

This is feasible through richer owned operation outcomes. It does not require
publishing a second raw packet stream or giving applications QoS-state control.
Use `docs/spec/mqtt-v5.0.md` and its requirement index to preserve legal packet
properties and their optional presence.

## Implementation requirements

- [ ] Add owned acknowledgement details to wrapper-core results and preserve
  them before the backend mapping discards the native packet.
- [ ] Keep details available on successful and broker-rejected operations.
  Design a tagged terminal outcome/detail accessor or an equivalent owned
  object; failures must not destroy the acknowledgement payload.
- [ ] Retain exact successful reason distinctions and recovered QoS 2 outcomes.
  Represent absent packets explicitly for QoS 0 and locally failed operations.
- [ ] Preserve native per-filter order and cardinality for SUBACK/UNSUBACK,
  ordered duplicate User Properties, and present-empty Reason Strings.
- [ ] Add additive C completion/detail accessors and, where useful, copy APIs.
  Borrowed views remain valid for the documented owner lifetime, including
  after client destruction. Never return pointers into transient native events.
- [ ] Preserve existing completion kinds, per-filter accessors, error categories,
  and delivery status. New details supplement existing behavior.
- [ ] Define available v4 details explicitly without inventing v5 reason fields
  or per-filter UNSUBACK results that v4 does not carry.
- [ ] Bound retained data by actual packet limits and operation ownership. Avoid
  duplicating large properties into both events and completions unnecessarily.
- [ ] Keep response properties out of automatic logs; expose them as explicit
  caller-owned observations, not as a new authentication authority.
- [ ] Document which intermediate QoS packets remain internal. Full raw packet
  tracing is a separate scope from terminal operation results.

## Verification and completion

- [ ] Round-trip exact acknowledgement contents for successful and rejected
  operations, mixed subscription results, and QoS 2 recovery.
- [ ] Verify retention after client destruction, copy after owner destruction,
  wrong-kind access, absent fields, and initialized outputs on accessor failure.
- [ ] Update C header/exports, examples, README, root `CHANGELOG.md`, and
  `PARITY.md` to distinguish coarse results from complete acknowledgements.

Complete when terminal broker information observable through Rust notices is
also accessible through C without reading a parallel raw-event channel.
