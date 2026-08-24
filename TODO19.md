# Explicit Native Completion Bridge Exploration for Python

## Goal

Determine, with a bounded prototype and measurements, whether the Python wrapper
should replace most extension-side Tokio futures with an explicit native
completion bridge while retaining the public, typed `asyncio` API described in
`TODO8.md`.

This is an architecture exploration, not a commitment to rewrite the wrapper.
The contender is legitimate only if it preserves the existing MQTT semantics,
bounded-memory guarantees, cancellation behavior, event ordering, and
deterministic cleanup while measurably simplifying runtime ownership or
improving resource use and performance. Reject it if it merely moves async
runtime machinery into bespoke thread, callback, and lifetime machinery.

The public API remains `asyncio`-native in either outcome. This exploration is
about the private Rust-to-Python wakeup mechanism, not about replacing
coroutines, awaitable MQTT completions, async event iteration, loop affinity,
or asynchronous context management.

## Current baseline and precise question

The current extension uses `pyo3-async-runtimes` with its Tokio integration.
Rust futures wait for native startup, command admission, connection
observation, operation completion, events, and shutdown; `future_into_py`
turns those futures into Python awaitables. Blocking construction and bounded
joins use `tokio::task::spawn_blocking`.

The shared wrapper core already owns the actual MQTT execution model:

- each `NativeClient` owns a dedicated native driver thread;
- that thread owns the rumqtt client and its current-thread Tokio runtime;
- commands, events, terminal status, and operation results cross owned,
  bounded native channels or repeatable native handles; and
- Python's event loop does not poll the MQTT driver.

The question is therefore narrower than “Tokio versus asyncio”:

> Can a small, explicit native dispatcher observe the core's blocking or
> notification interfaces and deliver owned results to the bound Python loop
> with `loop.call_soon_threadsafe`, eliminating the Python extension's Tokio
> executor and `pyo3-async-runtimes` dependency without introducing more
> threads, races, retained Python objects, or shutdown complexity?

Do not count removal of dependency lines as simplification. Compare complete
state machines, thread ownership, cancellation paths, finalization behavior,
unsafe code, tests, and failure modes.

## Feasibility constraint discovered before the spike

The current core does not yet expose one blocking or callback-based source that
can multiplex arbitrary connection, admission, completion, event, and shutdown
observations:

- `CompletionHandle` has `try_wait`, `wait`, timeout waits, and `wait_async`;
- `ConnectionHandle` has `try_wait` and `wait_async`, but no blocking wait;
- `EventConsumer` has `try_recv`, `recv_timeout`, and `recv_async`; and
- asynchronous command admission is expressed through the core's async API.

Consequently, a naïve implementation would need a blocking task or thread for
each operation. That is not an acceptable contender. It has unbounded or
capacity-proportional thread growth, expensive cancellation, awkward joins,
and worse interpreter shutdown behavior than the baseline.

The prototype must first prove one of these bounded designs:

1. **One dispatcher per client:** a single joinable native dispatcher owns all
   Python-facing waits for that client and multiplexes commands, connection,
   completions, events, cancellation, and shutdown.
2. **One dispatcher per interpreter/module instance:** one joinable dispatcher
   serves all clients belonging to that interpreter and uses client identifiers
   plus generation-safe registration tokens.
3. **Core-provided host notification stream:** the wrapper core exposes a
   bounded, host-neutral stream of “observation ready” notices from which the
   extension retrieves the authoritative result. This is acceptable only if it
   is also a sound primitive for the C or other native wrappers, or fixes a
   shared correctness invariant; do not add Python lifecycle concepts to
   `wrapper-core`.

Prefer the smallest design that remains bounded and joinable. Do not introduce
a process-global dispatcher, detached/daemon threads, one thread per Python
future, periodic polling, or an unbounded notification queue.

## Required prototype boundary

