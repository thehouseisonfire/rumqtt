# TODO: Finish and Validate the Executor-Neutral Atomic Blob Store

## Objective

Finish the engineering, validation, performance, and release work required for
`session-store-file/atomic-blob-store` to be maintained and published as an
independent general-purpose crate.

The supported abstraction is:

> Within one process, atomically save, load, inspect, quarantine, and clear one
> bounded blob per opaque key on a trusted local filesystem, with same-key FIFO
> ordering, bounded different-key concurrency, accidental-corruption detection,
> genuine bounded-memory streaming, and deterministic explicit shutdown.

The crate is not a database, queue, object service, append-only log,
multi-process coordination service, or security boundary. Do not add
cross-process locking, authentication, encryption, CAS, transactions, or
hostile-filesystem defenses as part of this work.

Keep the crate pre-release until every acceptance criterion in this document is
satisfied.

## Architecture and Invariants

The package has one executor-neutral engine shared by two facades:

- `BlockingAtomicBlobStore` provides blocking complete and `Read`/`Write`
  streaming operations.
- `atomic_blob_store::tokio::AtomicBlobStore`, behind the opt-in `tokio`
  feature, pumps borrowed `AsyncRead`/`AsyncWrite` endpoints and waits for
  engine results.
- One named coordinator OS thread per store owns ordering, admission,
  maintenance barriers, lifecycle, and completion bookkeeping.
- A per-store, lazily started, bounded worker pool owns synchronous filesystem
  execution. No caller runtime owns or gates accepted filesystem work.
- Runtime-neutral bounded channels carry streaming chunks and completions
  between facades and the engine.

Preserve these contracts while completing the work:

- Complete operations submit when their methods are called. Dropping their
  returned waiter discards only the result.
- Borrowed streaming operations submit on first poll or blocking call.
- Operations for one key execute in FIFO submission order. Different keys may
  execute concurrently up to `max_concurrent_operations`.
- Store-wide maintenance, `flush`, and `close` retain their coordinator order
  relative to keyed operations.
- `flush` includes operations accepted before its barrier, excludes later
  operations, does not close the store, and remains ordered if its waiter is
  dropped.
- `close` is ordered, shared by all clones, deterministic, idempotent, drains
  earlier accepted work, rejects later work with `StoreClosed`, and joins all
  store-owned execution resources.
- Last-handle drop initiates best-effort drainage without blocking `Drop`.
  Callers requiring an observable shutdown point must use `close`.
- Streaming save cancellation before the engine accepts the explicit
  input-complete marker aborts staging and preserves the previous blob. After
  that marker, commit is accepted work and must survive caller task or runtime
  loss.
- Streaming load validates the complete envelope before producing output,
  performs the existing two-pass seek-and-stream flow, and does not flush or
  shut down the caller's destination.
- Streaming transports remain bounded independently of blob size.
- V1 envelope bytes, BLAKE3 filename derivation, configuration validation,
  namespaces, platform-specific durability behavior, and public error meaning
  remain stable.
- Independent stores do not coordinate, even when opened over the same root and
  namespace.
- Unsupported targets compile and return `UnsupportedPlatform` when opening a
  store.

The MQTT adapter supplies its canonical domain, suffix, namespaces, and key
encoding. It recognizes only canonical files. Do not restore alternate legacy
filename discovery or arbitrary-filename access.

## 1. Separate Internal Responsibilities

