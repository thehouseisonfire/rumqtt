# Incoming PUBLISH Packet Identifier Observations

Status: complete (Linux x86_64 validation; platform and historical ABI limits below).

## Goal

Preserve the native incoming PUBLISH packet identifier through wrapper-core and
C for protocol correlation, without exposing packet-ID assignment or ACK injection.

## Foundation and scope

Both native `Publish` types contain `pkid`. The mappings in
`native-wrappers/wrapper-core/src/backend/v4.rs` and `v5.rs` omit it from
`IncomingPublish`; C therefore cannot observe it. ACK tokens already provide
client-bound, connection-generation-bound acknowledgement authority.

## Implementation requirements

- [x] Add an explicit optional packet identifier to the owned incoming publish
  observation. QoS 0 has no identifier; QoS 1/2 retain the exact native value.
  Treat an impossible zero for QoS 1/2 through native protocol validation rather
  than silently fabricating an identifier.
- [x] Expose an additive C accessor with separate presence and value outputs.
  Preserve existing publish accessors and record layouts under the ABI policy;
  wrong-kind and invalid-handle outputs follow existing initialization rules.
- [x] Define correlation scope: an identifier is not a globally unique message
  ID, durable deduplication key, or operation ID. Preserve a read-only connection
  generation on the observation where needed to distinguish reused identifiers;
  if unavailable before an accepted connection, expose absence explicitly.
  Even a client/generation/identifier tuple is not unique: identifiers can be
  reused after exchange completion within the same connection.
- [x] Retain metadata with the event so it remains readable after client
  destruction. Use the same representation in automatic and manual ACK modes;
  ACK availability is independent of identifier presence.
- [x] Keep acknowledgement authority exclusively in tokens. An observed integer
  must not be accepted as a substitute for the event/token or allow a stale ACK
  to enter a replacement connection.
- [x] Keep Python and JavaScript source compatibility with the core model change;
  new public accessors in those bindings are outside this C task. Audit exhaustive
  struct literals and event mappings rather than filling identifiers with defaults.

## Verification and completion

- [x] Check v4/v5 QoS 0/1/2, retransmissions, both ACK modes, and identifier reuse
  within one connection and after reconnect. Compare C-observed identifiers with
  deterministic broker input.
- [x] Test retained events after client destruction, wrong event kinds, optional
  outputs, and output initialization. Prove existing ACK token generation checks
  still reject stale and foreign-client acknowledgements.
- [x] Consult `docs/spec/mqtt-v3.1.1.md` and `docs/spec/mqtt-v5.0.md` for packet-ID
  scope; update C header/exports, README, `PARITY.md`, and `CHANGELOG.md`.
- [x] Run relevant protocol/core/C tests, native consumers, and ABI checks.

Complete when C observes the same incoming packet identifier as Rust, with its
limited correlation scope documented and acknowledgement authority unchanged.

## Implementation and validation

`IncomingPublish` owns `packet_id: Option<u16>` and
`connection_generation: Option<u64>`. Both mappings copy the native wire ID
according to QoS and snapshot the existing accepted-connection generation.
The additive `rumqttc_event_publish_packet_id()` and
`rumqttc_event_publish_connection_generation()` accessors preserve C layouts
and signatures. Exhaustive Rust struct literals need the new fields; Python
and JavaScript public event formats remain unchanged.

Validated on Linux x86_64:

- `cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next -p rumqttc-c-next`:
  418 passed, two existing ignored tests (manual diagnostics benchmark and Mosquitto Will test).
  Repeating with `--no-default-features`: 349 passed, the same two ignored tests.
- `RUMQTTC_TEST_EXECUTION=shared` with core `--test incoming_publish`: both tests passed.
  The new tests also passed on dedicated execution without default features.
- `cargo test -p mqttbytes-v4-next -p mqttbytes-v5-next`: 316 passed.
  JavaScript Rust `event::tests`: two passed; Python/JavaScript compiled in the
  native workspace check. Native fixture Python unit tests: 26 passed.
- Full CMake native consumers/examples: 76 passed, two skipped (logging and
  ordered-shutdown examples require their disabled features). Includes the new
  deterministic v4/v5, automatic/manual ACK observation consumer.
- `native-wrappers/c/tests/abi/check.sh all` passed, including C/C++ consumers,
  generated declarations, exports, and package checks. Local before/after ABI
  containment passed: exactly two functions added; public records identical.
  `compare-release.sh` reported no published baseline for `0.1.0-alpha`.
- Native workspace check, formatting, and whitespace checks passed. Clippy
  completed with the existing `double_must_use` warning in
  `wrapper-core/src/completion.rs:124`; strict `-D warnings` fails on that warning.

macOS/Windows execution and historical published-release ABI containment were
not verified locally. No release baseline was fabricated or changed.