Build the contender behind a private, non-default Cargo feature such as
`explicit-python-bridge`. The production facade and its public types must be
unchanged so the same behavioral suite can run against both implementations.
Do not remove the Tokio adapter until the decision gate is satisfied.

The prototype may initially cover a vertical slice, but that slice must include
all of the difficult lifecycle categories:

1. initial `connect()` with concurrent and cancelled Python waiters;
2. request-channel admission under saturation;
3. one tracked QoS 1 publish completion;
4. a pending `events().__anext__()` call, including its cancellation;
5. graceful close and immediate close with bounded native joins;
6. loop closure while native work remains pending; and
7. interpreter/process cleanup with an open client.

A connect-and-publish happy-path microbenchmark alone is not sufficient to
establish feasibility.

## Proposed bridge contract

### Native registrations

Represent every host wait with an owned native registration containing at
least:

- a monotonically unique registration identifier;
- client identity and client generation;
- operation kind and, where applicable, `OperationId`;
- only owned Rust state needed to observe the authoritative core result;
- an atomic lifecycle state such as `Registered`, `Ready`, `Scheduled`,
  `Cancelled`, or `Retired`; and
- a weak or otherwise explicitly releasable reference to the Python delivery
  target.

The core's `CompletionHandle`, `ConnectionHandle`, and terminal state remain
authoritative. A bridge registration is only a delivery subscription. Dropping
or cancelling it must not recall admitted MQTT work.

Use generation-safe identifiers so a late native notice cannot resolve a new
future that reused an address or numeric slot. Retirement must be idempotent.
Every registration must have exactly one terminal ownership path, including
failure to schedule onto the loop.

### Python-loop delivery

Create the `asyncio.Future` on the running loop while attached to the correct
interpreter. When native state becomes ready, schedule one small callback with
the loop's thread-safe scheduling facility. The callback must:

- verify the client, interpreter/module instance, and registration generation;
- remove the registration before or atomically with delivery;
- avoid `InvalidStateError` when the future was cancelled or already resolved;
- set either the typed result or stable wrapper exception;
- hold Python references only for the duration required by the ownership
  contract; and
- never run MQTT protocol work or a blocking native wait.

The dispatcher must never invoke arbitrary application callbacks directly.
Calling the loop's scheduling primitive is the only cross-thread Python action
permitted by the design. Record exactly which PyO3/CPython APIs make this safe
during normal operation and how delivery is disabled before finalization.

### Cancellation

Python cancellation retires only the host registration. It does not cancel an
admitted publish, subscribe, unsubscribe, or diagnostic command and does not
claim that broker delivery did not occur.

Preserve the narrower manual-acknowledgement rule from `TODO8.md`: cancellation
before ACK admission restores the token for retry; cancellation after admission
drops only the Python waiter. Test both sides of that boundary if
acknowledgements enter the prototype.

Cancelling `__anext__()` must release the sole pending event registration
without consuming or discarding the next event. This race must be demonstrated
under repeated cancellation, not reasoned about only from the happy path.

### Boundedness and wakeups

The complete design must state and enforce hard bounds for:

- dispatcher threads;
- registered waits per client;
- ready notices not yet scheduled;
- callbacks scheduled but not yet run;
- retained Python futures and loop references; and
- events buffered outside `wrapper-core`.

The event path may retain the configured bounded core event buffer plus one
pending iterator registration. It must not add a second event queue. Coalesce
wakeups where possible, but never coalesce ordered MQTT events or distinct
operation completions.

A saturated or stopped Python loop must not block the MQTT driver. The bridge
must have a defined failure transition when it cannot schedule delivery; it
must retire registrations, request appropriate client cleanup, and remain
joinable rather than retrying forever.

### Startup and shutdown

Native client construction and native joins may block and must remain off the
Python event-loop thread. The contender must identify who owns and joins any
dispatcher thread as clearly as `NativeClient` owns its driver thread.

Normal close, immediate close, garbage collection, module teardown, and
interpreter finalization must follow an explicit order:

