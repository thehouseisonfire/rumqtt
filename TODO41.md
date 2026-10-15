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

## Implementation requirements

- [ ] Define separate states for waiting, admitted, cancelled before admission,
  and failed. Successful observation returns the admitted operation's existing
  completion handle and operation ID, with repeatable results. Do not assign an
  MQTT operation ID or report delivery while the request is only waiting.
- [ ] Copy inputs and retain required owners when creating a pending operation.
  Bound pending count and retained bytes independently of native queue and MQTT
  publish budgets. Reject overload before retaining work; account for large
  payloads without imposing a hidden unbounded offline queue.
- [ ] Reuse cancellation-safe core admission primitives. Serialize cancellation
  with the exact native admission boundary: cancellation either proves no
  admission or reports the already admitted operation. Dropping or timing out an
  observer must have explicitly defined behavior; timed observation alone never
  cancels work. Do not cancel an admitted MQTT operation or an active native poll.
- [ ] Own and track pending tasks through shutdown and client joining. Terminal
  failure wakes all pending observers; cleanup releases inputs and registration
  owners. No per-request OS thread or untracked detached task is permitted.
- [ ] Retain generation-bound ACK authority while waiting and restore retry
  availability on cancellation before admission. Coordinate with
  [TODO48.md](TODO48.md); retaining an event is an acceptable compatibility path.
- [ ] Preserve fair progress across producers and shared-runtime peers. Document
  whether waiter order is guaranteed; do not promise fairness that the native
  admission primitives cannot provide. Honor shutdown, recovery, and ordered
  fences at the same boundary as immediate admission.
- [ ] Add opaque C handles, retain/release, poll, timed observation, and explicit
  cancellation using `docs/c-abi-compatibility.md`. Reject blocking waits on
  execution workers and in host callbacks; initialize failure outputs consistently.

## Verification and completion

- [ ] Test both protocols with full queues, disconnected MQTT 5 capability waits,
  exhausted count/byte budgets, restoration, and oversized requests that must
  fail immediately. Test shutdown and ordered-fence races while waiting.
- [ ] Force cancellation to race successful admission; assert exactly one
  outcome, no duplicate packets, correct ACK authority, and bounded owner release.
  Cover observer timeout/drop, repeated observation, and client destruction.
- [ ] Run native C producers alongside event consumption and shared-runtime
  peers; prove bounded pending storage and continued keepalive/ACK progress.
- [ ] Update C header/exports, admission examples, READMEs, `PARITY.md`, and
  `CHANGELOG.md`. Run relevant core/C suites, feature checks, and ABI consumers.

Complete when C can wait for the same admission conditions as wrapper-core,
with an unambiguous boundary between waiting, admission, and MQTT completion.
