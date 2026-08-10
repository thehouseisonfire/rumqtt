# Native-Wrapper Boundary and Lifecycle Revisions

## Goal

Review the provisional `rumqttc-wrapper-core` boundaries after real C and
JavaScript or Python wrappers have shaped their public APIs. For each area in
this document, determine from wrapper requirements, measurements, and failure
tests whether the current implementation should be retained, documented more
precisely, optimized, or replaced.

These are investigations, not instructions to change every listed behavior.
The current architecture is the baseline until evidence supports a different
contract. Keep the crate private and malleable while these decisions remain
open.

Execution placement and optimizing the per-client dedicated-thread model are
tracked separately in `TODO10.md`. Decisions here must be tested against every
execution mode selected there.

## Preconditions

Start this review after:

- at least two native wrappers consume `rumqttc-wrapper-core`;
- their public command, completion, event, error, and cleanup APIs have working
  integration tests;
- the C wrapper exercises explicit ownership and timed blocking operations;
- an asynchronous wrapper exercises promises or futures, event iteration,
  cancellation, garbage collection, and environment shutdown; and
- representative workloads can report queue depth, outstanding operations,
  event consumption, driver polling, and cleanup latency.

Do not expand the shared abstraction for hypothetical wrapper features. Add or
change shared behavior only when at least two wrappers need the same semantics
or when correctness requires enforcing a shared invariant.

## Decision method

For every section below:

1. identify a concrete wrapper requirement, correctness gap, or measured cost;
2. document the current observable behavior;
3. list the smallest credible alternatives, including retaining the behavior;
4. test MQTT 3.1.1 and MQTT 5 where the concern is protocol-facing;
5. compare boundedness, failure visibility, API clarity, and teardown;
6. record a keep, change, or defer decision with supporting evidence; and
7. remove experimental paths that are not selected.

Do not silently change admission, completion, overload, or delivery claims
after wrappers publish stable APIs. If a public behavior must change, define a
migration and versioning plan in the affected wrapper.

## 1. Bound outstanding completion bookkeeping

### Current behavior

The underlying MQTT request channel and host event channel are bounded, but the
driver's `completion_tx` registration channel is unbounded. The pending
completion map can also grow with admitted operations that have not reached a
terminal MQTT milestone. A host can therefore retain or generate more wrapper
bookkeeping than the instantaneous request-channel capacity suggests,
especially during slow acknowledgements, reconnects, or broker unavailability.

### Questions

- Does the underlying client's inflight and request behavior provide a
  sufficient effective bound under every supported command and QoS?
- Can dropped host waiters leave completion tracking alive for an unacceptably
  long time?
- Should one limit cover registrations, all unfinished tracked operations, or
  separate operation classes?
- Should diagnostics and shutdown reserve capacity independent of ordinary
  operations?
- What finite default remains useful for high-throughput wrappers?

### Required investigation

Measure registrations, pending futures, completion senders, MQTT inflight work,
and total memory under:

- sustained QoS 1 and QoS 2 publication to a slow broker;
- subscribe and unsubscribe bursts;
- broker disconnection and reconnect backoff;
- hosts that retain every completion;
- hosts that immediately drop every completion waiter; and
- concurrent graceful or immediate shutdown.

Compare at least:

- a bounded completion-registration channel;
- a semaphore or permit limiting all unfinished tracked operations;
- one command envelope that reserves completion bookkeeping before MQTT
  admission; and
- retaining the current design with a demonstrated effective bound.

If an explicit bound is selected:

- reserve bookkeeping capacity before reporting successful admission;
- never admit MQTT work and then report ordinary `NotAdmitted` solely because
  wrapper bookkeeping is full;
- classify delivery as ambiguous if capacity failure occurs after MQTT
  admission and delivery cannot be disproved;
- use a configurable finite default and expose diagnostics for utilization;
- reserve any capacity required for shutdown and terminal wakeups; and
- release every permit during completion, driver failure, and shutdown.

