# TODO: Implement an Executor-Neutral Atomic Blob Store Core with a Tokio Adapter

## Purpose

Refactor `session-store-file/atomic-blob-store` into the complete architecture selected after the
runtime review in `TODO8.md`:

> One executor-neutral, process-local coordination engine owns filesystem execution, same-key FIFO,
> concurrency admission, barriers, streaming commit state, and shutdown. A blocking facade and a
> Tokio-specific async facade use that same engine. The core must not depend on a caller runtime for
> accepted filesystem work to finish.

This is an implementation plan for a fresh session. It is intentionally prescriptive about semantics
and boundaries. Do not begin by adding caller-provided Tokio `Handle` support. A bare `Handle` does
not own the runtime and cannot guarantee that accepted-but-not-started blocking jobs survive runtime
shutdown.

The redesign must preserve the existing on-disk format, platform durability behavior, public error
meaning, same-key ordering, genuine bounded-memory streaming, and rumqtt compatibility.

## Required Preparation

Before editing:

1. Read the repository `AGENTS.md`.
2. Read `TODO8.md`, especially sections 6 and 7.
3. Read this file completely.
4. Trace the current implementation and tests in:

   - `session-store-file/atomic-blob-store/src/lib.rs`;
   - `session-store-file/atomic-blob-store/src/tests.rs`;
   - `session-store-file/atomic-blob-store/README.md`;
   - `session-store-file/atomic-blob-store/FORMAT.md`;
   - `session-store-file/adapter/src/shared.rs`;
   - `session-store-file/benchmarks/src/persistence.rs`.
5. Run and record the baseline:

```bash
cargo test --manifest-path session-store-file/Cargo.toml -p atomic-blob-store
cargo test --manifest-path session-store-file/Cargo.toml -p rumqttc-session-store-file-next --all-features
```

At the time this plan was written, the atomic blob store baseline was 47 unit tests and one
format-compatibility test passing on Unix. Do not treat that count as a substitute for running the
current tree.

## Architectural Decision

Implement architecture B as a single Cargo package with separable facades:

- An executor-neutral internal engine.
- A public blocking facade.
- An optional public Tokio facade behind an explicit `tokio` feature.

Use one engine and one scheduler implementation. Do not create independent blocking and async
coordination implementations.

The engine may own ordinary OS threads and blocking synchronization. “Executor-neutral” means:

- no Tokio types in core configuration, filesystem, scheduler, worker, lifecycle, or format modules;
- no reliance on a caller async runtime to run accepted filesystem operations;
- no generic executor trait whose only real implementation is Tokio;
- completion and streaming transports may be runtime-neutral futures/channels if they also support
  blocking worker access.

The package should support:

```bash
# Blocking/core-only build: must not compile or link Tokio.
cargo check --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

# Tokio facade.
cargo check --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --features tokio
```

Prefer `default = []`. The rumqtt adapter must explicitly enable the `tokio` feature. If compatibility
pressure justifies temporarily default-enabling Tokio, document that as a migration-only choice and
retain a tested `--no-default-features` build with no Tokio dependency.

## Non-Goals

Do not add any of the following as part of this refactor:

- caller-provided `tokio::runtime::Handle` execution;
- a generic async executor abstraction;
- cross-process coordination or locking;
- coordination between independently opened stores;
- a new on-disk envelope version;
- filename, hash, namespace, suffix, or domain changes;
- one-pass corrupt-data-to-output streaming loads;
- whole-blob buffering hidden behind streaming method names;
- cancellation of a commit after the streaming input-complete marker;
- configurable queue capacity unless separately justified and benchmarked;
- unrelated MQTT codec streaming changes.

The current coordinator bounds active filesystem operations, not the number or total memory of
queued complete-payload submissions. Preserve that behavior during the architectural refactor.
Evaluate queue-count backpressure separately after semantic parity is established.

## Invariants That Must Not Change

### Submission and ordering

