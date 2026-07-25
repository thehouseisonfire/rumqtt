# TODO: Harden Native Windows Validation in CI

## Objective

Increase confidence in the Windows implementation of
`session-store-file/atomic-blob-store` using deterministic tests executed on
native GitHub-hosted Windows runners.

The validation must cover the Windows filesystem backend, executor-neutral
engine, blocking facade, Tokio facade, and packaged crate. It must emphasize
failure boundaries, interruption states, shutdown, and path handling rather
than merely repeating successful operations.

This work does not attempt to prove that the implementation is bug-free,
simulate physical power loss, characterize every Windows filesystem, or
replace testing on production hardware. It must establish strong evidence for
the documented behavior that can reasonably be obtained from hosted CI.

Do not add scheduled or nightly workflows. Do not rely on pull-request events.

## Relevant Architecture and Contracts

The crate uses:

- one executor-neutral coordinator thread per open store;
- a lazily started, bounded worker pool for synchronous filesystem operations;
- bounded channels between the engine and its blocking and Tokio facades;
- native Windows wide-path APIs for staging, replacement, clear, quarantine,
  cleanup, and directory synchronization.

Preserve these contracts:

- a successful save leaves one complete old-or-new canonical blob;
- a failed or interrupted pre-commit save preserves the previous canonical
  blob;
- clear exposes only the previous blob or absence;
- quarantine may commit its rename before a later synchronization error is
  reported, and the resulting ambiguity must remain documented and observable;
- cleanup recognizes only store-owned staging names and respects their age;
- same-key operations remain FIFO while different keys may run concurrently up
  to the configured bound;
- complete operations survive waiter or caller-runtime loss after acceptance;
- streaming save cancellation preserves the previous blob until the explicit
  input-complete marker is accepted;
- `close` drains accepted work and joins every store-owned worker and
  coordinator thread;
- all concurrent close callers wait for the same completed join and observe the
  same outcome;
- blocking-only builds do not resolve Tokio.

Use deterministic barriers, channels, child-process protocols, and private
test-only hooks. Do not use sleeps as the mechanism for reaching a failure or
interruption point. A timeout may only guard against deadlock after the test
has deterministically established the intended state.

## 1. Make Engine Failure Hooks Available to Windows Tests

Generalize the private test instrumentation currently restricted to Unix so it
can exercise executor-neutral engine failures on both Unix and Windows.

Keep all hooks private and compiled only for tests. They must not:

- change the public API;
- add branches to production builds;
- expose engine commands or platform backends;
- depend on Tokio inside the engine;
- alter operation ordering or error categories.

Run the following tests natively on Windows:

- a filesystem job panic is caught at the job boundary;
- the failed operation completes with the documented error;
- its active key and registry entry are released;
- the worker facility accepts later same-key work;
- worker startup failure releases admission and returns `WorkerUnavailable`;
- worker dispatch failure releases admission and returns
  `WorkerUnavailable`;
- maintenance panic or dispatch failure clears the maintenance barrier;
- coordinator failure transitions the shared lifecycle and rejects later work
  deterministically;
- worker-exit panic makes `close` return `ShutdownFailure`;
- concurrent close callers observe the same shutdown failure;
- a coordinator paused after sending close results prevents every close caller
  from returning until the coordinator exits;
- no failure leaves active keys, registry entries, coordinator threads, or
  worker threads behind.

Resource-exit assertions must use deterministic private notifications or join
state. Counting completion of the final filesystem operation is insufficient.

## 2. Add Native Windows Commit and Interruption Tests

Add Windows-specific test boundaries around native staging, rename, commit,
and directory-synchronization operations.

### Save and replace

Test both complete and streaming saves:

- failure before staging creation;
- failure during staging writes;
- failure immediately before replacement;
- successful replacement;
- failure after replacement but before successful directory synchronization.

For failures before replacement, assert that the previous canonical bytes
remain unchanged and no partial canonical blob is visible. For failures after
replacement, assert the documented committed-new-value ambiguity and verify
that the canonical blob is complete and valid.

### Process interruption

Use an isolated child test process and a deterministic parent/child protocol:

1. The child opens the store and signals when it reaches a named test stage.
2. The parent waits for that exact signal.
3. The parent terminates the child without allowing it to advance.
4. A fresh process opens or inspects the store.
5. The test validates the permitted canonical and staging states.

Cover interruption:

