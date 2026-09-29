# C Wrapper Explicit Runtime Ownership and Shared Execution

## Goal

Let C applications run many clients on an explicitly owned shared execution
context, and support caller-driven progress where a sound Tokio integration can
be demonstrated. Preserve dedicated-thread startup as the existing default.

## Current foundation and feasibility

`native-wrappers/wrapper-core/src/runtime.rs` creates one current-thread Tokio
runtime and OS thread per client. Both native clients depend on Tokio I/O and
timers. [TODO10.md](TODO10.md) owns the execution-model investigation and
measurement criteria; this TODO defines the resulting C integration work.

Library-owned shared Tokio execution is feasible. A C host cannot hand in an
arbitrary executor and thereby satisfy Tokio's reactor contract. Caller-driven
execution requires a demonstrated bounded runtime-pump API; integration with a
foreign reactor requires an additional supported boundary. Do not claim either
capability merely because events can be received without blocking.

## Implementation requirements

- [ ] Complete the relevant TODO10 measurement and lifecycle decisions and
  record the selected shared-runtime or shard architecture. Reuse its work,
  rather than maintaining a competing execution-model investigation.
- [ ] Extract one owned driver future used by dedicated and shared execution.
  Keep packet mapping, reconnect, callback handling, fairness, and shutdown in
  that driver; do not copy protocol loops for each placement mode.
- [ ] Introduce opaque C execution-context handles with explicit construction,
  capacity/worker configuration, retain/release, and client attachment.
  Avoid an immortal process-global runtime.
- [ ] Define context ownership across configurations, clients, administrative
  work, pending callbacks, and task completion. Context shutdown must resolve
  active client outcomes before allowing code or owners to be unloaded.
- [ ] Preserve client panic containment and exactly-once terminal outcomes.
  Bound work per poll so one reconnecting or busy client cannot starve peers.
- [ ] Define close/join semantics for tasks separately from OS threads. A
  callback or context worker must never block waiting for its own task teardown.
- [ ] Keep existing bounded admission, event overflow, independent terminal
  observation, acknowledgement tokens, and completion semantics in every mode.
- [ ] Prototype an explicitly selected caller-driven context that retains
  pending driver futures between bounded pump calls. Never create/drop a native
  `poll()` future on each call: cancelled AUTH or I/O may not be resumable.
- [ ] For caller-driven mode, document thread affinity, exclusive pumping,
  timer progress, maximum wait, host starvation, and close progress. Require
  callers to drive cleanup instead of offering an impossible blocking join.
- [ ] Expose foreign event-loop wake handles only if I/O and timer wakeups can
  be delivered reliably using supported interfaces. A notification queue alone
  is not a Tokio reactor bridge. Otherwise offer bounded pumping and document
  that limitation, leaving reactor embedding for a later native boundary.

## Verification and completion

- [ ] Compare both protocols at 1, 10, 100, and 1,000 clients using TODO10's
  resource, throughput, fairness, and teardown methodology.
- [ ] Verify context shutdown, failed start, task panic, callback retention,
  repeated creation, and peer isolation without leaked tasks or workers.
- [ ] If caller-driven mode is exposed, prove pending AUTH, DNS, I/O, timers,
  and shutdown work across repeated bounded pump calls with a C consumer.
- [ ] Update C header/exports, capabilities, README, examples, `PARITY.md`, and
  root `CHANGELOG.md`; document execution guarantees for each shipped mode.

Complete when the selected shared mode is usable from C with explicit resource
ownership. Record caller-driven/foreign-reactor feasibility separately; do not
mark unsupported executor embedding complete as a side effect.