- Complete `save`, `load`, `clear`, `inspect`, `quarantine`, cleanup, and `flush` operations are
  accepted or rejected when their method is called, not when their returned future is first polled.
- A dropped complete-operation future discards only its result. An accepted operation still runs.
- Borrowed streaming operations submit on first poll because the borrowed endpoint must be driven.
- Operations accepted for one key execute in FIFO submission order.
- Different keys may execute concurrently, but active operations never exceed
  `max_concurrent_operations`.
- Store-wide maintenance and lifecycle barriers preserve their position relative to key operations.
- Independent store instances do not share queues even if their roots and namespaces match.

### Save cancellation cutover

Streaming save has a core-owned state transition:

```text
Queued -> Feeding -> InputComplete -> Committing -> Complete
                    \
                     cancellation before InputComplete -> AbortStaging
```

- Reader error, task cancellation, adapter drop, or transport closure before `InputComplete` aborts
  staging and preserves the previous canonical blob.
- Once the engine accepts `InputComplete`, commit is accepted work and must finish independently of
  the caller future and caller runtime.
- Dropping the result receiver after `InputComplete` discards only the result.
- Declared length, early EOF, trailing input, checksum calculation, and maximum-size enforcement
  remain unchanged.

### Load streaming

- Validate the complete envelope before writing the first destination byte.
- Keep the existing two-pass seek-and-stream design.
- Use bounded fixed-size chunks; memory must not scale with blob size.
- Destination failure or cancellation may leave caller-owned output partially written.
- Invalid envelopes produce no output.
- Do not flush or shut down the caller’s destination.
- Preserve the current acknowledgement rule: the same-key operation remains active until the
  destination has consumed all chunks or cancellation is observed.

### Filesystem and format

- V1 bytes and filename derivation remain exactly stable.
- Save replacement retains old-or-new complete canonical-path behavior.
- Clear retains old-or-absent behavior.
- Atomic commit ambiguity remains represented as such.
- Platform-specific directory synchronization, staging, quarantine, and cleanup behavior remains
  unchanged.
- No Tokio worker or other async executor thread performs blocking filesystem calls.

## Lifecycle Contract

Add a first-class shared lifecycle:

```text
Open -> Closing -> Closed
  \        \
   \        infrastructure failure
    -> Failed
```

All clones observe the same lifecycle.

### `flush`

`flush()` is an ordered barrier:

- it waits for every operation accepted before the barrier;
- it does not reject or include operations accepted after the barrier;
- dropping its result future does not remove the barrier;
- it does not close the store;
- it may wait indefinitely behind a streaming operation whose endpoint is stalled.

Do not aggregate ordinary earlier operation errors into `flush()`. Those results belong to their
operations. `flush()` reports barrier/engine failure.

### `close`

`close()` is a store-wide ordered lifecycle barrier:

- its linearization point is coordinator ordering, not only an API-side atomic flag;
- it stops acceptance of later operations across every clone;
- it drains all complete work accepted before it;
- an input-complete streaming save before it drains through commit;
- an earlier pre-input-complete or stalled stream must complete or cancel before close can finish;
- it shuts down and joins the store-owned execution resources;
- it is idempotent;
- concurrent callers observe one shared closing outcome;
- after closure, every operation returns a distinct `StoreClosed` error rather than a generic
  coordination failure.

Like `flush()`, `close()` does not aggregate discarded operation-specific errors. It must report
engine, worker, or shutdown failure.

Suggested facade signatures:

```rust
impl BlockingAtomicBlobStore {
    pub fn flush(&self) -> Result<(), AtomicBlobStoreError>;
    pub fn close(&self) -> Result<(), AtomicBlobStoreError>;
}

impl tokio::AtomicBlobStore {
    pub fn flush(&self) -> Operation<()>;
    pub fn close(&self) -> Operation<()>;
}
```

If `Operation<T>` is not the final public name, retain the semantics: construction submits complete
operations immediately, and polling only waits for the result.

