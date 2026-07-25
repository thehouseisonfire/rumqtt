# TODO: Complete Atomic Blob Store Validation

## Objective

Complete the remaining lifecycle, cross-facade, streaming, and performance
validation for `session-store-file/atomic-blob-store`.

The supported abstraction is:

> Within one process, atomically save, load, inspect, quarantine, and clear one
> bounded blob per opaque key on a trusted local filesystem, with same-key FIFO
> ordering, bounded different-key concurrency, accidental-corruption detection,
> genuine bounded-memory streaming, and deterministic explicit shutdown.

The crate is not a database, queue, object service, append-only log,
multi-process coordination service, or security boundary. Do not add
cross-process locking, authentication, encryption, compare-and-swap,
transactions, or hostile-filesystem defenses.

## Relevant Architecture and Invariants

The package has one executor-neutral engine shared by two facades:

- `BlockingAtomicBlobStore` provides blocking complete operations and
  `Read`/`Write` streaming.
- `atomic_blob_store::tokio::AtomicBlobStore`, behind the opt-in `tokio`
  feature, pumps borrowed `AsyncRead`/`AsyncWrite` endpoints and waits for
  engine results.
- One named coordinator OS thread per store owns ordering, admission,
  maintenance barriers, lifecycle, and completion bookkeeping.
- A per-store, lazily started, bounded worker pool owns synchronous filesystem
  execution.
- Runtime-neutral bounded channels carry streaming chunks and completions
  between the facades and engine.

Preserve these contracts:

- Complete operations submit when their methods are called. Dropping the
  returned waiter discards only its result.
- Borrowed streaming operations submit on first poll or blocking call.
- Operations for one key execute in FIFO submission order. Different keys may
  execute concurrently up to `max_concurrent_operations`.
- Store-wide maintenance, `flush`, and `close` retain coordinator order
  relative to keyed operations.
- `flush` includes operations accepted before its barrier, excludes later
  operations, does not close the store, and remains ordered if its waiter is
  dropped.
- `close` is ordered, shared by all clones, deterministic, idempotent, drains
  earlier accepted work, rejects later work with `StoreClosed`, and joins all
  store-owned execution resources.
- Every concurrent `close` caller waits for coordinator termination and
  observes the shared coordinator-join outcome.
- Last-handle drop initiates best-effort drainage without blocking `Drop`.
  Callers requiring an observable shutdown point must use `close`.
- Streaming save cancellation before the engine accepts the explicit
  input-complete marker aborts staging and preserves the previous blob. After
  that marker, commit is accepted work and must survive caller task or runtime
  loss.
- Streaming load validates the complete envelope before producing output,
  performs the two-pass seek-and-stream flow, and does not flush or shut down
  the caller's destination.
- Streaming transports remain bounded independently of blob size.
- V1 envelope bytes, BLAKE3 filename derivation, validation, namespaces,
  platform durability behavior, and public error meanings remain stable.
- Independent stores do not coordinate, even when opened over the same root
  and namespace.
- Engine and filesystem modules remain independent of Tokio. A blocking-only
  dependency graph must not contain Tokio.

Use barriers, channels, and deterministic test hooks for concurrency and
failure tests. Do not use sleeps as the synchronization mechanism. A bounded
timeout may guard a test against deadlock only after deterministic
synchronization establishes the state being tested.

## 1. Complete Lifecycle and Runtime-Independence Tests

Add deterministic tests for the lifecycle transitions and barrier behaviors
specified below.

### Flush ordering

Prove that:

- `flush` waits for all earlier accepted operations;
- work submitted after a `flush` barrier is excluded from that barrier;
- dropping a `flush` waiter does not remove or reorder the barrier;
- multiple interleaved flush barriers preserve submission order;
- a completed `flush` leaves the store open and usable.

The assertions must distinguish operation acceptance, dispatch, completion,
and waiter completion. Do not infer ordering solely from final filesystem
state.

