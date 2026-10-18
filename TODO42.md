# C Wrapper Readiness Notifications for Host Event Loops

Status: implemented behind the disabled-by-default `readiness` feature; Linux
correctness/ABI validation passed. The performance acceptance gate is not met,
and native macOS/Windows execution remains outstanding. TODO46 connection-observer
integration is implemented; its validation is recorded in TODO46.

## Goal

Let a C application integrate events and operation observations into its own
reactor without periodic polling or a blocking observer thread per client.

## Foundation and scope

Core completion, pending-admission, and connection cells already retain results
and have asynchronous wakeups. Event consumers asynchronously receive from
separate event and terminal channels; receiving consumes those observations.
C exposes polling and timed waits but no non-consuming readiness bridge.

Keep MQTT on dedicated or shared managed execution from [TODO33.md](TODO33.md).
Deliver client-event/terminal, MQTT-completion, and [TODO41.md](TODO41.md)
pending-admission readiness first. Include [TODO46.md](TODO46.md) first-connection
observer registrations through
the same immutable-result machinery; basic observer APIs remain independent.
Existing polling and timed-wait APIs remain independently usable.

The initial implementation uses OS wake primitives with no host callbacks,
notification delivery threads, additional runtime, or polling supervisor. It
does not add caller-driven MQTT pumping, callback scheduling, deadlines, or a
replacement Python/JavaScript bridge. The rejected
[explicit Python bridge ADR](native-wrappers/python/experiments/explicit_python_bridge/ADR.md)
is relevant evidence for keeping scheduling and lifecycle machinery small.

## Implementation requirements

- [x] Add an explicitly owned notification context, shareable across clients and
  operations, with one context usable per host event loop. Keep a constant number
  of OS signaling resources per context, independent of registration count; do
  not allocate a descriptor/handle per source. Define context and registration
  ownership, finite limits, exhaustion errors, and transactional registration.
- [x] Use a bounded registration table with durable ready bits or equivalent
  bounded state. Prefer scanning ready slots over a separate overflowing queue.
  Enforce both per-context registration limits and per-source subscriber limits,
  including subscriptions across contexts, so producer fan-out is bounded.
  Identify registrations with generations so slot reuse cannot misroute stale
  notifications or acknowledgements.
- [x] Define an explicit register/check/arm and drain/acknowledge/rearm protocol
  with durable readiness or source revisions that cannot lose wakeups. Enumerating
  ready sources and draining the OS primitive must not consume source results.
  Acknowledge the observed revision without erasing a newer publication. Returning
  to sleep must either prove no unacknowledged work remains or preserve a wakeup.
  Notifications are hints to inspect authoritative sources; duplicates and
  coalescing are harmless. Registration after readiness must report it immediately.
- [x] Distinguish recurring event readiness from immutable terminal results.
  Event readiness covers queued events and separately published terminal status;
  a partial drain must remain discoverable on rearm. Acknowledged immutable results
  must not repeatedly wake that registration merely because they remain readable.
  New registrations still observe those results. Pending-admission readiness means
  a terminal admission outcome, not a cancellation request or MQTT completion;
  callers register the returned completion separately after admission.
- [x] Instrument event and terminal publication without consuming either channel
  or holding the C event-consumer mutex. Do not implement readiness by running
  `EventConsumer::recv_async()` or a competing receiver. Cover successful event
  enqueue, terminal-only publication, and publication during teardown, including
  driver cancellation before its first poll.
- [x] Choose a portable base contract and explicit platform adapters: a pollable
  descriptor on supported POSIX hosts and a documented waitable Windows object,
  or equivalent supported mechanisms. Report adapter capabilities separately.
  A Windows HANDLE is not a POSIX socket descriptor; document which host wait
  mechanisms each adapter supports rather than promising universal integration.
- [x] Keep producer work bounded and nonblocking, without acquiring host-controlled
  locks or invoking host code on MQTT workers. A saturated OS signal primitive preserves durable
  readiness and must not fail MQTT or block its workers. Define context-local
  failure observation for other signaling errors. Any ready-queue optimization
  must retain a complete bounded-table rescan fallback when saturated.
- [x] Retain only the source observation ownership needed by each registration;
  avoid client/runtime retention and source/context ownership cycles. Retained
  completion, admission, and future connection results stay readable after client
  destruction. Define event-source invalidation and its wakeup on client destruction;
  retention does not authorize use of a destroyed C client or promise retained
  event consumption after destruction.
- [x] A pending-admission registration counts as an external observer under
  TODO41. Releasing the original pending handle keeps observation alive while
  registered. Deregistering the last external owner requests cancellation while
  waiting, using existing arbitration; it does not prove non-admission. Readiness
  acknowledgement and OS-signal draining never request cancellation, and releasing
  a completion registration never cancels admitted MQTT work.