### Last-handle drop

Dropping the last public handle must initiate best-effort orderly drainage of already accepted work.
It cannot report errors and must not block an arbitrary async worker in `Drop`.

Internal event/completion sender ownership must keep the coordinator alive until queued and active
accepted work has resolved. Explicit `close()` is the only deterministic and observable shutdown API.

Document that process termination can still interrupt best-effort drop drainage; callers requiring a
known shutdown point must call `close()`.

## Public API Direction

The crate is pre-release, so prefer a clear API over preserving misleading top-level Tokio coupling.

### Shared public types

Keep these executor-neutral and shared:

- `BlobFormatIdentity`;
- `AtomicBlobStoreOptions`;
- `AtomicBlobStoreError`;
- `AtomicBlobStoreConfigError`;
- `StoreOperation`;
- `BlobState`;
- `BlobInspection`;
- `BlobMetadata`;
- `QuarantineInfo`;
- cleanup report types;
- `blob_filename`.

Add lifecycle errors deliberately:

```rust
#[non_exhaustive]
pub enum AtomicBlobStoreError {
    // Existing variants...
    StoreClosed,
    EngineFailed,
    WorkerUnavailable,
    ShutdownFailure { /* source/context as appropriate */ },
}
```

Do not collapse filesystem, corruption, streaming, worker, closed-store, and shutdown errors into one
coordination variant. Callers must be able to distinguish retry, data repair, reconfiguration, and
lifecycle misuse.

### Blocking facade

Provide an idiomatic public blocking type:

```rust
pub struct BlockingAtomicBlobStore;

impl BlockingAtomicBlobStore {
    pub fn open(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
    ) -> Result<Self, AtomicBlobStoreError>;

    pub fn load(&self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError>;
    pub fn save(&self, key: &[u8], payload: Vec<u8>) -> Result<(), AtomicBlobStoreError>;

    pub fn save_from<R: Read + ?Sized>(
        &self,
        key: &[u8],
        reader: &mut R,
        declared_len: u64,
    ) -> Result<(), AtomicBlobStoreError>;

    pub fn load_into<W: Write + ?Sized>(
        &self,
        key: &[u8],
        writer: &mut W,
    ) -> Result<Option<BlobMetadata>, AtomicBlobStoreError>;

    // clear, inspect, quarantine, cleanup, flush, close, blob_path
}
```

Blocking streaming must use the same engine stream protocol as Tokio streaming. Do not bypass the
coordinator and do not hold an unrelated per-key mutex while performing direct filesystem work.

### Tokio facade

Place Tokio integration behind the `tokio` feature:

```rust
#[cfg(feature = "tokio")]
pub mod tokio {
    pub struct AtomicBlobStore;

    impl AtomicBlobStore {
        pub async fn open(...) -> Result<Self, AtomicBlobStoreError>;

        pub fn load(&self, key: &[u8]) -> Operation<Option<Vec<u8>>>;
        pub fn save(&self, key: &[u8], payload: Vec<u8>) -> Operation<()>;

        pub async fn save_from<R>(
            &self,
            key: &[u8],
            reader: &mut R,
            declared_len: u64,
        ) -> Result<(), AtomicBlobStoreError>
        where
            R: ::tokio::io::AsyncRead + Unpin + Send + ?Sized;

        pub async fn load_into<W>(
            &self,
            key: &[u8],
            writer: &mut W,
        ) -> Result<Option<BlobMetadata>, AtomicBlobStoreError>
        where
            W: ::tokio::io::AsyncWrite + Unpin + Send + ?Sized;

        // clear, inspect, quarantine, cleanup, flush, close, blob_path
    }
}
```

The Tokio facade must not call `tokio::task::spawn_blocking` for canonical store filesystem work.
Its runtime drives only borrowed endpoint I/O and waits for engine completions.