1. stop accepting registrations;
2. make Python delivery unavailable;
3. retire or wake all outstanding registrations;
4. request client shutdown;
5. wake the dispatcher without relying on Python;
6. perform bounded native joins from a safe context; and
7. release remaining Python references while the owning interpreter is valid.

Do not use `__del__`, `atexit`, or daemon-thread behavior as proof that native
cleanup is correct. The prototype must preserve the current best-effort final
fallbacks while demonstrating a primary, deterministic ownership path.

## Core API exploration

Before changing `wrapper-core`, write a short design note under the prototype
that evaluates whether its existing primitives can support a bounded
dispatcher. In particular, investigate:

- whether admissions can be initiated nonblocking and retried only after a
  real capacity notification rather than periodic polling;
- whether connection observation needs a blocking wait or a host-neutral
  registration primitive;
- whether many `CompletionHandle` values can feed one bounded readiness stream
  without cloning results eagerly;
- whether `EventConsumer` and terminal status can participate in the same
  selector without changing event order; and
- whether shutdown can wake every blocked host observation deterministically.

If a new core primitive is necessary, prefer readiness notification followed
by authoritative `try_wait`, not callbacks carrying host objects. It must use
owned Rust data, be independently testable, and not mention Python, the GIL,
event loops, or interpreters.

Do not distort the shared core solely to make dependency removal possible. If
the only clean implementation requires a Python-specific observer registry in
the core, record that as evidence against the contender.

## Performance and resource evaluation

Benchmark both implementations from the same optimized wheel build, CPython
version, broker setup, transport, payloads, capacities, and machine. Run enough
iterations to report distributions rather than one timing. Include warm-up and
record CPU model, OS, Python version, Rust profile, broker, and whether TLS is
enabled.

At minimum measure:

- median and p95/p99 Python-observed latency for admission-only QoS 0;
- median and p95/p99 latency for tracked QoS 0 and QoS 1 completion;
- publish throughput with 1, 32, and 256 concurrent Python tasks;
- ordered incoming-event throughput and latency;
- connect and close latency across repeated client creation;
- idle CPU usage with connected clients and no traffic;
- resident memory and native thread count for 1, 10, and 100 clients;
- retained Python/native objects after repeated create/connect/cancel/close;
- callback backlog behavior when the Python loop is deliberately stalled; and
- shutdown time with pending connection, admission, completion, and event
  waits.

Use local brokers and stable, repository-owned harnesses. Separate MQTT/network
latency from bridge overhead with a test-only native completion source that can
complete operations deterministically. Do not draw conclusions from networked
throughput alone, where broker and socket costs can hide bridge overhead.

The explicit bridge need not be faster in raw MQTT throughput to win. Reduced
idle resources, fewer runtime threads, more deterministic cleanup, or a
substantially smaller lifecycle state space can be legitimate benefits.
Conversely, a microbenchmark improvement that worsens finalization or adds
unbounded resources is not a win.

## Complexity evaluation

Include a written comparison covering:

- production Rust and Python code added, removed, and changed;
- number and ownership of native threads and runtimes;
- number of synchronized state machines and race transitions;
- use of `unsafe` or direct CPython FFI;
- dependency and wheel impact;
- cancellation and loop-closure behavior;
- interpreter-finalization assumptions;
- subinterpreter and free-threaded-CPython implications, without claiming
  support for either;
- test-only hooks required to make races deterministic; and
- maintenance risk when PyO3, CPython, asyncio, or Tokio changes.

Line count is supporting evidence, not the decision metric. Prefer explicit
state that can be exhaustively tested over implicit behavior, but reject a
home-grown executor disguised as a bridge.

## Correctness and lifecycle test matrix

Run the existing Python wrapper suite unchanged against both backends, then add
deterministic tests for:

- completion before registration, during registration, before callback
  scheduling, after scheduling, and after Python cancellation;