Do not replace one unbounded queue with another or discard completion state
silently.

## 2. Validate driver-loop fairness

### Current behavior

The v4 and v5 drivers use a biased `tokio::select!` loop. Completion
registration, diagnostics, completed notices, and event-loop polling have a
stable priority order. This makes control behavior predictable, but sustained
readiness in an earlier branch may delay `EventLoop::poll`, network I/O,
keep-alive work, and incoming MQTT processing.

### Required investigation

Create deterministic stress tests combining:

- continuous command admission;
- repeated diagnostics requests;
- immediately ready completion futures;
- sustained incoming and outgoing MQTT traffic;
- keep-alive and retransmission deadlines;
- reconnect storms; and
- graceful and immediate shutdown.

Measure maximum and percentile delay between event-loop polls, command and
event latency, broker disconnects, and starvation of lower-priority work.

Compare:

- the current biased selection;
- unbiased selection;
- a fixed control-work budget followed by a mandatory event-loop poll;
- separate queues with explicit weighted scheduling; and
- an underlying-client API that exposes a clearer work or polling budget.

Retain biased selection only when the priority order is intentional,
documented, and demonstrated not to starve protocol progress. Do not rely on
average latency when rare poll delays can trigger keep-alive failure.

## 3. Decide the event-consumer return contract

### Current behavior

`EventConsumer` returns `Result<Option<WrapperEvent>>`. The current in-process
channel path represents driver failure as a terminal `WrapperEvent` and does
not otherwise produce an ordinary receive error. This makes the `Result` layer
largely reserved rather than behaviorally meaningful.

### Questions

Determine how the C, JavaScript, and Python APIs naturally distinguish:

- an ordinary event;
- clean end of stream;
- terminal driver failure;
- receive timeout or nonblocking `would block`;
- host-side cancellation; and
- destruction of the wrapper environment.

Compare:

- retaining `Result<Option<_>>` with concrete error cases;
- returning `Option<_>` and keeping terminal failure as event data;
- returning terminal driver failure as `Err` and reserving events for MQTT and
  lifecycle activity; and
- separate ordinary-event and terminal-status receive operations.

Choose one semantic model before stable wrapper APIs encode redundant or
contradictory states. Ensure terminal failure remains observable when the
ordinary event buffer is full. Do not use formatted Rust errors as stream-state
discriminants.

## 4. Define finalization, joining, and reaping

### Current behavior

Dropping `NativeClient` requests immediate shutdown, but a host finalizer cannot
safely perform an unbounded thread join. Dropping a Rust `JoinHandle` detaches
the driver thread until it exits. JavaScript garbage collection, Python
interpreter finalization, C handle destruction, process exit, and ordinary
explicit close have different blocking and callback constraints.

### Required investigation

Test:

- graceful and immediate explicit close;
- forgotten or garbage-collected clients;
- Node environment and worker termination;
- Python interpreter and event-loop teardown;
- C destruction with unreachable brokers;
- process exit with live clients;
- module unloading where supported; and
- failure after partial client or thread construction.

Compare:

- wrapper-owned bounded joins in cleanup hooks;
- a shared background reaper that owns terminating driver handles;
- explicit ownership requiring callers to close and join;
- task tracking on a shared runtime selected by `TODO10.md`; and
- documented process-exit abandonment after callbacks have been disabled.

The selected design must:

- never block a host finalizer indefinitely;
- never call into a destroyed Python or JavaScript environment;
- prevent ordinary cleanup from routinely leaking detached threads or tasks;
- resolve or fail pending host operations exactly once;
- tolerate repeated and concurrent close requests; and
- define what a bounded cleanup timeout means for remaining native work.

Do not hide teardown failure merely because the operating system eventually
reclaims resources at process exit.

## 5. Validate event-stream topology

### Current behavior

One `EventConsumer` maps cleanly to a C pull interface or one JavaScript/Python
async iterator. It preserves ordering, gives one owner responsibility for
draining backpressure, and avoids duplicating manual-ack authority.