`open()` must also avoid blocking a caller Tokio worker. It may submit initialization to an
engine-owned bootstrap path or use a short-lived ordinary thread. Do not require a current runtime
handle merely to normalize or initialize the root.

### Compatibility

Choose one clearly documented transition:

- move the async store to `atomic_blob_store::tokio::AtomicBlobStore` and update callers; or
- temporarily provide a deprecated top-level Tokio alias when the feature is enabled.

Do not use the name `AtomicBlobStore` for both blocking and Tokio types at the same module path.
Do not retain permanent aliases that obscure execution ownership.

## Internal Module Layout

Split the current monolithic `lib.rs` approximately as follows:

```text
atomic-blob-store/src/
  lib.rs
  config.rs
  error.rs
  format.rs
  path.rs
  filesystem/
    mod.rs
    unix.rs
    windows.rs
  engine/
    mod.rs
    event.rs
    lifecycle.rs
    operation.rs
    scheduler.rs
    stream.rs
    workers.rs
  blocking.rs
  tokio.rs
  tests/
    mod.rs
    ...
```

Exact file boundaries may change, but preserve these responsibilities:

- `format`: headers, checksum, bounded decoding, fixtures, format metadata;
- `path`: namespace and suffix validation, key hashing, canonical paths;
- `filesystem`: synchronous platform operations only;
- `engine`: ordering, admission, barriers, cancellation states, completion, shutdown;
- `blocking`: blocking endpoint pumping and blocking waits;
- `tokio`: Tokio endpoint pumping and async waits.

Do not expose platform backends or engine commands as public APIs merely to make the split easier.

## Engine and Worker Design

### Coordinator

Retain one process-local coordinator per opened store unless measurements prove another design is
superior. It should be an ordinary named OS thread using blocking receive.

The coordinator owns:

- the key-to-queue registry;
- the active-key set;
- the configured active-operation count;
- the global pending/barrier queue;
- lifecycle transition ordering;
- dispatch and completion bookkeeping.

Use one event path for blocking and Tokio facades.

### Worker facility

Replace the private Tokio runtime with an owned bounded blocking executor.

Requirements:

- no more than the configured or explicitly documented worker capacity executes at once;
- no thread per operation;
- worker panics are caught at the job boundary;
- a panicked job releases its key and completes its waiter with an engine/worker error;
- a worker remains usable after an ordinary job panic when sound to do so;
- close joins workers deterministically;
- idle thread cost is measured;
- thread names identify the blob store without exposing roots or keys.

Do not immediately write a large general-purpose thread-pool library. First evaluate a small
store-specific pool against a maintained dependency. Record:

- thread creation policy: eager or lazy;
- queue ownership and shutdown;
- panic behavior;
- join behavior;
- per-store versus process-shared workers;
- impact on the per-store concurrency guarantee.

Prefer a per-store owned pool initially because its close and drainage ownership are unambiguous.
If an eager pool creates unacceptable idle overhead, implement or select a bounded lazy pool before
release. A global pool is acceptable only if cross-store contention and shutdown ownership are
specified and tested.

### Runtime-neutral transport

The stream transport must support:

- bounded async send/receive for the Tokio endpoint side;
- blocking send/receive for filesystem workers and the blocking facade;
- closure detection as cancellation;
- no runtime-specific wakeup assumptions;
- a clear input-complete message separate from channel closure;
- prompt worker-error delivery while the async reader or EOF probe is pending.

Evaluate a maintained runtime-neutral channel such as `async-channel` or an equivalently small
abstraction. Do not commit to a dependency until a focused prototype verifies:

1. bounded backpressure in both blocking and async directions;
2. sender/receiver drop behavior;
3. cancellation wakeups;
4. no hidden executor/runtime;
5. required minimum Rust version;
6. panic and close behavior.

Keep this prototype isolated or delete it after recording the conclusion.

Completion waiting may use the same transport or a runtime-neutral oneshot. Avoid a bespoke unsafe
waker implementation.

