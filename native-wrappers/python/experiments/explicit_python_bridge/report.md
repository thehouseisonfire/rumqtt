# Explicit Bridge Comparison Report

## Environment and method

Both backends were built as optimized CPython 3.14 wheels from the same worktree and measured as
separate processes against the repository Mosquitto fixture over plain TCP. The host was Linux
7.2.0-1-cachyos on a 13th Gen Intel Core i5-13500H, CPython 3.14.2, Rust 1.96.1, and Mosquitto 2.1.2.
TLS was disabled. Latency distributions contain 600 observations; throughput contains 12 runs of 600
operations at each concurrency. Raw samples are in `raw/tokio.json` and `raw/explicit.json`.

The runs were performed sequentially rather than interleaved, so small timing differences must not be
treated as stable wins. Network event latency includes broker and socket scheduling. The deterministic
probe isolates Rust-to-Python scheduling but still includes asyncio Future creation and delivery.

## Results

| Metric | Tokio baseline | Explicit bridge | Observation |
|---|---:|---:|---|
| deterministic completion median | 40.92 us | 13.22 us | explicit faster in this run |
| admission-only QoS 0 median | 42.44 us | 46.98 us | explicit 10.7% slower |
| tracked QoS 0 median | 82.74 us | 82.54 us | effectively equal |
| tracked QoS 1 median | 102.24 us | 95.10 us | explicit 7.0% faster |
| throughput, 1 task | 9,525/s | 14,278/s | explicit 49.9% higher |
| throughput, 32 tasks | 16,583/s | 16,084/s | explicit 3.0% lower |
| throughput, 256 tasks | 16,297/s | 21,611/s | explicit 32.6% higher |
| incoming events | 4,615/s | 6,844/s | explicit 48.3% higher |
| incoming event median latency | 33.64 ms | 44.31 ms | explicit 31.7% worse |
| connect median | 1.61 ms | 1.45 ms | explicit 9.9% faster |
| close median | 182.93 us | 110.49 us | explicit 39.6% faster |
| pending-connect shutdown median | 110.57 us | 41.90 us | explicit 62.1% faster |

After connection/start/close churn, the baseline Tokio blocking pool retained 530 threads with one
live client and 629 with 100 clients. The explicit backend used 3 and 102 respectively: Python main,
one module dispatcher, and one driver per client. RSS was 45.98 MB versus 33.15 MB at one client and
64.03 MB versus 52.60 MB at 100 clients. Both retained zero of 50 closed Python clients.

When 2048 completions became ready while the loop was blocked, neither backend grew threads and both
delivered every result afterward. The explicit callback backlog grew RSS by 1.93 MB versus 0.80 MB for
the baseline, a 2.4x regression.

Release wheels without test hooks were 2,819,440 bytes for the baseline and 2,670,146 bytes for the
explicit backend, a 149,294-byte (5.3%) reduction.

## Complexity

The existing adapter was 407 lines in `client.rs`. The contender added 212 lines of facade bindings,
920 lines of dispatcher/state-machine code, and 140 lines for core readiness, before tests and
documentation. It owns a command queue, client registry, registration registry, cancellation relay,
event return slot, deadline scheduler, Python scheduling callback, and module/thread join protocol.
No direct FFI or new `unsafe` code was required.

Implementation uncovered lost-wakeup, close-before-connect, retained-event, and manual-ack transition
bugs before the behavioral suite passed. Those are exactly the lifecycle responsibilities currently
delegated to Tokio, `pyo3-async-runtimes`, and wrapper-core wait primitives. Future PyO3, asyncio, and
interpreter-finalization changes would need review across this bespoke state machine.

The baseline's retained blocking-pool threads are a genuine resource defect worth addressing
separately by configuring or avoiding its general Tokio blocking pool. That narrower change does not
justify replacing all extension-side futures with the prototype.

## Bounded Tokio follow-up

An intermediate production candidate was subsequently configured with Tokio's maintained runtime
builder and a hard ceiling of 32 blocking workers. The original raw files above remain unchanged. Four additional
fresh-process runs used an A/B/B/A order (bounded Tokio, explicit, explicit, bounded Tokio); their raw
results are `tokio-bounded-1.json`, `explicit-followup-1.json`, `explicit-followup-2.json`, and
`tokio-bounded-2.json`. Values below are the median of the two process-level medians for each backend.

| Metric | Bounded Tokio | Explicit bridge |
|---|---:|---:|
| deterministic completion median | 40.11 us | 14.75 us |
| admission-only QoS 0 median | 35.37 us | 20.61 us |
| tracked QoS 0 median | 73.96 us | 37.98 us |
| tracked QoS 1 median | 80.69 us | 55.88 us |
| throughput, 1 task | 12,857/s | 16,768/s |
| throughput, 32 tasks | 18,805/s | 19,749/s |
| throughput, 256 tasks | 22,856/s | 20,956/s |
| incoming events | 5,104/s | 7,013/s |
| incoming event median latency | 38.90 ms | 35.53 ms |
| connect median | 1.52 ms | 1.49 ms |
| close median | 187.17 us | 154.25 us |
| pending-connect shutdown median | 93.08 us | 50.08 us |
| callback-backlog RSS growth | 1.47 MB | 1.93 MB |
| threads, one live client | 50 | 3 |
| RSS, one live client | 36.98 MB | 33.23 MB |
| threads, 100 live clients | 149 | 102 |
| RSS, 100 live clients | 53.12 MB | 52.68 MB |