### Close ordering and shared completion

Prove that:

- `close` drains earlier complete operations;
- later operations are rejected with `StoreClosed` across all clones;
- a close behind multiple queued operations preserves their coordinator order;
- close waits for a streaming save after input-complete acceptance;
- close remains behind a pre-input-complete stalled stream until that stream
  is cancelled;
- operations after successful close return the documented lifecycle error;
- operations after failed close return the documented lifecycle error;
- blocking and Tokio callers can close concurrently without deadlock;
- every concurrent close caller waits until workers and the coordinator have
  terminated;
- every concurrent close caller observes the same coordinator-join result.

The coordinator-exit test must keep the coordinator deterministically paused
after close results become available but before thread exit, and must show that
no caller returns during that interval.

### Last-handle drainage and resource termination

Prove that dropping the final handle:

- drains an already accepted complete operation;
- allows an accepted post-input-complete streaming save to commit;
- aborts a pre-input-complete streaming save without replacing the canonical
  blob;
- eventually terminates the coordinator and every lazily started worker;
- leaves no active-key or registry bookkeeping behind.

Resource-termination assertions must observe the engine's owned resources
directly through private test instrumentation or deterministic exit
notifications. Completion of the final filesystem operation alone is
insufficient evidence.

### Caller-runtime independence

Use independently constructed current-thread Tokio runtimes to prove that:

- a store opened under one runtime remains usable and closable under another;
- a complete operation accepted under one runtime completes after that runtime
  is destroyed;
- streaming input accepted through input-complete commits after its task and
  runtime are destroyed;
- destroying a task/runtime before input-complete aborts staging and preserves
  the previous canonical blob;
- a new runtime can observe the resulting state;
- the blocking facade operates and closes in a process that never constructs a
  Tokio runtime.

Also verify the blocking-only build and dependency graph:

```bash
cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo tree --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features
```

## 2. Complete Cross-Facade and Streaming Conformance

Build a reusable scripted conformance suite that executes equivalent scenarios
through the blocking and Tokio facades. The suite must compare observable
results, not merely confirm that each facade succeeds independently.

For scenarios supported by both I/O models, compare:

- result values and public error categories;
- final canonical bytes and diagnostic paths;
- same-key FIFO behavior and different-key admission;
- maintenance, `flush`, and `close` ordering;
- early EOF, trailing input, source failure, and destination failure;
- validation-before-output and exact `BlobMetadata`;
- cancellation or source-drop behavior at equivalent stream states;
- destination ownership, including the absence of facade-initiated flush or
  shutdown.

Do not expose a mixed blocking/Tokio public handle API for testing. Keep the
script and adapters test-only.

Run the streaming boundary matrix through both facades:

- empty, one-byte, one-chunk, one-chunk-plus-one, multi-chunk, maximum-sized,
  and over-limit blobs;
- early EOF before data, within a chunk, and exactly at chunk boundaries;
- trailing input after the declared length;
- source errors before and after at least one chunk;
- destination errors before and after at least one chunk;
- cancellation or source drop before the first chunk and while transport
  backpressure is active;
- cancellation during the final EOF probe;
- cancellation immediately before and immediately after input-complete
  acceptance;
- worker failure while a source read is pending;
- worker failure while the final EOF probe is pending;
- invalid envelopes producing no output;
- destinations not being flushed or shut down;
- bounded transport memory for payloads substantially larger than the chunk
  size.

Blocking I/O cannot represent task cancellation identically to async I/O.
Where exact equivalence is impossible, define the corresponding blocking event
precisely—for example, source drop, endpoint disconnection, or an injected
reader/writer failure—and document that mapping in the test.

Keep immutable V1 fixtures independent of the encoder under test. The
conformance work must not regenerate or rewrite them.

## 3. Complete the Performance Harness and Paired Measurements

Extend the maintained persistence benchmark rather than adding a one-off
harness. Preserve the existing machine-readable schema and command structure.