Refactor the remaining monolithic implementation into reviewable internal
modules without changing the public API or behavior. Use boundaries equivalent
to:

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
```

Exact filenames may differ, but responsibilities must be clear:

- `config` validates and owns immutable format and execution configuration.
- `error` contains executor-neutral public error and operation types.
- `format` owns headers, checksums, bounded decoding, and format metadata.
- `path` owns namespace/suffix validation, key hashing, and canonical paths.
- `filesystem` contains synchronous platform operations only.
- `engine` owns lifecycle, scheduling, admission, barriers, cancellation
  states, workers, and runtime-neutral transport.
- `blocking` and `tokio` contain only facade-specific endpoint pumping and
  result waiting.

Do not expose engine commands, test hooks, or platform backends publicly. Keep
Tokio types and calls confined to `tokio.rs` and Tokio-specific tests. Preserve
the non-Unix/non-Windows compile path while moving platform code.

Run focused format, scheduler, lifecycle, facade, and platform tests after each
logical extraction. Immutable format fixtures must never be regenerated by the
implementation under test.

## 2. Add Deterministic Engine-Failure Coverage

Introduce minimal test-only hooks or injectable internal boundaries that can
exercise infrastructure failures without sleeps or production-only behavior
changes.

Add tests proving:

- a filesystem job panic is caught at the job boundary;
- the panicked operation completes with the documented engine/worker error;
- its key is released and later same-key work cannot be stranded;
- the worker facility remains usable after an ordinary caught job panic;
- worker startup or dispatch failure releases admission and completes the
  waiter;
- coordinator channel loss transitions the shared lifecycle consistently and
  rejects new work deterministically;
- a worker join panic makes `close` return `ShutdownFailure`, and concurrent
  close callers observe the same outcome;
- maintenance-job panic or dispatch failure clears the maintenance-active state
  and cannot strand later work;
- infrastructure failure cannot leave registry entries, active keys,
  coordinator threads, or worker threads behind.

Keep error categories distinct. Do not collapse `StoreClosed`, `EngineFailed`,
`WorkerUnavailable`, `ShutdownFailure`, filesystem errors, corruption, and
stream endpoint errors into a generic coordination error.

## 3. Complete Lifecycle and Runtime-Independence Tests

Use barriers, channels, and deterministic test hooks rather than timing-only
assertions.

Add lifecycle tests for:

- `flush` waiting for all earlier operations while excluding later
  submissions;
- dropping a `flush` waiter without removing or reordering the barrier;
- multiple interleaved flush barriers;
- `close` draining earlier complete operations and rejecting all later
  operations across clones;
- `close` waiting for a streaming save whose input-complete marker has already
  been accepted;
- `close` waiting behind a pre-input-complete stalled stream until cancellation;
- operations after successful or failed close returning the documented
  lifecycle error;
- explicit close from blocking and Tokio callers without deadlock;
- last-handle drainage terminating coordinator and worker threads, not merely
  completing the final filesystem operation.

Add runtime-independence tests for:

- opening under one Tokio runtime and using the store under another;
- submitting a complete operation, destroying its caller runtime, and observing
  completion from a new runtime or the filesystem;
- completing streaming input, destroying the task/runtime, and observing the
  committed blob;
- destroying the task/runtime before input completion and observing staging
  abort with the previous canonical blob preserved;
- using and closing the blocking facade in a process that never constructs a
  Tokio runtime;
- building and testing the crate without the Tokio feature or any Tokio
  dependency in the resolved graph.

## 4. Complete Cross-Facade and Streaming Coverage

Build a reusable scripted conformance suite that runs equivalent sequences
through the blocking and Tokio facades. Compare:

- result values and public error categories;
- final canonical bytes and diagnostic paths;
- same-key FIFO and different-key admission;
- maintenance, `flush`, and `close` ordering;
- early EOF, trailing input, reader failure, and destination failure;
- validation-before-output and exact `BlobMetadata`;
- cancellation/source-drop behavior at equivalent stream states.

Do not expose a mixed blocking/Tokio handle API merely for testing.

Complete the streaming boundary matrix in both facades:

- empty, one-byte, one-chunk, multi-chunk, maximum-sized, and over-limit blobs;
- early EOF before data, within a chunk, and at chunk boundaries;
- trailing input after the declared length;
- source errors before and after at least one chunk;
- destination errors before and after at least one chunk;
- cancellation or source drop before the first chunk and under backpressure;
- cancellation during the final EOF probe;
- cancellation immediately before and after input-complete acceptance;
- worker failure while a source read or EOF probe is pending;
- invalid envelopes producing no output;
- destinations not being flushed or shut down;
- bounded transport memory for payloads much larger than the chunk size.

Where a behavior cannot be represented identically by blocking and async I/O,
document the precise correspondence used by the conformance test.

## 5. Extend and Run the Performance Harness

Extend the maintained persistence benchmark rather than creating an
unreviewed, one-off harness. Emit machine-readable results using the existing
schema and record the commands, commit identifiers, build profile, platform,
filesystem, payload sizes, sample counts, and worker configuration.

Measure paired baseline and final results for:

- store open and deterministic close latency;
- idle coordinator and worker thread count per store;
- peak thread count below, at, and above configured concurrency;
- one, two, and many simultaneously open stores;
- resident memory for idle and active stores;
- complete save and load latency across representative payload sizes;
- streaming save and load latency across the same payload sizes;
- allocation count and peak RSS for multi-chunk streaming;
- same-key serialized throughput;
- different-key scaling below, at, and above the configured bound;
- maintenance, `flush`, and `close` barrier latency;
- cold lazy-worker startup;
- slow-source and slow-destination backpressure;
- rumqtt v4/v5 end-to-end save/load latency and checkpoint growth.

Use a sound platform-specific method for thread, RSS, and allocation
measurements and document its limitations. Do not infer allocation behavior
only from source inspection.

Explain noise and any material regression. Lifecycle ownership may justify a
neutral raw-latency result, but an unexplained material regression fails the
gate. Update:

- `session-store-file/benchmarks/README.md`;
- `session-store-file/benchmarks/PERSISTENCE.md`;
- `session-store-file/benchmarks/PERSISTENCE-RESULTS.md`.

## 6. Obtain Supported-Platform Evidence

Run the full native suite on both Unix and Windows implementations. CI
configuration alone is not evidence of a passing platform run.

Windows evidence must include:

- native create and replace;
- clear staging and old-or-absent interruption behavior;
- quarantine, including rename-success/sync-failure ambiguity;
- owned temporary-file cleanup and age handling;
- relative and extended paths, including non-Unicode wide units;
- lifecycle drainage, worker panic recovery, and deterministic close;
- blocking and Tokio streaming through the native backend.

Unix evidence must include:

- atomic replacement and interruption behavior;
- directory synchronization and clear semantics;
- quarantine ambiguity;
- dependency-owned temporary-file behavior;
- lifecycle drainage, worker panic recovery, and deterministic close;
- blocking and Tokio streaming through the native backend.

Record the operating system, filesystem, toolchain, commands, and results in
the benchmark/validation documentation. Do not describe Unix results as Windows
evidence or simulated fault injection as native platform evidence.

## 7. Final Documentation and Source Audits

After the module extraction and tests are complete, audit public rustdoc,
README, examples, diagnostics, thread names, feature names, package metadata,
and changelogs.

Documentation must continue to state:

- the abstraction, trust model, and non-goals;
- executor-neutral ownership of blocking execution resources;
- the shared semantics of the blocking and Tokio facades;
- submission timing for complete and streaming operations;
- streaming cancellation cutover and validation-before-output;
- same-key FIFO, different-key bounds, and unbounded queued complete-payload
  memory;
- `flush`, `close`, stalled-stream, last-handle, and process-termination
  behavior;
- independent-store and cross-process limitations;
- thread/runtime cost and feature selection;
- power-loss limitations and atomic-commit ambiguity;
- V1 format and semver compatibility policy.

Update the independent core and adapter changelogs for any public API,
dependency, lifecycle, or migration changes made by this work.

Run these audits:

```bash
cargo tree --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