## Implementation Stages

Each stage must compile and have focused tests before proceeding. Keep commits squash-friendly but
make the work reviewable in these conceptual units.

### Stage 1: Freeze contracts and add missing lifecycle tests

- Add tests that explicitly capture current submission timing.
- Add tests for all-handle drop drainage.
- Add tests for barriers behind stalled streaming operations.
- Add tests for cancellation immediately around the input-complete boundary.
- Add tests for load acknowledgement ordering.
- Add test helpers that observe coordinator and worker events without sleeping when possible.
- Document the intended `flush` versus `close` distinction.

Do not refactor behavior in this stage.

### Stage 2: Extract pure synchronous filesystem and format modules

- Move configuration, format, path, and platform functions out of `lib.rs`.
- Remove Tokio types from all extracted modules.
- Preserve exact errors and failpoint coverage.
- Keep golden fixtures immutable.
- Run format, interruption, Unix, and Windows-specific tests where available.

The existing async facade should still function through the current coordinator at the end of this
stage.

### Stage 3: Introduce engine lifecycle and explicit close

- Add `Open`, `Closing`, `Closed`, and `Failed`.
- Add ordered close events and idempotent waiting.
- Reject post-close work distinctly.
- Ensure concurrent clones cannot bypass close.
- Preserve implicit last-handle drainage.
- Test worker/coordinator panic and channel-closure paths.

It is acceptable for this stage still to use the private Tokio blocking pool internally, provided the
new lifecycle semantics are correct and isolated behind an execution interface.

### Stage 4: Introduce an internal blocking-executor interface

- Define the smallest interface the scheduler needs to dispatch a `'static` filesystem job and
  receive exactly one completion.
- Adapt the current private Tokio runtime behind it temporarily.
- Ensure cancelled-before-start dispatch cannot strand an active key.
- Keep all normal and panic completions on one coordinator path.

Do not make this a public generic executor API.

### Stage 5: Replace the private Tokio runtime

- Implement or integrate the owned bounded worker facility.
- Remove Tokio from engine and filesystem modules.
- Verify accepted complete work drains after every caller runtime is dropped.
- Verify close joins all owned threads.
- Audit for `spawn_blocking`, `Handle`, `Runtime`, Tokio channels, and Tokio synchronization outside
  the Tokio facade and its tests.

At this checkpoint the canonical execution engine must be executor-neutral.

### Stage 6: Add the blocking facade

- Implement blocking complete operations using the engine.
- Implement genuine blocking `Read`/`Write` streaming through the shared bounded protocol.
- Preserve same-key FIFO and barriers across clones.
- Add standalone non-async examples and doctests.
- Prove `--no-default-features` has no Tokio dependency with `cargo tree`.

### Stage 7: Move async pumping into the Tokio facade

- Replace engine Tokio channels with the validated runtime-neutral transport.
- Implement Tokio streaming around the shared stream protocol.
- Preserve prompt worker-error selection while reads or EOF probes are pending.
- Preserve current future-drop semantics before and after input completion.
- Keep complete-operation submission at method call.
- Update examples and documentation to use the explicit Tokio module/feature.

### Stage 8: Migrate the rumqtt adapter

- Enable the `atomic-blob-store` Tokio feature explicitly.
- Update v4 and v5 imports and construction.
- Preserve namespaces, keys, filenames, domain tag, suffix, envelope bytes, and legacy precedence.
- Route legacy inspection away from caller `Handle::try_current().spawn_blocking()`.
- Keep MQTT session encode/decode behavior unchanged; it may still allocate a complete payload.
- Run adapter golden, dual-protocol, consumer, and process-restart tests.

### Stage 9: Documentation, audit, and performance gate

- Rewrite crate-level and README architecture documentation.
- Document blocking versus Tokio facade behavior.
- Document worker and coordinator thread costs.
- Document `flush`, `close`, drop, stalled-stream shutdown, and process termination.
- Update `CHANGELOG.md`.
- Run the dependency and terminology audits.
- Run before/after benchmarks and record results.
- Do not publish another alpha until all gates below pass.