- before save replacement;
- after save replacement;
- before clear rename;
- after clear rename;
- after quarantine rename and before successful directory synchronization.

Save recovery may observe only a complete old or complete new canonical blob.
Clear recovery may observe only the old blob or absence. Quarantine recovery
must validate both the canonical and diagnostic paths according to the exact
commit stage reached.

Do not use a timer to guess when the child reached a filesystem operation.

### Quarantine ambiguity

Inject a failure after a successful quarantine rename but before successful
directory synchronization. Assert that:

- the returned error preserves its operation and I/O source;
- the canonical path is absent;
- the diagnostic destination exists and contains the complete prior blob;
- a later inspect, load, clear, or quarantine operation remains coherent.

## 3. Complete Owned-Staging Cleanup Coverage

Exercise cleanup through the native Windows backend with:

- a stale owned save-staging file that is removed;
- a recent owned save-staging file that is skipped;
- a stale owned clear-staging file that is removed;
- a recent owned clear-staging file that is skipped;
- an owned filename at the age boundary;
- malformed owned-looking names;
- valid hash and nonce shapes with the wrong suffix or format identity;
- unrelated files;
- an entry that fails metadata inspection;
- an entry that fails removal;
- multiple entries producing a mixed `CleanupReport`.

Verify exact `removed`, `skipped`, and `failures` classifications. Cleanup must
never remove unrelated or malformed names.

Tests involving age must set and inspect file timestamps explicitly. Do not
wait for files to age.

## 4. Exercise Windows Paths Through Real Filesystem Operations

Path tests must use the production backend rather than validating only the
wide-string conversion helper.

Exercise save, load, replace, quarantine, clear, and close with:

- a relative root resolved before the process current directory changes;
- Unicode root and namespace components;
- Unicode opaque keys;
- an absolute extended-length path longer than 260 UTF-16 code units;
- a path constructed with `OsString::from_wide` containing non-Unicode wide
  units where the hosted filesystem and Windows APIs permit it;
- an extended UNC-form conversion test even when CI cannot provide a real UNC
  share.

If the hosted filesystem rejects creation of a non-Unicode path independently
of the crate, the test must distinguish that environmental rejection from a
crate conversion failure. The production conversion must still be covered with
an exact unit test.

Every path case that can be created must perform actual blob-store operations
and validate canonical bytes.

## 5. Run the Full Native Facade Matrix

Use the shared conformance suite for both `BlockingAtomicBlobStore` and
`atomic_blob_store::tokio::AtomicBlobStore` on Windows.

Cover:

- empty, one-byte, one-chunk, one-chunk-plus-one, multi-chunk, maximum-sized,
  and over-limit payloads;
- complete and streaming save/load;
- early EOF before data, within a chunk, and at a chunk boundary;
- trailing input;
- source failure before and after a delivered chunk;
- destination failure before and after a written chunk;
- invalid envelopes producing no output;
- destinations not being flushed or shut down;
- cancellation before the first chunk and under backpressure;
- cancellation during the final EOF probe;
- cancellation immediately before and after input-complete acceptance;
- worker failure while a source read or EOF probe is pending;
- same-key FIFO ordering;
- different-key concurrency at the configured worker bound;
- maintenance and `flush` ordering;
- successful, failed, idempotent, and concurrent close;
- last-handle drainage for complete and streaming work;
- caller-runtime destruction before and after input-complete acceptance.

Where blocking I/O cannot represent async cancellation exactly, document and
test the corresponding source drop, endpoint disconnection, or injected I/O
failure.

## 6. Add Model-Based Operation Sequences

Add a deterministic model test that generates bounded sequences over a small
set of keys and compares the store with an in-memory reference model.

Include:

- save;
- streaming save;
- load;
- streaming load;
- inspect;
- clear;
- quarantine;
- flush;
- close as the terminal operation.

Generate sequence seeds explicitly and print the seed on failure. Keep the
default CI case count bounded so the test remains fast and reproducible.
Include a command-line or environment override for running more cases
manually.

The model must compare public results, canonical state, and lifecycle errors.
It must not reproduce implementation internals as its oracle.

## 7. Validate Release Builds and the Packaged Crate

Add a dedicated native Windows validation job that runs:

```bash
cargo test --locked --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --locked --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --all-features

cargo test --locked --release --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --all-features

cargo test --locked --manifest-path session-store-file/Cargo.toml \
  --workspace

cargo clippy --locked --manifest-path session-store-file/Cargo.toml \
  --workspace --all-targets --all-features -- -D warnings
```