### Missing scenarios

Add measurements for:

- maintenance-barrier latency, including an ordered barrier behind active work;
- slow-source streaming backpressure;
- slow-destination streaming backpressure;
- MQTT v4 and v5 recovery/load latency through the production adapter path.

Slow endpoints must use deterministic coordination rather than arbitrary
sleeps. Record separately:

- time until transport backpressure is established;
- operation completion latency after the endpoint is released;
- configured chunk size and channel capacity;
- payload size and worker bound.

The maintenance scenario must record barrier latency both when idle and when
queued behind accepted keyed work. It must verify the expected ordering while
measuring it.

The MQTT recovery scenario must measure loading and applying an existing
checkpoint separately from save/barrier latency and checkpoint growth. Keep
protocol version, checkpoint shape, payload size, inflight count, and
filesystem configuration explicit in the output.

### Paired execution

Run paired baseline and final measurements for the new scenarios using:

- the same maintained harness revision;
- release builds with the same profile;
- identical platform, filesystem, payload, worker, and sample configuration;
- alternating baseline/final execution where practical to limit host drift;
- enough samples to report p50, p95, and p99 without presenting short-loopback
  noise as a performance claim.

Every retained result must include:

- command and run identifier;
- baseline and final commit identifiers;
- build profile and Rust toolchain;
- operating system, architecture, filesystem, and relevant mount details;
- payload and chunk sizes;
- sample count and worker configuration;
- raw samples in the existing JSON schema.

Explain noise and any material regression. An unexplained material regression
fails the gate.

Update:

- `session-store-file/benchmarks/README.md`;
- `session-store-file/benchmarks/PERSISTENCE.md`;
- `session-store-file/benchmarks/PERSISTENCE-RESULTS.md`.

## Validation Commands

At minimum, run:

```bash
cargo fmt --manifest-path session-store-file/Cargo.toml --all --check

cargo check --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --features tokio

cargo test --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v4

cargo test --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v5

cargo test --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v4,v5

cargo test --manifest-path session-store-file/Cargo.toml --workspace

cargo clippy --manifest-path session-store-file/Cargo.toml \
  --workspace --all-targets --all-features -- -D warnings

cargo doc --manifest-path session-store-file/Cargo.toml \
  --workspace --all-features --no-deps
```

Run the new benchmark scenarios in addition to the existing paired matrix.
Run relevant main-workspace client tests if adapter behavior or public rumqtt
APIs change.

Repeat the source audits:

```bash
rg -n 'tokio|spawn_blocking|Handle|Runtime' \
  session-store-file/atomic-blob-store/src

rg -n -i 'rumqtt|mqtt|session|checkpoint|legacy' \
  session-store-file/atomic-blob-store
```

Tokio source matches must remain confined to the feature-gated facade and its
tests. Protocol-specific terminology must not enter the generic crate.

## Acceptance Criteria

The requirements are satisfied when:

- deterministic tests directly prove dropped and interleaved `flush` barrier
  behavior;
- deterministic tests directly prove ordered, shared close completion and
  termination of all store-owned threads;
- last-handle drainage is verified for complete and streaming operations,
  including direct resource-termination evidence;
- accepted complete and streaming work survives caller-runtime loss at the
  documented cancellation cutover;
- one reusable conformance suite runs the required result, error, ordering,
  cancellation, ownership, and streaming-boundary scenarios through both
  facades;
- streaming transport remains demonstrably bounded for payloads much larger
  than its chunk size;
- V1 fixtures, canonical paths, adapter bytes, and public error categories
  remain stable;
- the maintained harness measures maintenance latency, deterministic
  slow-endpoint backpressure, and MQTT v4/v5 recovery/load latency;
- paired results for the new scenarios are retained with complete methodology
  and no unexplained material regression;
- blocking-only builds contain no Tokio dependency;
- formatting, feature builds, workspace tests, Clippy, docs, dependency
  audits, and terminology audits pass.
