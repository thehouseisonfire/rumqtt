# MQTT 5 Session Recovery Boundary Follow-up

## Goal

Finish the automated coverage required by `TODO3.md` and prove that MQTT 5
session recovery behaves deterministically across both live reconnects and
process restarts. The implementation must never replay local state as a resumed
session when the broker reports that no session survived.

The underlying recovery implementation, design documentation, and changelog
already cover much of the intended behavior. This follow-up is specifically
about closing the remaining coverage and verification gaps rather than adding a
second recovery mechanism.

## Required Work

### 1. Define the recovery boundary matrix explicitly

Add a table-driven test model whose cases explicitly describe at least:

- recovery mode: live reconnect or process restart;
- Clean Start: `true` or `false`;
- broker Session Present: `true` or `false`;
- effective Session Expiry Interval: zero or nonzero;
- local state: matching checkpoint, missing checkpoint, or incompatible
  checkpoint;
- broker state: resumed session or no surviving session;
- connection closure: graceful DISCONNECT, abrupt transport loss, or
  graceful-disconnect timeout;
- Session Expiry source: CONNECT value, CONNACK override, or client DISCONNECT
  override;
- Client Identifier: unchanged or changed; and
- persistence result: success or injected load/save/clear failure.

Do not generate meaningless Cartesian-product cases merely to increase the
case count. Represent invalid or protocol-contradictory combinations explicitly
and assert the expected rejection. Give every supported boundary a named case
and a complete expected outcome.

Each case must state whether the client should:

- accept or reject the CONNACK;
- resume or reset live MQTT state;
- load, save, retain, or clear the checkpoint;
- replay packets or admit only fresh work;
- preserve or release packet identifiers; and
- surface a persistence or session-state error.

### 2. Complete live-reconnect coverage

Extend the MQTT 5 reliability coverage beyond the existing individual happy
paths. Exercise the matrix through an actual broker connection wherever the
expected behavior is observable on the wire.

At minimum, cover:

- `Clean Start = 1` with `Session Present = 0`, proving old live and persisted
  state is cleared before fresh requests are admitted;
- protocol rejection of `Clean Start = 1` with `Session Present = 1`;
- `Clean Start = 0`, matching local state, and `Session Present = 1`, proving
  recovery;
- `Clean Start = 0`, matching local state, and `Session Present = 0`, proving
  reset without replay;
- strict rejection of `Session Present = 1` without matching local state;
- the explicit `AllowBrokerOnly` compatibility policy, including its packet-ID
  allocation restrictions;
- unchanged versus changed Client Identifier boundaries;
- graceful DISCONNECT, abrupt transport loss, and disconnect-timeout behavior;
- effective expiry zero versus nonzero; and
- CONNECT, CONNACK, and client DISCONNECT expiry precedence across subsequent
  reconnects.

Verify that incompatible state is cleared before the event loop processes new
application work. Include an injected clear failure and prove that stale state
cannot be loaded or replayed while invalidation remains pending.

### 3. Complete process-restart coverage

Replace or extend the current single MQTT 5 file-backed QoS 1 restart scenario
with a table-driven MQTT 5 restart suite. Keep subprocess isolation so the test
continues to prove recovery across a real process boundary.

At minimum, add restart scenarios for:

- resumed and missing broker sessions;
- effective expiry zero and nonzero;
- graceful DISCONNECT and abrupt process/transport termination;
- CONNACK and client DISCONNECT expiry overrides;
- unchanged and changed Client Identifiers;
- checkpoint load, save, and clear failures where the file-store test harness
  can inject them deterministically;
- outgoing QoS 1 PUBLISH;
- outgoing QoS 2 before PUBREC;
- outgoing QoS 2 after PUBREC, represented by PUBREL;
- incomplete incoming QoS 2 state;
- pending SUBSCRIBE; and
- pending UNSUBSCRIBE.