rg -n 'tokio|spawn_blocking|Handle|Runtime' \
  session-store-file/atomic-blob-store/src

rg -n -i 'rumqtt|mqtt|session|checkpoint|legacy' \
  session-store-file/atomic-blob-store
```

The blocking-only dependency graph must not contain Tokio. Tokio source matches
must be confined to the feature-gated facade and its tests. Protocol and legacy
terminology must not appear in the generic crate.

## 8. Release and Product Gates

Before publishing:

1. Verify that the configured crates.io package name is available and suitable.
   If it is not, rename package metadata, dependency declarations,
   documentation, and examples consistently before reserving a name.
2. Confirm that maintainers accept the independent crate's compatibility,
   security-response, documentation, CI, and release-cadence cost.
3. Publish an alpha/pre-release only after all engineering, benchmark, audit,
   and native-platform gates pass.
4. Exercise the prerelease in at least one non-MQTT application and solicit
   external API/format feedback before declaring the API and format stable.
5. Resolve actionable feedback and rerun all affected gates.

The generic crate must retain an independent version and changelog. The MQTT
adapter must depend on an intentional compatible version range and re-export
only types that are part of its own contract.

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

Also run:

- the benchmark scenarios defined in Section 5;
- native Unix and Windows validation from Section 6;
- all dependency and source audits from Section 7;
- relevant main-workspace client tests if adapter dependencies or rumqtt public
  APIs change.

## Acceptance Criteria

The work is complete only when:

- internal module boundaries separate configuration, errors, format, paths,
  platform filesystem code, engine scheduling/lifecycle, and facade pumping;
- engine and platform modules have no Tokio dependency;
- the blocking-only graph contains no Tokio;
- deterministic tests cover worker panic, infrastructure failure, lifecycle
  failure, and resource termination;
- `flush`, `close`, last-handle drainage, and caller-runtime loss satisfy the
  contracts above;
- blocking and Tokio facades pass the shared conformance and streaming boundary
  suites;
- all V1 fixtures, canonical paths, MQTT adapter bytes, and public error
  categories remain stable;
- unsupported targets compile and fail at open with `UnsupportedPlatform`;
- the complete paired benchmark matrix is recorded with no unexplained material
  regression;
- native Unix and Windows runs provide platform-specific evidence;
- formatting, feature builds, tests, Clippy, docs, dependency audits, and
  terminology audits pass;
- package-name availability and independent-maintenance ownership are confirmed;
- a prerelease has been exercised outside MQTT and actionable feedback is
  resolved before stabilization.