## Required Tests

### Shared engine invariants

- same-key FIFO for every operation kind;
- different-key concurrency reaches but never exceeds the configured bound;
- a blocked key does not block unrelated keys when capacity remains;
- key registry entries are removed after completion;
- maintenance barriers preserve interleaved order;
- dropped complete-operation waiters do not cancel work;
- accepted work drains after every public handle is dropped;
- independent stores remain independent;
- worker panic cannot strand an active key;
- coordinator/worker infrastructure failure rejects new work deterministically.

### Lifecycle

- flush includes earlier and excludes later submissions;
- dropping a flush future leaves the barrier in place;
- close drains earlier submissions and rejects later submissions;
- concurrent close calls are idempotent and observe one outcome;
- close from one clone closes every clone;
- close waits behind an active streaming operation;
- cancellation releases a stalled stream so close can finish;
- close after input complete waits for the commit;
- operations after close return `StoreClosed`;
- last-handle drop does not leak coordinator or worker threads;
- explicit close can be called from both blocking and Tokio callers without deadlock.

### Cross-facade parity

Run the same scripted operation sequences through blocking and Tokio facades and compare:

- results and error categories;
- final canonical bytes;
- same-key ordering;
- barrier ordering;
- cancellation cutover where the blocking API has an equivalent source/drop failure;
- format and path outputs.

Also test blocking and Tokio clones of the same internal engine only if such mixed construction is
intentionally exposed. Do not expose mixed facades merely for testing convenience.

### Streaming

- empty, one-byte, one-chunk, multi-chunk, maximum, and over-limit blobs;
- early EOF at every relevant boundary;
- trailing input after declared length;
- reader error before and after at least one chunk;
- destination error before and after at least one chunk;
- cancellation before first chunk;
- cancellation under backpressure;
- cancellation during final EOF probe;
- cancellation immediately before and after input-complete acceptance;
- worker failure while source read is pending;
- worker failure while EOF probe is pending;
- validation-before-output;
- two-pass load returns exact metadata;
- destination is not flushed or shut down;
- memory remains bounded independently of blob size.

### Runtime independence

- open under one Tokio runtime and use under another;
- drop the construction runtime before use;
- submit a complete operation, drop its caller runtime, and verify it drains;
- complete streaming input, drop the Tokio task/runtime, and verify commit drains;
- drop the runtime before streaming input completion and verify staging aborts;
- use the blocking facade in a process with no Tokio runtime;
- compile and test with no Tokio feature.

### Compatibility and platforms

- immutable V1 golden fixtures;
- wrong domain, version, checksum, truncation, trailing bytes, and oversized declarations;
- Unix atomic replacement and interruption tests;
- Windows native replace, clear staging, cleanup, quarantine, and path tests;
- rumqtt v4/v5 golden key, filename, namespace, envelope, and codec tests;
- dual-protocol shared-root tests;
- consumer process-restart tests;
- no claim of cross-process safety.

Avoid timing-only concurrency assertions where hooks, barriers, or channels can prove the order
deterministically.

## Required Benchmarks

Record the current implementation before replacing its runtime, then compare the final design using
the maintained persistence harness.

Measure:

- open latency and close latency;
- idle OS thread count per store;
- peak thread count under configured concurrency;
- one, two, and many simultaneously opened stores;
- resident memory per idle and active store;
- complete save/load latency by payload size;
- streaming save/load latency by payload size;
- allocations and peak RSS for multi-chunk streaming;
- same-key serialized throughput;
- different-key scaling below, at, and above the concurrency bound;
- maintenance/flush/close barrier latency;
- cold worker startup;
- idle worker retirement if implemented;
- slow source and slow destination backpressure;
- rumqtt end-to-end save/load latency and checkpoint growth.

