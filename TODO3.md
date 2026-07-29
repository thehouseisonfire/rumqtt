# MQTT 5 Session Recovery Boundaries

## Goal

Complete deterministic MQTT 5 session recovery across reconnects and process
restarts without replaying state from a session the broker did not resume.

## Required Work

- Define and implement every combination of Clean Start, Session Present,
  effective Session Expiry Interval, matching local checkpoint, and broker-only
  session state.
- Preserve original packet identifiers and correct DUP behavior only when the
  MQTT session survives.
- Cover outgoing QoS 1 and QoS 2 exchanges, PUBREL, incoming QoS 2 state,
  SUBSCRIBE, and UNSUBSCRIBE.
- Apply CONNECT, CONNACK, and client DISCONNECT Session Expiry Interval values
  with their correct lifetimes and precedence.
- Clear incompatible live and persisted state before admitting new work.
- Keep strict broker-only resume rejection as the default; isolate and document
  any explicit compatibility policy.

## Tests

- Add a table-driven boundary matrix for live reconnect and process restart.
- Cover graceful disconnect, transport loss, expiry zero, expiry override,
  changed Client Identifier, missing broker session, and persistence failure.
- Assert wire-level packet identifiers, DUP flags, replay order, and checkpoint
  contents.

## Completion Criteria

- Every matrix cell has an explicit expected outcome and automated coverage.
- A missing broker session can never cause old packet identifiers to be replayed
  as resumed work.
- Targeted reliability tests and `cargo test -p rumqttc-v5-next` pass.
- The supported recovery contract is documented in `rumqttc-v5/design.md` and
  any user-visible changes are added to `CHANGELOG.md`.