The bounded adapter retained exactly 50 threads with one client in both runs: 32 blocking workers,
the host's Tokio async workers, the Python main thread, and one MQTT driver. This is 480 fewer threads
than the original baseline result. At 100 clients it retained 149 rather than 629 threads. The change
also reduced the original one-client and 100-client RSS measurements by about 20% and 17%,
respectively. Callback-backlog growth remains lower than the explicit bridge, although both follow-up
values were higher than their first-run measurements.

These follow-up timing results vary enough from the first sequential comparison to reinforce its
caution against treating small latency differences as stable wins. The bounded Tokio change fixes
the production resource defect without taking ownership of the explicit bridge's scheduler and
teardown state machine, so the rejection decision remains unchanged.

## Tokio blocking-cap sweep

After removing the rejected implementation, the retained harness measured caps 4, 8, 16, 32, and 64
in three fresh processes each. Runs used forward, reverse, and rotated orders. Because cap 4 was the
smallest qualifying value in the first sweep, cap 2 received three additional fresh-process runs. Raw
process results and execution order are preserved in `raw/tokio-cap-sweep.json`.

The balanced score gives equal weight to deterministic completion latency, a 2,048-completion burst,
and publish throughput at concurrency 32 and 256. Each component is normalized against the best
observed result and combined with a geometric mean; lower is better. Values below are medians of the
three process-level medians.

| Cap | Completion | 2,048 burst | Throughput 32 | Throughput 256 | Threads, 1 client | RSS | Saturated 50 ms close | Score |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 27.48 us | 22.78 ms | 28,270/s | 32,435/s | 20 | 36.69 MB | 50.90 ms | 1.010 |
| 4 | 34.03 us | 33.61 ms | 22,225/s | 24,446/s | 22 | 36.66 MB | 51.57 ms | 1.338 |
| 8 | 27.75 us | 34.24 ms | 18,818/s | 21,358/s | 26 | 36.93 MB | 51.87 ms | 1.377 |
| 16 | 26.43 us | 33.11 ms | 20,061/s | 22,692/s | 34 | 37.18 MB | 51.84 ms | 1.308 |
| 32 | 28.29 us | 32.83 ms | 20,085/s | 23,433/s | 50 | 37.96 MB | 51.49 ms | 1.317 |
| 64 | 31.20 us | 32.63 ms | 18,307/s | 23,143/s | 82 | 39.25 MB | 51.56 ms | 1.383 |

Cap 2 is the only value within 5% of the best balanced score. It produced the best completion-burst
and both concurrent-throughput results, retained no clients, delivered every scheduled completion,
and used 20 total threads with one live MQTT client. Larger caps had isolated single-completion wins,
but those did not offset their additional threads and weaker combined scores. The smaller pool likely
reduces contention among workers that ultimately serialize access to standard CPython, but timing
results remain specific to this host and workload.

The production cap is therefore 2. Wrapper-owned native start and join operations are admitted
through a semaphore with `cap - 1` slots, so at cap 2 only one potentially long native operation can
run while one blocking worker remains available for Python result delivery. A single wall-clock
shutdown budget covers both waiting for that slot and awaiting the blocking task. A timed-out task
retains its slot until it actually exits, preventing callers from exceeding the cap by abandoning
join handles. In the adversarial benchmark, all native-operation slots were occupied for 250 ms and
a close with a 50 ms budget returned `TIMEOUT` in 50.90--51.87 ms at every tested cap. The
benchmark-only environment override and saturation probe are not compiled into normal wheels.

A post-review regression run at the production cap is preserved in
`raw/tokio-cap-2-immediate-close.json`. With the native-operation slot occupied for 250 ms, an
immediate close exhausted its 50 ms join budget in 51.41 ms, but the separately dispatched shutdown
was observed as a terminal non-graceful event 0.36 ms later, before the blocker released the slot. This
distinguishes bounded termination observation from the nonblocking shutdown request: a timeout no
longer prevents the driver and connection from being closed.

The corresponding graceful-close regression is preserved in
`raw/tokio-cap-2-graceful-close.json`. With the same 250 ms slot blocker, graceful close exhausted its
50 ms budget in 51.65 ms and the immediate escalation was observed as a terminal non-graceful event
0.33 ms later. Python has already made the client unusable at that point, so escalation is required:
returning a graceful timeout while leaving a resumable native connection would split the adapter and
driver lifecycle states indefinitely.

Cancellation coverage at the production cap is preserved in
`raw/tokio-cap-2-graceful-cancellation.json`. The graceful-close future was canceled while the sole
native-operation slot remained occupied; its cancellation guard requested immediate shutdown and a
terminal non-graceful event was observed 0.32 ms later. The guard is armed before slot admission and
disarmed only on confirmed graceful success, so cancellation is safe both while queued and after a
blocking graceful-close task has started.
