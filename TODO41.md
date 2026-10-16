# C Wrapper Deferred Command Admission

Status: planned.

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
Readiness integration is not a prerequisite. [TODO48.md](TODO48.md) can reduce
retained ACK storage but is not required for the initial event-based path.

## Implementation requirements

- [ ] Define separate states for waiting, admitted, cancelled before admission,
  and failed. Successful observation returns the admitted operation's existing
  completion handle and operation ID, with repeatable results. Each observation
  returning a completion supplies an independently owned reference to the same
  operation. Creating a pending handle accepts ownership for an admission attempt;
  it does not mean MQTT admission. Do not assign an MQTT operation ID to the
  waiting handle or report delivery while it is waiting. Retained terminal results
  remain observable after client destruction without retaining live execution.
- [ ] Add finite, configurable per-client pending-count and retained-byte limits,
  shared by all producers and independent of native queue and MQTT publish
  budgets. Specify defaults, accounting, overload errors, and when reservations
  are released. Use checked size preflight and reserve capacity before copying
  inputs or retaining owners; failed construction rolls back every reservation.
  Charge payloads, topics, properties, filter collections and their variable
  metadata, plus any event data retained for ACK authority. Bound fixed overhead
  through the count limit and document transient copies and caller-retained
  terminal results separately. An individually oversized or intrinsically invalid
  request fails during creation, without entering the waiting set.
- [ ] Copy inputs and retain required owners only after pending capacity is
  reserved. Release command data and unnecessary owners promptly on every terminal
  path; release reservations only after the storage they cover is released.
  Transferring a publish into native admission must acquire the existing native
  budgets rather than bypassing them. Pending storage is process-local and adds
  no durability guarantee or unlimited offline queue.
- [ ] Reuse cancellation-safe core admission primitives. Serialize cancellation
  with the exact native admission boundary and publication of the admission
  result. A confirmed cancellation proves no admission; if admission wins, return
  the existing operation and completion. A task-abort request alone is not proof
  of non-admission. Cancellation requests must be nonblocking and idempotent; if
  arbitration is pending, expose that state and let poll/timed observation obtain
  the authoritative terminal result. Preserve a winning terminal failure.
  Do not cancel an admitted MQTT operation or an active native poll.
- [ ] Observer timeout never cancels work or becomes a cached terminal result.
  Releasing one retained pending handle affects only that observer. Releasing
  the last external owner, including any readiness registration retaining the
  source, requests cancellation while waiting without blocking destruction.
  If admission wins that race, the admitted operation continues normally.
  No destruction or timeout path may claim non-admission without arbitration.
- [ ] Add explicit ownership of pending futures/tasks to both dedicated and shared
  managed execution; the current driver/auxiliary cleanup tracking is a foundation,
  not an existing general admission-task facility. Close task registration with
  client shutdown, settle all waiting operations on terminal failure, wake their
  observers, and release task-owned inputs and registration owners before client
  join succeeds. Prevent retained handles or task/client ownership cycles from
  keeping execution alive after teardown. No per-request OS thread, caller-runtime
  dependency, or untracked detached task is permitted.
- [ ] Retain generation-bound ACK authority while waiting and restore retry
  availability on cancellation or failure before admission, provided the generation
  remains valid. Coordinate with [TODO48.md](TODO48.md); retaining an event is an
  acceptable compatibility path if its retained data is charged. Serialize pending
  and immediate ACK aliases through one authority; successful admission consumes
  it once. Never hold an event/authority mutex across an asynchronous wait.
- [ ] Preserve progress across producers and shared-runtime peers with bounded
  scheduling work. Poll independent admission futures without allowing a publish
  waiting for capabilities or budget to block eligible ACK/control work behind it.
  Specify that creation order does not guarantee admission order; current core
  notification-driven retries do not establish FIFO admission. Exercise contention
  without promising starvation freedom or latency bounds unsupported by core.
- [ ] Honor shutdown, recovery, and ordered fences through the same core gate as
  immediate admission. A request still waiting when an ordered fence commits is
  outside its admitted prefix and must fail without admission. Do not drain the
  pending set into that prefix or silently resubmit across a recovery boundary.
- [ ] Add opaque C handles, retain/release, poll, timed observation, and explicit
  cancellation using [the C ABI policy](docs/c-abi-compatibility.md), with additive
  APIs and versioned configuration records. Reject blocking waits on
  execution workers and in host callbacks; initialize failure outputs consistently.
- [ ] Document that hosts must continue draining events during admission waits.
  Waiting does not drive event consumption or relax event-backpressure termination.
  Include a bounded producer example with independent event consumption and show
  pending creation, admission observation, then MQTT completion observation.

## Verification and completion

- [ ] Test both protocols with full queues, disconnected MQTT 5 capability waits,
  exhausted count/byte budgets, restoration, and oversized requests that must
  fail during creation. Race concurrent producers against pending limits; verify
  rejection precedes copying/owner retention, checked accounting includes metadata
  and retained ACK events, and failed construction releases reservations.
- [ ] Deterministically force cancellation to race native admission and result
  publication; assert one stable terminal outcome, no duplicate packets, correct
  ACK authority, and bounded owner release. Cover cancellation repeated before and
  after admission/failure, observer timeout, individual/final owner release,
  repeated and concurrent observation, and independent completion ownership.
- [ ] Test shutdown, terminal failure, recovery and ordered-fence races while
  waiting, including exclusion from a fence's admitted prefix. Race task registration
  with client destruction and context shutdown on dedicated and shared execution;
  successful join must leave no pending task-owned data or executable work.
  Retained terminal handles must remain readable without retaining the runtime.
- [ ] Run native C producers alongside event consumption and shared-runtime
  peers; prove bounded pending storage and continued keepalive/ACK progress under
  sustained contention. Keep a publish capability/budget wait pending while an
  eligible ACK/control operation proceeds. Verify a stalled event consumer still
  triggers the existing event-backpressure contract and wakes pending observers.
- [ ] Update C header/exports, admission examples, READMEs, `PARITY.md`, and
  `CHANGELOG.md`. Run relevant core/C suites, feature checks, and ABI consumers.

Complete when C can wait for the same admission conditions as wrapper-core,
with an unambiguous boundary between waiting, admission, and MQTT completion.