- cancellation racing successful result and terminal driver failure;
- loop closure immediately before and after native readiness;
- reuse-resistant registration identifiers and late notices;
- concurrent `connect()` waiters where one, some, or all are cancelled;
- event wait cancellation without event loss or reordering;
- core event-buffer overflow while no Python callback can run;
- request saturation without busy polling or thread growth;
- graceful close timing out while callbacks are queued;
- immediate close with every category of wait pending;
- repeated create/connect/close with stable thread and object counts;
- garbage collection cycles containing clients, iterators, acknowledgements,
  and futures;
- normal process exit, `sys.exit()`, and loop closure with live clients; and
- test-injected panic at native wait, scheduling, and delivery boundaries.

Use barriers or test hooks rather than sleeps to place races at exact state
transitions. Run the lifecycle subset under sanitizers and available leak/race
tools supported by the Python build and platform. A passing happy-path suite is
not sufficient.

## Decision gate

Adopt the explicit bridge only if all of the following are true:

1. It passes the full behavioral, cancellation, overload, lifecycle, and wheel
   suites on every CPython version and platform advertised by the package.
2. It uses a fixed number of dispatcher threads per client or interpreter and
   never creates a thread or blocking worker per operation.
3. It introduces no unbounded queue, busy polling, detached thread, retained
   borrowed Python memory, or callback from the MQTT driver thread.
4. It preserves the exact public API and documented delivery/cancellation
   semantics.
5. It has an explicit, bounded cleanup path when the Python loop is closed or
   unavailable.
6. It removes the extension-side Tokio runtime and
   `pyo3-async-runtimes`, rather than retaining them for substantial paths
   alongside the new machinery.
7. It demonstrates at least one material benefit with reproducible evidence:
   meaningfully lower idle memory/thread use, lower bridge latency, higher
   completion/event throughput, more predictable shutdown, or a clearly
   smaller and more auditable lifecycle model.
8. The measured regressions in other dimensions are documented and acceptable
   for the wrapper's expected workloads.

The result is **inconclusive**, not successful, if performance differences are
within noise and the complexity comparison lacks deterministic lifecycle
evidence.

Reject the contender if any of the following is necessary:

- one native thread or blocking-pool task per Python operation;
- periodic polling to discover capacity, events, or completions;
- a second unbounded or capacity-duplicating host event queue;
- process-global Python references or dispatcher state;
- daemon threads or indefinite joins;
- direct arbitrary Python execution from the MQTT driver thread;
- broad `unsafe` CPython integration without a narrowly proven requirement;
- weakening cancellation, overload, ordering, or cleanup guarantees; or
- substantial new core machinery useful only to delete an adapter dependency.

If rejected, retain the current `pyo3-async-runtimes` design, document why it
is the better complexity boundary, and keep the benchmark and lifecycle
harnesses as regression tests where practical. A well-supported rejection is a
successful outcome of this TODO.

## Deliverables

1. A private feature-gated prototype covering the required vertical slice.
2. A bridge design note with ownership, state-transition, wakeup, boundedness,
   and shutdown diagrams or tables.
3. Any proposed host-neutral core API, with focused unit tests and a written
   justification for why it belongs in shared wrapper infrastructure.
4. Reproducible functional, race, resource, and benchmark harnesses comparing
   the baseline and contender.
5. Raw benchmark outputs plus a concise comparison report containing hardware
   and software metadata.
6. A final architecture decision record choosing adoption or rejection against
   every decision-gate item.
7. If adopted, a separate implementation plan for deleting the baseline,
   updating the Python README and `CHANGELOG.md`, and running the complete
   release matrix. Do not turn the exploratory patch directly into production
   code without that review.

## Completion criteria

This TODO is complete when the feature-gated contender has either passed the
decision gate or been rejected with reproducible evidence, and the repository
contains an explicit architecture decision explaining the result.

It is not complete when Tokio dependencies have merely been removed, when only
microbenchmarks pass, or when the contender works during normal operation but
has not demonstrated bounded cancellation, loop closure, driver failure, and
interpreter cleanup.
