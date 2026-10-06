# C Wrapper Explicit Runtime Ownership and Shared Execution

## Goal

Let C applications run many clients on an explicitly owned, library-managed
shared execution context. Ship one architecture selected through TODO10's
measurements, with dedicated-thread startup preserved as the existing default.

Caller-driven pumping and foreign-reactor integration are separate follow-up
feasibility work, not requirements for the first shared-context release.

## Current foundation and feasibility

`NativeClient::start` in `native-wrappers/wrapper-core/src/runtime.rs` retains
one current-thread Tokio runtime and OS thread per client. `start_in` places the
same owned driver future on an explicit shared execution context. Both native clients depend on Tokio I/O and
timers. [TODO10.md](TODO10.md) owns the execution-model investigation and
measurement criteria; this TODO defines the resulting C integration work.

An owned asynchronous driver future now shares terminal publication and panic
reconciliation across both placements, with separate task and runtime teardown.

The measured Linux resource benefit justifies opt-in shared execution for
many-client workloads. Dedicated execution remains available for its simpler
ownership and scheduling isolation. The decision and latency regressions are
recorded in [the execution comparison](native-wrappers/wrapper-core/benches/execution.md);
macOS/Windows measurements remain pending.

## Implementation requirements

- [x] Complete the relevant TODO10 measurement and lifecycle decisions and
  record the workload, measured benefit, and selected shared-runtime or shard
  architecture. Reuse its work rather than maintaining a competing investigation
  or shipping multiple speculative shared modes.
- [x] Use one owned driver future for dedicated and shared execution. Keep
  packet mapping, reconnect, callback handling, fairness, and shutdown in that
  driver; do not copy protocol loops for each placement mode. Prove the selected
  task placement's ownership and `Send` requirements for affected feature profiles.
- [x] Share terminal publication and panic reconciliation across placements.
  Define task cancellation and runtime loss explicitly: dropping or aborting a
  task must not bypass pending-operation reconciliation or terminal observation.
- [x] Introduce opaque C execution-context handles with explicit construction,
  capacity/worker configuration, retain/release, and client attachment.
  Avoid an immortal process-global runtime. Enforce client capacity across
  concurrent starts and release reservations on every failed-start path.
- [x] Define context ownership across configurations, clients, administrative
  work, pending callbacks, and task completion. Retain/release manages references;
  releasing a handle must not stop attached clients. Define final-owner cleanup
  without ownership cycles or dropping/joining the runtime on its own worker.
- [x] Separate context shutdown request, teardown observation, and reference
  release. Define open, closing, and execution-quiescent states; reject new
  attachments once closing begins and arbitrate starts racing shutdown. Specify
  how shutdown requests client close, coalesces repeated requests, and handles
  deadlines or escalation without changing existing client close semantics.
- [x] Resolve active client outcomes before reporting successful context teardown.
  Success also requires owned tasks, blocking work, workers, and cleanup to have
  stopped. A timeout or abandonment is not quiescence and must leave teardown
  observable/retryable; it must not authorize library unloading.
- [x] Document the separate lifetime of host-retained callbacks, tokens, streams,
  registrations, and configurations. Context teardown alone is not permission
  to unload code or free these owners. Preserve the existing requirement to
  release all independent owners and finish host work before unloading.
- [x] Preserve client panic containment and exactly-once terminal outcomes.
  Bound cooperative work per poll, including immediately ready MQTT and control
  paths. State that fairness depends on callbacks and destructors returning
  promptly; synchronous host work cannot be preempted by a timeout. Measure busy
  and reconnecting clients' effect on peers instead of promising hard latency bounds.
- [x] Configure or document blocking-pool limits and auxiliary work as well as
  scheduler workers. Worker count alone does not bound total threads or queued
  blocking work. Include DNS and deferred TLS work in resource and teardown accounting.