Do not rely on workspace feature unification as the only blocking-only or
Tokio-enabled test. Keep the explicit feature runs.

Then:

1. Run `cargo package --locked --no-verify -p atomic-blob-store`.
2. Inspect the packaged file list.
3. Extract the generated package into a temporary directory.
4. Build and run a minimal external blocking consumer against the package.
5. Build and run a minimal external Tokio consumer against the package.
6. Exercise open, save, load, streaming, flush, and deterministic close.

The package smoke consumers must not use workspace path dependencies or
private APIs.

## 8. Configure Native Windows CI

Do not add `schedule` or `pull_request` triggers.

Run the Windows validation:

- on pushes to `main` that modify the file-store workspace, relevant client
  crates, benchmark runner support, or the Windows workflow itself;
- through `workflow_dispatch` so the full validation can be rerun before a
  release or after a runner-image change.

Use:

- `windows-2022` as the pinned reference environment;
- `windows-latest` as an additional manually dispatched compatibility
  environment.

The ordinary `main` push must run the pinned `windows-2022` job. The manual
workflow must run both images with `fail-fast: false`. Do not mark either image
experimental and do not use `continue-on-error`.

Keep deterministic functional tests in the required job. A separate repeated
test step may run the high-risk lifecycle and interruption test binaries
multiple times, but:

- repetition must remain bounded;
- failures must fail the job;
- the workflow must not automatically retry until success;
- test iteration and model seed must be visible in the log.

Suggested repeated cases are:

- concurrent close and shared join outcome;
- last-handle worker and coordinator termination;
- same-key cancellation and FIFO release;
- streaming cancellation around input-complete;
- child-process save, clear, and quarantine interruption.

Use `RUST_BACKTRACE=1`. Set explicit job and test timeouts so a deadlock fails
with useful diagnostics.

## 9. Retain Windows Validation Evidence

For each native Windows job, record:

- runner label and exact runner-image version;
- Windows edition, version, and build;
- CPU architecture and logical processor count;
- filesystem and volume information for the workspace and temporary directory;
- `rustc -Vv` and `cargo -V`;
- exact Cargo commands;
- build profile and enabled features;
- repeated-test iteration and model seeds;
- test results.

Upload a validation artifact containing the environment report and command
output. On failure, also upload surviving test directories, staging files, and
child-process logs when doing so does not expose credentials or unrelated
runner data.

Artifact collection must use `if: always()` so diagnostics survive a failed
test step. A green run is valid evidence only when every required command
completed successfully; artifact upload success must not mask test failure.

## Validation

Run locally where platform-independent:

```bash
cargo fmt --manifest-path session-store-file/Cargo.toml --all --check

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --no-default-features

cargo test --manifest-path session-store-file/Cargo.toml \
  -p atomic-blob-store --all-features

cargo test --manifest-path session-store-file/Cargo.toml --workspace

cargo clippy --manifest-path session-store-file/Cargo.toml \
  --workspace --all-targets --all-features -- -D warnings
```

Before treating Windows validation as complete, manually dispatch the workflow
and require both `windows-2022` and `windows-latest` jobs to pass without
ignored failures.

## Acceptance Criteria

The work is complete when:

- engine failure hooks and deterministic resource-exit assertions run natively
  on Windows without affecting production builds;
- Windows save, replace, clear, and quarantine interruption tests prove only
  their documented states;
- quarantine rename-success/synchronization-failure ambiguity is tested through
  the native backend;
- cleanup removal, skipping, malformed-name isolation, age handling, and mixed
  failures are covered;
- relative, Unicode, extended-length, and supported non-Unicode wide paths
  perform real blob-store operations;
- blocking and Tokio facades pass the shared streaming, ordering, cancellation,
  runtime-loss, and shutdown matrix on Windows;
- bounded model-based operation sequences agree with the public reference
  model and report reproducible seeds;
- debug and release-profile native Windows tests pass;
- blocking-only and all-feature configurations are tested explicitly;
- an external blocking consumer and external Tokio consumer run successfully
  from the packaged crate;
- `windows-2022` validation runs on relevant pushes to `main`;
- a manually dispatched run passes on both `windows-2022` and
  `windows-latest`;
- environment, command, result, and failure evidence is retained as workflow
  artifacts;
- no required Windows job or command uses `continue-on-error`, ignored
  failures, or pass-until-success retries.
