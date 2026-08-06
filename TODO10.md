# Native-Wrapper Driver Execution and Scalability

## Goal

Determine whether the per-client dedicated-thread model in
`rumqtt-wrapper-core` should remain the only execution mode, remain the default
alongside a second mode, or be replaced after real native wrappers establish
their lifecycle and scheduling requirements.

The current model deliberately favors isolation and predictable progress. Each
`NativeClient` owns one named operating-system thread, one Tokio current-thread
runtime, and one MQTT event loop. Do not optimize that model away based only on
its apparent per-client cost. Measure it through actual C and JavaScript or
Python wrappers, prototype only credible alternatives, and preserve the
observable wrapper contracts regardless of the selected implementation.

This TODO owns execution placement, runtime ownership, scalability, and task or
thread teardown. The provisional wrapper-core API and policy questions are
tracked separately in `TODO11.md`.

## Current decision

Implement dedicated-thread execution only until a real wrapper demonstrates a
need for another mode. In the current implementation, `NativeClient::start`:

1. validates and consumes an owned `ClientConfig`;
2. constructs one v4 or v5 asynchronous client and `EventLoop`;
3. starts one named operating-system thread;
4. builds a Tokio current-thread runtime on that thread;
5. continuously polls the event loop and tracked completion futures; and
6. reports events, completions, terminal status, and diagnostics through
   thread-safe channels.

Tokio is an internal execution dependency because the rumqtt clients already
use Tokio networking, timers, and event-loop futures. The host does not provide
an executor and cannot accidentally stop MQTT progress by failing to poll a
host future. Python, JavaScript, and C scheduling, callback, promise, GIL, and
ABI behavior remains outside `rumqtt-wrapper-core`.

The model deliberately incurs:

- one operating-system thread and one small Tokio runtime per client;
- channel handoffs and context switches between the host and driver;
- no runtime or thread sharing across clients; and
- wrapper-specific finalizer and bounded-join integration.

These costs buy failure isolation, straightforward ownership, and the same
progress guarantee for C, Python, Node.js, Deno, and Bun. They are expected to
be acceptable for a modest number of clients but may become material for
processes with hundreds or thousands of clients.

An asynchronous host API is not evidence that executor embedding is needed.
The wrapper-core asynchronous admission, completion, and event futures can be
bridged into host futures or promises while MQTT continues to run on its
dedicated thread.

## Preconditions

Begin this investigation only after:

- `rumqtt-wrapper-core` is used by at least two native wrappers;
- one consumer exercises an asynchronous host runtime, preferably the
  JavaScript wrapper from `TODO6.md` or a Python asyncio wrapper;
- one consumer exercises explicit native ownership and teardown, preferably
  the C wrapper from `TODO7.md`;
- both wrappers test connect, reconnect, event consumption, graceful close,
  immediate close, forgotten clients, and host finalization; and
- the dedicated-thread implementation has a repeatable benchmark and resource
  measurement harness.

Do not block the first wrapper releases on this exploration unless measurements
show a concrete resource, latency, correctness, or teardown problem.

## Contracts every execution mode must preserve

Changing where the driver runs must not silently alter:

- admission versus MQTT completion semantics;
- tracked QoS 0 flush, QoS 1 acknowledgement, and QoS 2 completion milestones;
- reconnect behavior after recoverable connection errors;
- bounded request and event delivery;
- visible event-buffer overflow through an independent terminal path;
- single-use, client-scoped manual-ack tokens;
- graceful and immediate shutdown delivery claims;
- exactly one terminal result for each registered completion;
- stable error kinds and ambiguous-delivery classification; or
- the rule that dropping a host waiter does not cancel admitted MQTT work.

Use one underlying driver loop whenever more than one execution mode exists.
Do not maintain thread-backed and executor-backed polling implementations that
can diverge in protocol translation, reconnect, overload, or shutdown behavior.

## 1. Establish representative workloads

Use identical broker, protocol, payload, and host API semantics when comparing
execution models. At minimum, measure:

- MQTT 3.1.1 and MQTT 5;
- TCP and TLS, plus WebSocket and WSS where supported;
- 1, 10, 100, and 1,000 concurrent clients;
- idle connected clients and periodic low-rate publishing;
- sustained QoS 0, QoS 1, and QoS 2 publishing;
- subscription-heavy incoming traffic;
- reconnect storms and prolonged broker unavailability;
- slow event consumers and event-buffer pressure;
- graceful and immediate environment shutdown; and
- repeated construction and destruction that exposes leaked threads, tasks,
  callbacks, handles, or runtime state.

Record at least:

- resident memory, virtual memory, and incremental memory per client;
- operating-system thread count and stack reservation;
- idle and active CPU consumption;
- command, completion, and event throughput;
- p50, p95, and p99 admission, completion, and event latency;
- scheduler fairness and maximum delay between event-loop polls;
- reconnect convergence time;
- shutdown latency and missed shutdown deadlines;
- channel depth, outstanding tracked operations, and event-buffer utilization;
  and
- JavaScript event-loop delay or Python loop latency where applicable.