The redesign may improve lifecycle ownership even if raw latency is neutral, but it must not introduce
an unexplained material regression. Record platform, filesystem, build profile, payload sizes, and
worker configuration with results.

## Documentation Requirements

Update public documentation to state:

- the core is executor-neutral but owns blocking execution resources;
- the blocking and Tokio facades share one semantic engine;
- Tokio is an endpoint adapter, not the owner of filesystem jobs;
- complete-operation submission versus streaming first-poll submission;
- the exact streaming cancellation cutover;
- validation-before-output and two-pass load cost;
- same-key FIFO and different-key concurrency limits;
- queued-operation memory is not bounded by the active-operation limit;
- `flush` versus `close`;
- stalled streams may stall barriers and close;
- last-handle drop is best effort and explicit close is deterministic;
- independent stores and processes do not coordinate;
- thread/runtime overhead and feature selection.

Update:

- `session-store-file/atomic-blob-store/README.md`;
- crate-level rustdoc;
- `session-store-file/atomic-blob-store/CHANGELOG.md`;
- blocking and Tokio examples;
- rumqtt adapter README and examples if imports or features change;
- benchmark documentation and recorded results.

## Dependency and Source Audits

After the split:

```bash
cargo tree --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

rg -n 'tokio|spawn_blocking|Handle|Runtime' \
  session-store-file/atomic-blob-store/src
```

Every Tokio match must be confined to:

- the feature-gated Tokio facade;
- Tokio-specific tests;
- explicitly labeled migration documentation.

Also run the generic terminology audit required by `TODO8.md`:

```bash
rg -n -i 'rumqtt|mqtt|session|checkpoint' \
  session-store-file/atomic-blob-store
```

Every remaining match must be removed or explicitly justified.

## Validation Commands

At minimum:

```bash
cargo fmt --manifest-path session-store-file/Cargo.toml --all --check

cargo check --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --features tokio

cargo test --manifest-path session-store-file/Cargo.toml --workspace

cargo clippy --manifest-path session-store-file/Cargo.toml \
  --workspace --all-targets --all-features

cargo doc --manifest-path session-store-file/Cargo.toml \
  --workspace --all-features --no-deps
```

Run the repository’s relevant main-workspace tests if dependency declarations or rumqtt public APIs
change. Run supported Windows validation in CI or on an actual Windows environment; Unix success is
not evidence for native Windows shutdown and staging behavior.

## Completion Gates

Architecture B is complete only when all of the following are true:

- core, filesystem, scheduler, worker, lifecycle, and blocking modules contain no Tokio dependency;
- no caller runtime owns or gates canonical filesystem execution;
- a blocking-only build has no Tokio in its dependency graph;
- blocking and Tokio facades use one coordinator and operation state machine;
- genuine bounded-memory streaming remains available in both facades;
- same-key FIFO, different-key bounds, maintenance barriers, and cancellation cutover are unchanged;
- `flush` and deterministic, idempotent `close` are implemented and documented;
- accepted complete work and post-input-complete streaming commits survive caller runtime shutdown;
- last-handle drop drains accepted work best-effort without blocking `Drop`;
- workers and coordinator terminate after explicit close;
- all V1 bytes, paths, and golden fixtures remain unchanged;
- the rumqtt adapter no longer uses caller `spawn_blocking` for legacy inspection;
- Unix and Windows platform behavior has appropriate evidence;
- feature, dependency, documentation, clippy, test, and benchmark gates pass;
- `CHANGELOG.md` records the public API, dependency, lifecycle, and migration changes.

## Final Direction

The persistence guarantee follows execution ownership. Keep ownership inside the store.

Do not optimize away the private Tokio runtime by borrowing a runtime whose shutdown the store cannot
control. Remove Tokio from the engine instead. Preserve one coordinator, one set of invariants, one
streaming state machine, and one shutdown lifecycle, then provide idiomatic blocking and Tokio-facing
ways to drive it.
