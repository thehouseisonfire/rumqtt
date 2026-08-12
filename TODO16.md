# Native Wrapper Coordinator Boundaries

## Goal

Tighten the internal coordinator boundaries in `rumqttc-wrapper-core` without
changing public Rust or C APIs, MQTT behavior, admission and completion
guarantees, shutdown outcomes, or event delivery.

Prefer narrow, behavior-oriented crate-private methods over direct access to
coordinator fields. Preserve the explicit MQTT v4 and v5 poll loops and keep
their scheduling behavior easy to review.

## Shared handle state

Make the fields of the handle's shared state private to `handle.rs`. Provide
focused methods for the operations required by runtime and adapter code,
including:

- lifecycle observation and progress notification;
- admission-gate coordination with connection invalidation and shutdown;
- protocol-specific ACK preparation without exposing the protocol client;
- operation registration and terminal failure; and
- shutdown commitment and terminal publication.

Do not add a general accessor for the protocol client, admission gate,
operation registry, shutdown coordinator, or acknowledgement coordinator.
Methods should express the invariant being enforced rather than merely return
an internal field.

The shared handle state may retain references to the focused operation,
shutdown, and acknowledgement coordinators. It must not duplicate their maps
or state machines.

## Shutdown reconciliation

Move shutdown terminal reconciliation behind a focused
`ShutdownCoordinator` API. The coordinator must remain the sole authority for:

- interpreting the committed graceful or immediate shutdown intent;
- determining the public shutdown completion, if any;
- resolving or failing the shutdown operation exactly once;
- reconciling graceful commitment with immediate escalation; and
- publishing the terminal lifecycle state and waking capacity waiters.

Runtime orchestration may drain protocol completion futures, fail unrelated
unfinished work, invalidate acknowledgements, and join the driver thread. It
must not inspect a shutdown disposition or assemble the final shutdown
operation result itself.

Keep shutdown records and phase details private. Expose decisions using narrow
semantic methods rather than numeric shutdown-kind values or a structure that
mirrors the internal record.

## Acknowledgement boundary

Remove runtime-level forwarding helpers whose only purpose is to call the
acknowledgement coordinator. Connection invalidation, prepared-ACK insertion,
and tracked-ACK completion should pass through focused shared-handle methods or
the acknowledgement coordinator where that coordinator is already the natural
owner.

Preserve serialization between ACK token creation or invalidation and command
admission. Do not expose acknowledgement maps, reservation state, connection
generations, or the coordinator itself to adapter code.

## Adapter-local operation scheduling

Keep each adapter's `FuturesUnordered` pending queue and its associated
driver-local lookup state beside the explicit `tokio::select!` loop. These are
poll-loop scheduling state, not shared operation-registry state.

`operations.rs` remains responsible for:

- operation-ID allocation;
- completion-cell registration and exactly-once resolution;
- completion and diagnostics registration messages; and
- pure or focused helpers used to accept, resolve, drain, or fail driver-local
  pending operations.

Do not hide the v4 and v5 poll loops behind a protocol-driver trait,
`async-trait`, dynamic dispatch, or a macro. Each loop must retain one pinned
`EventLoop::poll` future while servicing wrapper control branches. Selection
must remain fair and non-biased for MQTT progress, completion registration,
diagnostics, and completed notices, with priority only for terminal or
immediate-shutdown paths.

## Dependency and safety rules

- Do not add dependencies or change feature flags.
- Keep Flume, Tokio, and `FuturesUnordered` in their current roles.
- Do not introduce another broad shared mutable state object.
- Do not duplicate operation, shutdown, or acknowledgement state machines.
- Do not hold a lock while awaiting MQTT work, waiting for completion,
  delivering an event, or joining the driver thread.
- Preserve one immutable protocol selection per client.
- Preserve cancellation-safe async admission and strict MQTT 5 negotiated
  publish admission.
- Preserve exactly-once terminal resolution for every admitted operation.
- Preserve one caller timeout budget across close coordination, completion,
  and thread joining.
- Preserve the bounded ordinary event path, the capacity-independent terminal
  path, and immediate-shutdown bypass of obstructed event delivery.
- Preserve ACK generation, rollback, invalidation, and single-use semantics.
- Preserve error kind, delivery status, broker reason, and diagnostics fidelity.

## Implementation sequence

1. Add behavior-oriented shared-handle methods needed by the adapters and
   runtime.
2. Replace direct shared-field access and make all shared-state fields private.
3. Add semantic shutdown reconciliation methods and remove exposed disposition
   and numeric-kind inspection.
4. Remove runtime ACK forwarding helpers and route ACK behavior through the
   focused boundary.
5. Keep driver-local pending queues in the adapters while minimizing the
   visibility of their operation helper types.
6. Move or add unit tests alongside the coordinator whose invariant they
   exercise.
7. Run the complete verification matrix.

Keep each step compiling and behavior-preserving. Move an invariant and its
tests together; do not temporarily duplicate mutable state.

## Tests and verification

Add focused tests for newly introduced interfaces where existing public-API
coverage does not exercise their behavior. In particular, cover:

- graceful terminal reconciliation;
- immediate shutdown and graceful-to-immediate escalation;
- exactly-once shutdown-operation resolution;
- terminal lifecycle publication and waiter notification;
- ACK insertion, completion, rollback, and connection invalidation through the
  narrowed boundary; and
- fair progress of both explicit adapter loops under sustained completion and
  diagnostics traffic.

Run:

```text
cargo fmt --all -- --check
cargo check --workspace
cargo test -p rumqttc-core-next
cargo test -p rumqttc-v4-next
cargo test -p rumqttc-v5-next
cargo test -p rumqttc-wrapper-core-next
cargo test -p rumqttc-c-next
cargo hack --each-feature --exclude-all-features test -p rumqttc-v4-next -p rumqttc-v5-next
cargo hack clippy --each-feature --exclude-all-features --no-dev-deps -p rumqttc-v4-next -p rumqttc-v5-next
```

Also run the repository's C header, ABI, native integration/stress/example, and
package-consumer checks on their supported platforms.

## Completion criteria

This TODO is complete when:

- shared handle-state fields are private and consumers use narrow semantic
  methods;
- shutdown intent, escalation, final operation outcome, and lifecycle
  publication are reconciled through `ShutdownCoordinator` APIs;
- adapters do not access the protocol client or focused coordinators directly;
- runtime contains no acknowledgement forwarding helpers or shutdown-record
  interpretation;
- adapter-local pending queues remain explicit and both pinned poll loops retain
  their fairness and cancellation guarantees;
- public Rust and C APIs and all behavioral invariants remain unchanged; and
- formatting, tests, feature matrices, Clippy, and C ABI/native/package checks
  pass.