Run release builds on every supported operating system. Thread stacks, runtime
behavior, TLS packaging, and shutdown facilities differ enough that a
Linux-only result is not a general wrapper result.

## 2. Evaluate credible execution strategies

### 2.1 Retain one dedicated thread per client

Keep the current model when its resource use remains acceptable for documented
wrapper workloads. Prefer this result when it gives the clearest lifecycle and
most reliable progress without a material scalability limit.

If retained, document a measured or recommended client-count range. Consider a
smaller thread stack only after measuring maximum stack use on every supported
platform and retaining a safe margin.

### 2.2 Use a library-owned shared Tokio runtime

Evaluate running multiple client drivers as tasks on one library-owned
multi-thread Tokio runtime. Define explicit startup, reference counting, panic
handling, and bounded shutdown. An implicit global runtime must not keep the
process alive unexpectedly or allow one wrapper environment to stop clients
owned by another environment.

Determine:

- whether the runtime is process-global, wrapper-environment-local, or sharded;
- how worker counts and blocking work are configured;
- how driver failures and panics remain isolated;
- how fair scheduling is maintained during reconnect or completion storms;
- whether DNS, TLS, or connector work can delay unrelated clients; and
- how the last client releases runtime resources without racing a new client.

Do not introduce an immortal global runtime merely to reduce thread count.

### 2.3 Use library-owned runtime shards

Evaluate a bounded pool of current-thread runtimes with clients assigned to
shards. This may preserve simple single-threaded driver behavior while bounding
the total thread count.

Specify shard selection, capacity, load balancing, failure containment,
shutdown, and migration behavior. Measure the effect of a busy, reconnecting,
or slow client on peers sharing the same shard. Compare sharding with a Tokio
multi-thread runtime instead of assuming it is cheaper or fairer.

### 2.4 Embed in a host-provided executor

Add embedded execution only for a wrapper that can provide a suitable Tokio
executor through a supported integration API. A tentative internal shape is:

```rust,ignore
let (handle, events, driver) = Driver::new(config)?;
let task = host_runtime.spawn(driver.run());
```

Before exposing this mode, define:

- the required Tokio version and runtime, networking, and timer facilities;
- whether the driver future is `Send + 'static`;
- who owns the task handle and observes panics;
- how task cancellation differs from immediate MQTT shutdown;
- how pending operations terminate if the host runtime disappears first;
- whether the host may pause or starve MQTT progress;
- how interpreter, Node environment, and worker shutdown is coordinated; and
- whether sharing measurably improves resource use or latency.

Do not assume that Node.js, Deno, Bun, or Python exposing asynchronous APIs
means their internal executor is a stable native-extension boundary.

If embedding is selected, extract one `Driver::run` future and implement
`NativeClient::start` as the dedicated-thread adapter around it. Keep runtime
ownership out of the protocol-neutral command and event model.

### 2.5 Support more than one mode

Support dedicated and shared or embedded modes only when distinct wrappers or
deployment profiles demonstrably need them. Selection must be explicit and its
progress, teardown, and isolation consequences must be documented.

Avoid a generic executor trait or plugin framework. The underlying clients are
Tokio-based; pretending otherwise obscures rather than removes the runtime
contract.

## 3. Prototype and decide

For each alternative:

1. state the wrapper requirement or measured bottleneck;
2. capture the dedicated-thread baseline;
3. prototype the smallest credible implementation;
4. compare correctness, memory, CPU, latency, fairness, and teardown;
5. test both MQTT versions and every affected transport;
6. record the result in an architecture decision document; and
7. remove unselected APIs, features, and prototype code.

Prefer compatible internal refactoring while `rumqtt-wrapper-core` remains
private. After wrappers expose stable behavior, preserve their progress,
completion, overload, and shutdown contracts even if execution changes.

## Verification

Every selected mode must have deterministic tests for:

- no starvation under sustained control and MQTT traffic;
- identical v4/v5 completion, reconnect, and shutdown behavior;
- event-buffer overflow remaining visible through the reserved terminal path;
- driver cancellation or runtime loss producing defined terminal results;
- runtime or shard shutdown while other clients remain active;
- environment destruction without callbacks into an invalid host;
- repeated construction and teardown without leaked threads or tasks; and
- affected paths under ThreadSanitizer or model testing where practical.

Retain the existing wrapper-core lifecycle, overload, reconnect, protocol
parity, and shutdown suites. Check in benchmark methodology and summaries with
the exact host, operating system, toolchain, broker, and wrapper versions.

## Completion criteria

This TODO is complete when:

- at least two real wrappers have supplied lifecycle and workload data;
- the dedicated-thread baseline and credible alternatives have comparable
  measurements;
- one execution model, or an explicitly justified small set of modes, has been
  selected;
- runtime ownership, progress, cancellation, panic, and teardown contracts are
  documented for every selected mode;
- wrapper finalization cannot routinely leave driver work detached;
- the selected architecture passes the required correctness and stress tests;
  and
- unused prototypes and speculative executor abstractions have been removed.

Until then, keep dedicated-thread execution as the supported implementation and
keep `rumqtt-wrapper-core` private.
