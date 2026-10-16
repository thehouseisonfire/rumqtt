# C Wrapper Deferred Command Admission

Status: implemented and validated on Linux.

## Goal

Let C callers wait for MQTT command admission without polling, blocking an MQTT
worker, or confusing admission with protocol completion.

## Foundation and scope

`ClientHandle::admit_async()` already waits for request-channel capacity and,
for MQTT 5 publishes, capabilities, checkpoint restoration, and publish-budget
progress. The C helper in `native-wrappers/c/src/ffi.rs` uses `try_admit()` even
for tracked calls. Existing C calls must retain their immediate behavior.

Deliver a distinct, owned pending-admission operation for publish, subscribe,
unsubscribe, and acknowledgement. Reuse core admission rules and the managed
execution context. [TODO42.md](TODO42.md) supplies optional host readiness
integration; this feature must also work with polling and timed observation.
Readiness integration is not a prerequisite. [TODO48.md](TODO48.md) is implemented;
deferred ACKs require its detached tokens so waiting does not retain incoming events
or their publication storage. Existing immediate/event-based ACK APIs are unchanged.

## Implementation requirements

- [x] Define separate states for waiting, admitted, cancelled before admission,
  and failed. Successful observation returns the admitted operation's existing
  completion handle and operation ID, with repeatable results. Each observation
  returning a completion supplies an independently owned reference to the same
  operation. Creating a pending handle accepts ownership for an admission attempt;
  it does not mean MQTT admission. Do not assign an MQTT operation ID to the
  waiting handle or report delivery while it is waiting. Retained terminal results
  remain observable after client destruction without retaining live execution.
- [x] Add finite, configurable per-client pending-count and retained-byte limits,
  shared by all producers and independent of native queue and MQTT publish
  budgets. Specify defaults, accounting, overload errors, and when reservations
  are released. Use checked size preflight and reserve capacity before copying
  inputs or retaining owners; failed construction rolls back every reservation.
  Charge payloads, topics, properties, filter collections and their variable
  metadata, plus any event data retained for ACK authority. Bound fixed overhead
  through the count limit and document transient copies and caller-retained
  terminal results separately. An individually oversized or intrinsically invalid
  request fails during creation, without entering the waiting set.
- [x] Copy inputs and retain required owners only after pending capacity is
  reserved. Release command data and unnecessary owners promptly on every terminal
  path; release reservations only after the storage they cover is released.
  Transferring a publish into native admission must acquire the existing native
  budgets rather than bypassing them. Pending storage is process-local and adds
  no durability guarantee or unlimited offline queue.
- [x] Reuse cancellation-safe core admission primitives. Serialize cancellation
  with the exact native admission boundary and publication of the admission
  result. A confirmed cancellation proves no admission; if admission wins, return
  the existing operation and completion. A task-abort request alone is not proof
  of non-admission. Cancellation requests must be nonblocking and idempotent; if
  arbitration is pending, expose that state and let poll/timed observation obtain
  the authoritative terminal result. Preserve a winning terminal failure.
  Do not cancel an admitted MQTT operation or an active native poll.
- [x] Observer timeout never cancels work or becomes a cached terminal result.
  Releasing one retained pending handle affects only that observer. Releasing
  the last external owner, including any readiness registration retaining the
  source, requests cancellation while waiting without blocking destruction.
  If admission wins that race, the admitted operation continues normally.
  No destruction or timeout path may claim non-admission without arbitration.
- [x] Add explicit ownership of pending futures/tasks to both dedicated and shared
  managed execution; the current driver/auxiliary cleanup tracking is a foundation,
  not an existing general admission-task facility. Close task registration with
  client shutdown, settle all waiting operations on terminal failure, wake their
  observers, and release task-owned inputs and registration owners before client
  join succeeds. Prevent retained handles or task/client ownership cycles from
  keeping execution alive after teardown. No per-request OS thread, caller-runtime
  dependency, or untracked detached task is permitted.
- [x] Retain generation-bound ACK authority while waiting and restore retry
  availability on cancellation or failure before admission, provided the generation
  remains valid. Use [TODO48.md](TODO48.md)'s detached tokens; deferred creation
  reserves core authority without retaining event data. Serialize pending
  and immediate ACK aliases through one authority; successful admission consumes
  it once. Never hold an event/authority mutex across an asynchronous wait.
- [x] Preserve progress across producers and shared-runtime peers with bounded
  scheduling work. Poll independent admission futures without allowing a publish
  waiting for capabilities or budget to block eligible ACK/control work behind it.
  Specify that creation order does not guarantee admission order; current core
  notification-driven retries do not establish FIFO admission. Exercise contention
  without promising starvation freedom or latency bounds unsupported by core.
- [x] Honor shutdown, recovery, and ordered fences through the same core gate as
  immediate admission. A request still waiting when an ordered fence commits is
  outside its admitted prefix and must fail without admission. Do not drain the
  pending set into that prefix or silently resubmit across a recovery boundary.
- [x] Add opaque C handles, retain/release, poll, timed observation, and explicit
  cancellation using [the C ABI policy](docs/c-abi-compatibility.md), with additive
  APIs and versioned configuration records. Reject blocking waits on
  execution workers and in host callbacks; initialize failure outputs consistently.
- [x] Document that hosts must continue draining events during admission waits.
  Waiting does not drive event consumption or relax event-backpressure termination.
  Include a bounded producer example with independent event consumption and show
  pending creation, admission observation, then MQTT completion observation.