- [x] Define client task teardown separately from context worker joining. Closing
  one client must not stop peers. Reject blocking waits from callbacks or context
  workers when they depend on progress from that context, including waits for
  another client. Provide a nonblocking shutdown request/observation path.
- [x] Keep existing bounded admission, event overflow, independent terminal
  observation, acknowledgement tokens, and completion semantics in every mode.

## Verification and completion

- [ ] Compare both protocols at 1, 10, 100, and 1,000 clients using TODO10's
  resource, throughput, fairness, and teardown methodology. Include blocking-pool
  threads and supported operating systems; record regressions as well as benefits.
- [x] Verify capacity, shutdown/start races, failed start, task panic/cancellation,
  callback retention, release ordering, final-owner cleanup, and repeated creation
  without leaked tasks or workers. Verify timeout followed by successful teardown.
- [x] Verify peer progress under busy clients, reconnect storms, and slow but
  compliant callbacks; verify callback/worker blocking waits are rejected without
  deadlock. Exercise delayed blocking work and retained callback owners to prove
  that terminal client outcomes cannot be mistaken for complete teardown.
- [x] Retain dedicated-mode coverage and run existing completion, reconnect,
  overload, manual-ack, and shutdown suites against the shared mode for both
  protocols and affected transport/feature profiles, including real C consumers.
- [x] Update C header/exports, capabilities, README, examples, `PARITY.md`, and
  root `CHANGELOG.md`; document execution guarantees for each shipped mode.

Complete when one justified shared mode is usable from C with explicit resource
ownership, measured benefits and tradeoffs, and verified progress and teardown
contracts. Caller-driven execution and foreign-reactor embedding remain unsupported
unless separately demonstrated and documented.

## Optional follow-up: caller-driven execution and reactor integration

A C host cannot hand in an arbitrary executor and thereby satisfy Tokio's I/O
and timer contract. Repeated current-thread `Runtime::block_on` calls can resume
retained tasks, but this alone proves neither a bounded pump nor reactor embedding.
Do not claim either capability merely because events can be received without blocking.

Investigate this only for a concrete consumer requirement after selecting shared
execution. A prototype must:

- Retain pending driver futures between pump calls. Never create/drop a native
  `poll()` future on each call: cancelled AUTH or I/O may not be resumable.
- Define supported Tokio interfaces, thread affinity, exclusive/non-reentrant
  pumping, I/O and timer progress, and the meaning of each pump budget. Distinguish
  cooperative limits from hard wall-clock bounds; callbacks cannot be preempted.
- Document that host starvation stalls MQTT progress, deadline handling, reconnect,
  and cleanup. Require continued pumping through teardown and expose nonblocking
  close observation instead of a blocking join that depends on the caller pumping.
- Prove pending AUTH, DNS, I/O, timers, callbacks, and shutdown work across repeated
  pump calls with a real C consumer. Account for any auxiliary threads; caller-driven
  scheduling must not imply thread-free execution.
- Expose foreign event-loop wake handles only if both I/O and timer wakeups can be
  delivered reliably using supported interfaces. A notification queue alone is
  not a Tokio reactor bridge. If that boundary cannot be demonstrated, report the
  limitation and keep reactor embedding deferred; ship pumping only if independently
  justified and verified.

## Implementation status

The C API and wrapper-core context implementation, lifecycle regressions,
shared-placement behavioral suites, native ownership/unload consumers and
mixed-protocol example are implemented. Dedicated execution remains the default.
The Python shared selector exists only in `benchmark-testing` builds.
[Execution measurements](native-wrappers/wrapper-core/benches/execution.md)
record the architecture comparison and the remaining platform evidence. CI
runs shared behavior and real C/asyncio resource workloads on Linux, macOS and
Windows. Linux release measurements cover both protocols and 1/10/100/1,000
clients through real C and public asyncio consumers, including the private shard
comparison. Unexecuted macOS/Windows measurements and local WS/WSS listener
measurements remain outstanding evidence, even though their workflow and harness
are implemented. The platform-wide verification checkbox therefore stays open.
