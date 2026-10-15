# Incoming PUBLISH Packet Identifier Observations

Status: planned.

## Goal

Preserve the native incoming PUBLISH packet identifier through wrapper-core and
C for protocol correlation, without exposing packet-ID assignment or ACK injection.

## Foundation and scope

Both native `Publish` types contain `pkid`. The mappings in
`native-wrappers/wrapper-core/src/backend/v4.rs` and `v5.rs` omit it from
`IncomingPublish`; C therefore cannot observe it. ACK tokens already provide
client-bound, connection-generation-bound acknowledgement authority.

## Implementation requirements

- [ ] Add an explicit optional packet identifier to the owned incoming publish
  observation. QoS 0 has no identifier; QoS 1/2 retain the exact native value.
  Treat an impossible zero for QoS 1/2 through native protocol validation rather
  than silently fabricating an identifier.
- [ ] Expose an additive C accessor with separate presence and value outputs.
  Preserve existing publish accessors and record layouts under the ABI policy;
  wrong-kind and invalid-handle outputs follow existing initialization rules.
- [ ] Define correlation scope: an identifier is not a globally unique message
  ID, durable deduplication key, or operation ID. Preserve a read-only connection
  generation on the observation where needed to distinguish reused identifiers;
  if unavailable before an accepted connection, expose absence explicitly.
- [ ] Retain metadata with the event so it remains readable after client
  destruction. Use the same representation in automatic and manual ACK modes;
  ACK availability is independent of identifier presence.
- [ ] Keep acknowledgement authority exclusively in tokens. An observed integer
  must not be accepted as a substitute for the event/token or allow a stale ACK
  to enter a replacement connection.
- [ ] Keep Python and JavaScript source compatibility with the core model change;
  new public accessors in those bindings are outside this C task. Audit exhaustive
  struct literals and event mappings rather than filling identifiers with defaults.

## Verification and completion

- [ ] Check v4/v5 QoS 0/1/2, retransmissions, both ACK modes, and identifier reuse
  after reconnect. Compare C-observed identifiers with deterministic broker input.
- [ ] Test retained events after client destruction, wrong event kinds, optional
  outputs, and output initialization. Prove existing ACK token generation checks
  still reject stale and foreign-client acknowledgements.
- [ ] Consult `docs/spec/mqtt-v3.1.1.md` and `docs/spec/mqtt-v5.0.md` for packet-ID
  scope; update C header/exports, README, `PARITY.md`, and `CHANGELOG.md`.
- [ ] Run relevant protocol/core/C tests, native consumers, and ABI checks.

Complete when C observes the same incoming packet identifier as Rust, with its
limited correlation scope documented and acknowledgement authority unchanged.