When the restarted broker response has `Session Present = 0`, assert that no
packet from the old checkpoint appears on the wire. Then submit fresh work and
prove that it does not inherit an old packet identifier merely because that
identifier existed in the discarded checkpoint.

### 4. Assert protocol details, not only high-level events

For every replay-capable case, decode the actual MQTT frames and assert:

- the exact packet type;
- the original packet identifier when the session survives;
- `DUP = 1` for replayed QoS 1 and QoS 2 PUBLISH packets;
- `DUP = 0` for uninterrupted first transmissions;
- the correct PUBREL representation;
- deterministic replay ordering across mixed PUBLISH, PUBREL, SUBSCRIBE, and
  UNSUBSCRIBE state; and
- absence of any stale replay when the broker reports no session.

Assert the durable checkpoint after each meaningful transition. Check its
replay entries, incoming QoS 2 state, effective expiry metadata, identity and
scope compatibility fields, and removal or retention as appropriate. Avoid
tests that pass solely by observing an `Event::Outgoing` value without checking
the corresponding wire packet when wire behavior is part of the contract.

### 5. Cover persistence failures at each durability boundary

Use deterministic fault-injecting `SessionStore` implementations or supported
file-store fault hooks. Cover failures while:

- loading a checkpoint before connection;
- saving admitted outgoing recovery state;
- saving acknowledgement or terminal transitions;
- recording a DISCONNECT expiry override; and
- clearing state after a fresh broker session or zero-expiry termination.

For each failure, assert the returned error, notice completion behavior,
in-memory ownership state, retry behavior, and durable checkpoint contents.
The client must fail closed: it must not acknowledge durability it did not
achieve, reuse incompatible state, or admit work behind a failed mandatory
clear.

## Test Organization

- Put broker-backed live reconnect cases in
  `rumqttc-v5/tests/reliability.rs`, extracting reusable case definitions and
  broker helpers when that keeps the matrix readable.
- Put true process-boundary cases in
  `session-store-file/consumer-tests/tests/process_restart.rs` or a focused
  sibling integration-test module in that workspace.
- Keep small state-machine invariants as unit tests near `eventloop.rs` or
  `state.rs`, but do not use unit tests as a substitute for the required
  wire-level and process-restart coverage.
- Prefer typed case enums and explicit expected-result structs over boolean
  tuples. Case names should identify the boundary and expected behavior when a
  failure is reported.
- Reuse the existing packet codecs and session-store abstractions. Do not add
  sleeps for synchronization when a protocol frame, channel, or bounded timeout
  can make the test deterministic.

## Documentation

After the matrix establishes the supported behavior:

- update `rumqttc-v5/design.md` if any tested boundary is not already described
  precisely;
- update `CHANGELOG.md` for any user-visible correction or clarified guarantee;
  and
- keep strict broker-only resume rejection as the default, documenting
  `AllowBrokerOnly` only as an explicit compatibility policy with its safety
  restrictions.

## Completion Criteria

- Every meaningful matrix cell has a named expected outcome and automated
  coverage for live reconnect, process restart, or both where applicable.
- The restart suite covers all recovery-state classes: outgoing QoS 1, outgoing
  QoS 2 PUBLISH, PUBREL, incoming QoS 2, SUBSCRIBE, and UNSUBSCRIBE.
- Missing broker state, Clean Start, zero expiry, changed identity, and
  incompatible checkpoints never cause stale packets or old packet identifiers
  to be replayed as resumed work.
- Packet identifiers, DUP flags, replay order, wire absence, and checkpoint
  contents are asserted directly.
- Persistence load/save/clear failures are deterministic and prove fail-closed
  behavior.
- `cargo test -p rumqttc-v5-next --test reliability -- --nocapture` passes.
- `cargo test -p rumqttc-v5-next` passes.
- `cargo test --manifest-path session-store-file/Cargo.toml --workspace` passes.
- Relevant formatting and feature-sensitive checks pass for every changed
  workspace.
