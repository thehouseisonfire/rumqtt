# Native Wrapper Core Module Ownership

## Goal

Refactor `rumqttc-wrapper-core` so each runtime concern has one explicit module
owner. This is a behavior-preserving architectural change: do not alter public
Rust or C APIs, MQTT semantics, admission/completion guarantees, shutdown
outcomes, or event-delivery behavior.

Do not implement this as a file-only split around one broadly shared mutable
state object. Move the data and methods enforcing each invariant together, and
expose only narrow crate-private interfaces between modules.

## Required ownership

Use this module layout:

```text
rumqttc-wrapper-core/src/
├── lib.rs
├── command.rs
├── completion.rs
├── config.rs
├── error.rs
├── event.rs
├── protocol.rs
├── shutdown.rs
├── runtime.rs
├── handle.rs
├── operations.rs
├── acknowledgement.rs
└── adapter/
    ├── mod.rs
    ├── v4.rs
    └── v5.rs
```

### `handle.rs`

Own:

- `ClientHandle`, including clone/drop accounting;
- command dispatch and blocking, async, and nonblocking admission;
- admission ordering and lifecycle checks;
- the protocol-client enum used to submit commands; and
- the minimal shared handle state needed by producers.

The shared handle state may contain lifecycle observation, the admission gate,
the selected protocol client, progress notification, and references to focused
operation, shutdown, and acknowledgement components. It must not directly own
their maps or duplicate their state machines.

### `operations.rs`

Own:

- operation-ID allocation;
- the operation registry and completion-cell registration;
- driver-polled pending completion futures;
- diagnostics request registration and terminal resolution; and
- helpers that resolve or fail pending operations exactly once.

Completion value types and the public `CompletionHandle` remain in
`completion.rs`. Shutdown state does not move into the operation registry.

### `shutdown.rs`

Own:

- the coherent shutdown record and internal phase transitions;
- graceful commitment, immediate escalation, terminal reconciliation, and
  final operation outcomes;
- lifecycle publication associated with shutdown; and
- wake-up of capacity waiters on terminal transitions.

Provide narrow methods used by `ClientHandle`, the runtime, and protocol loops.
Do not expose the shutdown record or require callers to coordinate multiple
independent fields.

### `acknowledgement.rs`

Own:

- ACK keys, prepared protocol ACKs, and wrapper ACK tokens;
- connection-generation validation;
- reservation, rollback, insertion, invalidation, and single-use rules; and
- tracked ACK completion lookup and resolution.

Reservation guards and the acknowledgement maps belong here rather than in a
shared runtime structure.

### `runtime.rs`

Own:

- `NativeClient` and `NativeClientCloser`;
- driver-thread start, bounded join, drop, and nonblocking abandonment
  ownership;
- caller-deadline-aware close coordination;
- `EventConsumer` and terminal-status publication;
- ordinary bounded event delivery and its timeout policy; and
- common driver startup and terminal cleanup orchestration.

Runtime code may call a selected adapter loop, but it must not contain MQTT
version-specific options, packet/event conversion, or poll-loop control flow.

### `adapter/v4.rs` and `adapter/v5.rs`

Each adapter owns its protocol's:

- client and `MqttOptions` construction, including authentication and transport
  translation;
- explicit event-loop poll/select loop;
- incoming, outgoing, diagnostics, and notice conversion;
- request/completion error mapping; and
- protocol-specific publish-property or reason-code translation.

`adapter/mod.rs` may define small common context types and pure helpers used by
both adapters. Do not introduce a protocol-driver trait, `async-trait`, dynamic
dispatch, or a macro hiding the poll/select loops merely to remove duplication.

### Boundary modules

`command.rs`, `completion.rs`, `config.rs`, `error.rs`, `event.rs`, and
`protocol.rs` own public values and stable wrapper semantics only. `lib.rs`
re-exports the public API from the module that owns it.

Remove `driver.rs` once its responsibilities have moved. If a common private
driver-orchestration module remains necessary, give it a narrow name and scope;
it must not become a renamed global owner.

## Dependency and state rules

- Keep private fields private to their owning module. Prefer focused methods to
  `pub(crate)` fields.
- Avoid ownership cycles. Runtime owns thread/event delivery; handles own
  producer admission; adapters own protocol loops; focused coordinators own
  their mutable state.
- No single replacement for `Shared` may contain lifecycle, shutdown records,
  acknowledgement maps, operation maps, and transport channels together.
- No lock may be held while awaiting MQTT work, waiting for completion,
  delivering an event, or joining the driver thread.
- Do not add dependencies or change feature flags for this refactor.
- Keep Flume, Tokio, and `FuturesUnordered` in their current roles.

## Behavioral invariants

The refactor must preserve all of the following:

- one immutable MQTT protocol selection per client;
- one dedicated current-thread Tokio runtime and joinable driver thread;
- one cloneable command handle and one owned event consumer;
- nonblocking, blocking, and cancellation-safe async admission;
- strict MQTT 5 negotiated publish admission where configured;
- distinction between admission and repeatably observable completion;
- exactly-once terminal resolution for every admitted operation;
- coherent graceful shutdown, immediate escalation, idempotent cleanup, and one
  caller timeout budget across close coordination, completion, and join;
- one bounded ordinary event channel with finite delivery timeout and no silent
  incoming-publish loss;
- a separate capacity-independent terminal-status path;
- immediate shutdown bypass of an obstructed ordinary event channel;
- ACK token generation, rollback, invalidation, and single-use semantics; and
- error kind, delivery status, broker reason, and diagnostics fidelity.

Each adapter loop must retain one pinned `EventLoop::poll` future while wrapper
control branches are serviced. Selection remains fair and non-biased for MQTT
progress, completion registration, and diagnostics, with priority only for
terminal or immediate-shutdown paths. A control wake-up must not cancel a poll
future that may already have dequeued or mutated protocol state.

## Implementation sequence

1. Move acknowledgement state and behavior behind a focused registry API.
2. Move operation allocation, registration, pending-future handling, and
   diagnostics completion behind the operation registry API.
3. Move shutdown state and transitions behind a shutdown coordinator API.
4. Move `ClientHandle`, protocol request dispatch, and the reduced shared handle
   state into `handle.rs`.
5. Move protocol construction, conversion helpers, and the complete v4/v5 poll
   loops into their respective adapters.
6. Move native ownership, close coordination, event consumption/delivery,
   startup, and terminal cleanup into `runtime.rs`.
7. Remove `driver.rs`, minimize crate-private visibility, and move unit tests
   alongside the responsible modules.
8. Run the complete verification matrix.

Keep each step compiling and behavior-preserving. Do not temporarily duplicate
mutable state between the old and new owners; move an invariant and its tests as
one unit.

## Tests and verification

Retain all unit and integration coverage while moving tests to their owning
modules. Add structural tests only where a newly introduced narrow interface
has behavior not covered through the public API.

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
package-consumer checks on their supported platforms. Update documentation only
if symbol locations, rustdoc links, or internal architecture descriptions become
stale; do not claim a user-visible behavior change for module movement alone.

## Completion criteria

This TODO is complete when:

- `driver.rs` no longer exists or is replaced by a narrowly scoped common
  orchestration module;
- native ownership, handle admission, operations, shutdown, acknowledgements,
  and each protocol adapter have the ownership described above;
- no broad shared mutable state bag or duplicate coordinator remains;
- v4 and v5 poll loops are explicit, separately reviewable, and preserve pinned
  poll and fairness guarantees;
- public Rust and C APIs and all behavioral invariants remain unchanged; and
- formatting, tests, feature matrices, Clippy, and C ABI/native/package checks
  pass.