## Verification and completion

- [x] Test both protocols with full queues, disconnected MQTT 5 capability waits,
  exhausted count/byte budgets, restoration, and oversized requests that must
  fail during creation. Race concurrent producers against pending limits; verify
  rejection precedes copying/owner retention, checked accounting includes metadata
  and retained ACK events, and failed construction releases reservations.
- [x] Deterministically force cancellation to race native admission and result
  publication; assert one stable terminal outcome, no duplicate application
  submissions, correct ACK authority, and bounded owner release. Cover cancellation repeated before and
  after admission/failure, observer timeout, individual/final owner release,
  repeated and concurrent observation, and independent completion ownership.
- [x] Test shutdown, terminal failure, recovery and ordered-fence races while
  waiting, including exclusion from a fence's admitted prefix. Race task registration
  with client destruction and context shutdown on dedicated and shared execution;
  successful join must leave no pending task-owned data or executable work.
  Retained terminal handles must remain readable without retaining the runtime.
- [x] Run native C producers alongside event consumption and shared-runtime
  peers; prove bounded pending storage and continued keepalive/ACK progress under
  sustained contention. Keep a publish capability/budget wait pending while an
  eligible ACK/control operation proceeds. Verify a stalled event consumer still
  triggers the existing event-backpressure contract and wakes pending observers.
- [x] Update C header/exports, admission examples, READMEs, `PARITY.md`, and
  `CHANGELOG.md`. Run relevant core/C suites, feature checks, and ABI consumers.

Complete when C can wait for the same admission conditions as wrapper-core,
with an unambiguous boundary between waiting, admission, and MQTT completion.


## Implementation and validation evidence

The C API adds `rumqttc_client_publish_deferred`, `subscribe_deferred`,
`unsubscribe_deferred`, and `acknowledge_token_deferred`, plus owned pending-handle
retain/release, poll, timed observation and cancellation. The required observation
state distinguishes waiting, cancellation requested, admitted, cancelled and failed.
Every admitted observation can return an independently owned completion for the
same MQTT operation. Defaults are 1,024 pending requests and 16 MiB charged data,
with a size-versioned configuration record and coherent per-client usage snapshot.
Deferred ACKs use TODO48's detached tokens exclusively.

Wrapper-core composes one bounded admission supervisor alongside the pinned MQTT
driver in both execution modes. Independent jobs share the existing native admission
helpers and producer gate; a per-poll attempt budget and cooperative yields preserve
peer progress without promising FIFO order. Recovery epochs prevent submission
across an explicit recovery transaction. ACK reservations retain packet-key mapping
while waiting so retransmission aliases share one authority. Joining includes pending
cleanup; retained terminal handles own results without retaining execution.
Dedicated current-thread execution and its blocking workers now enforce the same
native-progress wait restrictions as shared execution.

Deterministic barriers race cancellation with native admission and result publication,
including a winning failure. Core/C tests cover concurrent pending reservations,
observer deadlines and ownership, copied caller inputs, independently owned completions,
retained admitted/failed results after client destruction, cancelled ACK retry,
retransmission authority, checkpoint loading, individual native oversize rejection,
count/byte budgets, explicit recovery epochs, ordered-fence exclusion, registration
racing joining, shared peers, keepalive/ACK progress and event-backpressure termination.
The native C fixture exercises all new APIs with two producers and independent event
consumption, for both protocols on dedicated/shared execution. The producer example
shows admission observation followed by MQTT completion observation.

Validation on Linux:

- Full core/C suites passed with default features and `--no-default-features`:
  `cargo test --manifest-path native-wrappers/Cargo.toml -p rumqttc-wrapper-core-next
  -p rumqttc-c-next`, with and without the latter flag.
- Full C suites also passed with `--no-default-features --features
  ordered-shutdown,tracing-log-compat`. All nine new C admission scenarios passed;
  the final retained-result assertion was rechecked with the targeted suite.
- The CI core feature matrix passed all 19 profiles using `cargo hack test
  --manifest-path native-wrappers/Cargo.toml --each-feature --exclude-all-features
  -p rumqttc-wrapper-core-next`. Earlier profiles were rechecked after the final
  ownership changes; the final full default/no-default suites include the dedicated
  blocking-worker regression.
- Seven affected native consumers passed: native integration, detached ACK tokens,
  publish admission, offline publish admission, deferred admission, and the deferred
  admission/manual acknowledgement examples.
- `native-wrappers/c/tests/abi/check.sh all` passed generated/header consistency,
  exports, C11/C++17 consumers and relocated pkg-config consumers. Optional error-output
  coverage passed for all 238 APIs.
- `cargo check --manifest-path native-wrappers/Cargo.toml --workspace` passed,
  including the JavaScript and Python crates.
- Core/C Clippy passed for all targets with the composed ordered-shutdown/tracing
  profile and `--no-deps -- -D warnings -W clippy::pedantic -W clippy::nursery`,
  retaining explicit exceptions for the existing `clippy::double_must_use` in
  `completion.rs` and `clippy::option_if_let_else` in `backend/mod.rs`. Those unrelated
  implementations were not changed.
- Rust formatting, optional error-output coverage, and `git diff --check` passed.

macOS/Windows native execution remains in the existing CI platform matrix.
TODO42 host readiness notifications remain separate work.