Real wrappers may request multiple subscribers, separate publish and lifecycle
streams, callbacks, diagnostics observers, or integration with more than one
host component.

### Required investigation

Record concrete use cases before adding fan-out. First determine whether thin
host-side adapters can provide convenience without changing the shared driver.

If shared fan-out or partitioning is needed, define:

- whether events are copied, shared, partitioned, or broadcast;
- ordering within and across streams;
- independent queue bounds and slow-consumer behavior;
- whether one full consumer can terminate or delay the MQTT driver;
- how terminal status reaches every consumer;
- which consumer owns or may exercise a manual-ack token;
- what happens when a consumer disappears with unacknowledged messages; and
- the total memory bound introduced by every additional queue.

Do not duplicate incoming publishes silently or allow competing consumers to
acknowledge the same opaque token. Preserve an explicit single-consumer error
if that remains the selected contract.

## 6. Decide Rustls provider policy

### Current behavior

The wrapper core selects the AWS-LC Rustls provider. This matches the current
client configuration but may affect binary size, build tooling,
cross-compilation, supported platforms, licensing review, and environments
with provider or FIPS requirements.

### Required investigation

Validate AWS-LC against every wrapper artifact and supported target, including:

- Linux glibc and musl builds;
- macOS and Windows targets;
- x86_64 and aarch64 packaging;
- Node-API and Python wheel build systems;
- static and dynamic C library distribution;
- binary-size and startup targets;
- licensing and security review; and
- any documented compliance or FIPS expectations.

Compare retaining one provider with exposing workspace-aligned AWS-LC and ring
features. If wrappers require a choice:

- align names and defaults with the v4 and v5 clients;
- make providers mutually exclusive;
- prevent Cargo feature unification from selecting both implicitly;
- ensure wrapper core and both underlying clients use compatible providers;
- test custom roots, platform roots, mutual TLS, TLS, and WSS for each provider;
  and
- include every supported provider in CI and release packaging tests.

Keep one provider when it satisfies every supported wrapper. Configurability
without a consumer requirement creates permanent build and test cost.

## Cross-cutting verification

Retain the wrapper-core lifecycle, overload, reconnect, protocol parity, and
shutdown suites. Add tests selected by the decisions above, including:

- bounded completion bookkeeping under broker stalls;
- no event-loop starvation under sustained control traffic;
- unambiguous event, end-of-stream, timeout, and terminal failure mapping in
  each wrapper;
- cleanup without callbacks into destroyed environments;
- repeated construction and destruction without leaked threads or tasks;
- single-consumer or fan-out invariants under event-buffer pressure;
- manual-ack ownership across the selected stream topology; and
- TLS provider builds and connection tests on supported targets.

Use stress testing, ThreadSanitizer, and model testing where practical. Record
the exact wrapper, broker, operating system, Rust toolchain, and workload used
for measurements or architecture decisions.

## Deliverables

For each area, check in:

- a concise decision record with the wrapper requirement and current baseline;
- benchmark or failure-test evidence;
- the alternatives evaluated;
- the selected behavior and rejected alternatives;
- updated wrapper-core and host-wrapper documentation;
- deterministic regression tests; and
- removal of unused experimental APIs and feature flags.

## Completion criteria

This TODO is complete when:

- at least two real wrappers have validated every listed boundary;
- all six areas have an explicit keep, change, or defer decision;
- unfinished-operation memory has a documented and tested bound or a measured
  justification for the effective existing bound;
- the selected driver scheduling policy has a starvation test;
- event and terminal receive semantics map cleanly into every supported wrapper;
- wrapper finalization has bounded, environment-safe cleanup behavior;
- event-stream ownership and manual acknowledgements remain unambiguous;
- TLS provider policy is compatible with every released artifact; and
- unselected prototypes and speculative abstractions have been removed.

Until then, treat `rumqttc-wrapper-core` as private, revisable infrastructure and
avoid promising stability for these internal APIs.