- [x] Support explicit deregistration and context close with an observable
  quiescence boundary proving no future source signal can use retired resources.
  Reject new registrations once closing starts. Define close wakeups, in-flight
  signal arbitration, concurrent waiting/enumeration/rearm, and supported host
  drainer concurrency without relaxing the single event-consumer rule.
  Context-owned borrowed resources stay valid until their documented release
  boundary; hosts must remove them from wait sets before release. Source/client
  destruction must not close a retained context's descriptor/handle. Document
  duplicated-resource ownership and how close prevents late signal/resource reuse.
- [x] Preserve existing event-backpressure termination. A stalled host can still
  terminate its affected client; notification delivery cannot make it a progressing
  MQTT client. Peer clients must continue under shared execution. Readiness neither
  supplies MQTT scheduling nor changes admission or delivery guarantees.
- [x] Add versioned C APIs and capability discovery without changing existing
  record layouts, following [the C ABI policy](docs/c-abi-compatibility.md).
  Expose borrowed versus duplicated OS resources explicitly and preserve existing
  output initialization and forbidden-blocking-context rules.

## Verification and completion

- [x] Deterministically race readiness publication with registration, checking,
  enumeration, OS draining, acknowledgement, rearming, deregistration, slot reuse,
  and shutdown. Cover already-completed sources, partial event drains, immutable
  results after acknowledgement, full signal buffers, repeated/coalesced wakeups,
  stale acknowledgements, and terminal-only readiness. Prove no competing event
  consumption and no missed publication between checking and sleeping.
- [x] Verify count/fan-out limits, rollback on registration failure, bounded ready
  storage, and any overflow rescan. Test pending-handle release while registered,
  final deregistration racing admission, retained results after client destruction,
  and event-source invalidation without keeping execution alive.
- [ ] Exercise many clients/completions through real POSIX and Windows consumers.
  Demonstrate no lost work, bounded resources, and peer MQTT progress under a
  stalled host reactor, including the affected client's backpressure termination.
  Record actual platform execution separately from wiring; do not claim adapter
  support from cross-compilation alone.
- [x] Verify descriptor/handle ownership, unload safety, late signal rejection,
  concurrent context close/deregistration, and signal quiescence. Run C/C++ header
  and ABI checks. Successful client join alone does not authorize unloading while
  notification resources or host users remain live.
- [ ] Measure the unregistered publication path, active event/completion bursts,
  wake coalescing, host wake latency, and retained memory/OS resources. Compare a
  concrete host-loop workload with polling/blocking observers, including shared
  execution, and record costs and regressions alongside benefits. Set acceptance
  criteria before measurement; reject material MQTT/peer-progress regressions or
  scheduler-like machinery whose measured benefit does not justify its lifecycle cost.
- [x] Document a complete host event-loop example, source/rearm semantics, and
  starvation consequences in the C README and `CHANGELOG.md`; update `PARITY.md`.
  Show registration, waiting, source observation, acknowledgement/rearm, admission
  followed by completion registration, and wait-set removal before teardown.

Complete when the host can sleep until observations may have changed, drain
them safely, and rearm without missed readiness or owning MQTT scheduling, with
bounded signaling resources and demonstrated lifecycle/platform correctness.
Connection-observer readiness completes alongside TODO46 and is recorded separately.

## Implementation evidence and open acceptance items

The core and C implementations provide bounded registrations, revision-based
acknowledgement, non-consuming source observation, and explicit close/quiescence.
The complete [C example](native-wrappers/c/examples/readiness.c) and
[host protocol](native-wrappers/c/README.md#host-event-loop-readiness-optional) cover the
observation/rearm and teardown sequences.

Linux passed the core feature matrix, C default/minimal/composed test suites,
native readiness consumer and example, C/C++ header and ABI checks, and the
three readiness package-consumer profiles. Cross-compilation passed for macOS
and Windows; actual execution is pending their CI jobs. The broader native
package suite's `native-auth-invalid` assertion also reproduced against the
pre-change baseline and remains a separate validation limitation.

[Measurements and raw datasets](native-wrappers/wrapper-core/benches/readiness.md)
record two 80-run Linux comparisons. Active readiness replaced roughly 482–493
idle polling waits per 500 ms with one OS wait and added two descriptors per
context without adding threads. The follow-up throughput losses against busy
polling were 40–48% with one client and 5.8–8.2% with 32 clients, exceeding the
predefined 5% gate; p99 evidence is also noisy and crosses the predefined gate
in some cases. The performance checkbox remains open. Event-burst, hot-peer,
retained-memory, and native platform measurements are still required before
claiming the plan's acceptance criteria are satisfied.
