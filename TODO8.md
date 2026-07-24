# TODO: Extract a General-Purpose Atomic Blob Store Core

## Goal and Scope

Turn the current protocol-neutral-in-types but session-specific file-store core into a deliberately
general crate for bounded, crash-consistent, keyed blob snapshots on trusted local filesystems.

The extracted crate is not a database, queue, object store, or multi-process coordination service.
Its supported abstraction is:

> Within one process, atomically save, load, inspect, quarantine, and clear one complete bounded blob
> per opaque key, with same-key operation ordering and accidental-corruption detection.

Do not publish the extracted core as an independent product until every requirement and release gate
below is satisfied. Until then, keep it private or clearly pre-release.

## 1. Neutral Package, Modules, and Type Names

Choose a package name that describes the abstraction without referring to rumqtt, MQTT, sessions, or
checkpoints. Before reserving or publishing a crates.io name, verify availability and avoid claiming a
name in documentation that the project cannot publish.

The primary API should use neutral vocabulary. A representative shape is:

```rust
pub struct AtomicBlobStore { /* ... */ }
pub struct AtomicBlobStoreOptions { /* ... */ }
pub enum AtomicBlobStoreError { /* ... */ }
pub struct BlobInspection { /* ... */ }
pub enum BlobState { Absent, Present }
pub struct QuarantineInfo { /* ... */ }
```

Rename internal and public concepts consistently:

- `FileStore` -> `AtomicBlobStore`;
- `FileStoreOptions` -> `AtomicBlobStoreOptions`;
- `FileStoreError` -> `AtomicBlobStoreError`;
- `CheckpointInspection` -> `BlobInspection`;
- `CheckpointState` -> `BlobState`;
- `checkpoint_filename` -> a neutral blob/object filename helper, if it remains public;
- coordinator thread names, operation variants, comments, and diagnostics must use blob/store
  terminology rather than session terminology.

Do not retain MQTT-branded aliases in the generic crate. Compatibility aliases, if required, belong
in the rumqtt adapter crate and should be deprecated there.

## 2. Explicit Format Identity Configuration

Replace the hard-coded `RUMQSESS` envelope magic and `.session` suffix with immutable configuration
provided when a store is opened.

Define a validated format identity containing at least:

- an envelope magic or domain tag;
- a filename suffix;
- the envelope format version expected by the store;
- optionally a human-readable format name used only in diagnostics.

Requirements:

- The magic/domain value must have a documented length bound and must not be empty.
- The filename suffix must be one safe filename suffix, not a path. Reject separators, parent/current
  directory components, NULs where relevant, and platform-specific invalid representations.
- Configuration must be owned by the store and immutable after opening.
- The domain identity must participate in decoding validation so two applications sharing a root
  cannot silently accept each other's blobs.
- Decide whether the domain identity also participates in key hashing. Document the decision and add
  collision-domain tests.
- Filename derivation must remain deterministic for a fixed configuration and key.
- Configuration errors must be distinguishable from I/O and corrupt-data errors.

Avoid arbitrary unbounded magic bytes that make parsing or allocation unsafe. A fixed-size domain tag
is simplest; a bounded length-prefixed tag is acceptable if its complete wire encoding is specified.

The rumqtt adapter must supply values that preserve the existing `RUMQSESS` envelope and `.session`
paths unless a separately documented migration is intentionally introduced. Generalization alone is
not permission to rewrite existing session files.

## 3. Remove Legacy-Session Policy from the Base Store

Delete these concepts from the generic public API and implementation:

- `LegacyLoad`;
- `load_with_legacy_filename`;
- `inspect_with_legacy_filename`;
- `LegacyDetected` state;
- `InvalidLegacyFilename` errors;
- any knowledge of the former rumqtt example filename format.

The base store should report only canonical blob state. Legacy discovery and migration are application
policies and belong in the MQTT adapter.

If the adapter still needs legacy detection, implement it outside the generic core with a narrowly
scoped, security-reviewed helper. It must not bypass the root trust model, scan arbitrary directory
contents, or weaken canonical-path precedence. Document when that compatibility code can be removed.

Do not add a generic “load arbitrary filename” escape hatch merely to preserve the old behavior; that
would undermine the opaque-key/path-safety abstraction.

## 4. Neutral Errors and Documentation

Audit every public and internal string, identifier, rustdoc paragraph, README section, thread name,
test name, feature name, and package metadata entry. The generic crate must contain no `rumqtt`,
`MQTT`, `session`, or protocol-specific terminology except in an explicitly labeled migration note
that is not part of its lasting API documentation.

The error taxonomy must distinguish:

- invalid configuration;
- unsupported platform;
- root/namespace validation failures;
- ordinary I/O operations with operation context;
- atomic commit ambiguity;
- corrupt or unsupported envelopes;
- size-limit failures;
- coordination shutdown/failure;
- quarantine outcomes, including rename-success/sync-failure ambiguity;
- maintenance failures.

Use `#[non_exhaustive]` deliberately for public error enums while the crate is evolving. Errors must
not expose MQTT assumptions, and callers must be able to determine whether retry, quarantine,
reconfiguration, or operator intervention is appropriate without parsing display strings.

Write standalone crate-level documentation that explains:

- the exact abstraction and non-goals;
- trust and attacker model;
- cancellation and operation-ordering semantics;
- process-interruption versus hardware-power-loss guarantees;
- supported platforms and filesystem assumptions;
- lack of cross-process locking, authentication, encryption, CAS, and transactions;
- memory and blob-size behavior;
- commit-error ambiguity;
- temporary-file cleanup behavior on each platform.

## 5. On-Disk Format Stability Policy

Create a versioned format specification in the generic crate's repository documentation. It must
define, byte for byte:

- magic/domain encoding;
- envelope version encoding and byte order;
- declared payload length and byte order;
- payload bytes;
- checksum algorithm and coverage;
- trailing-data policy;
- filename derivation, hash algorithm, hexadecimal casing, and suffix handling;
- namespace rules;
- temporary and quarantine naming rules where those names are project-owned.

Adopt and document this compatibility policy before a stable release:

1. A released reader accepts every format version explicitly listed as supported.
2. A writer emits one documented current version; it never upgrades stored blobs merely by opening
   or reading them.
3. An unsupported future version fails closed without modifying or quarantining data automatically.
4. Corrupt data is never silently treated as absence.
5. Format-breaking changes require a new envelope version and an explicit migration API or external
   migration tool.
6. Changes to hash, suffix, namespace, magic/domain, or canonical key bytes are path migrations, not
   ordinary implementation changes.
7. Semver policy must state whether adding read support for a new version is minor-compatible and
   whether changing the emitted version requires an opt-in or major release.

Check in golden fixtures for every supported format version. Fixtures must be generated once and then
treated as immutable compatibility inputs, not regenerated automatically by the implementation under
test. Include valid, truncated, oversized, checksum-invalid, trailing-data, wrong-domain, and
unsupported-version fixtures.

## 6. Decide and Enforce the Bounded-Snapshot I/O Contract

Make an explicit decision before stabilizing the API.

### Preferred initial contract

Keep the implementation a bounded complete-snapshot store and say so in the crate name, rustdoc,
README, options, and benchmarks. Rename `max_checkpoint_size` to `max_blob_size`. Document that:

- save owns or buffers one complete payload;
- load returns one complete allocation;
- replacement rewrites the complete envelope;
- memory and I/O costs are linear in blob size;
- the store is unsuitable for unbounded objects, append-only logs, and incremental updates.

This is acceptable if the default maximum and its rationale are documented and all allocation paths
are checked before allocation.

### Streaming evaluation

Before final API stabilization, prototype or formally reject streaming interfaces. Evaluate:

```rust
async fn save_from<R>(&self, key: &[u8], reader: R, declared_len: u64) -> Result<()>
async fn load_into<W>(&self, key: &[u8], writer: W) -> Result<BlobMetadata>
```

The design must account for checksum calculation, length verification, trailing input, atomic commit,
blocking filesystem isolation, async versus blocking traits, cancellation after submission, and the
fact that an arbitrary borrowed reader cannot simply be moved into a `'static` coordinator job.

Do not add nominally streaming methods that still collect the entire blob internally. If a sound,
portable API would significantly complicate ownership or executor neutrality, record the rejection
and retain the explicit bounded-snapshot contract.

## 7. Configurable Coordination and Runtime Behavior

The current design creates a coordinator thread and a private Tokio runtime per store. Make this a
documented policy rather than a hidden cost, then evaluate configurable modes.

At minimum, consider:

- the existing owned coordinator/runtime mode;
- use of a caller-provided Tokio `Handle` or executor abstraction;
- a synchronous/blocking store API from which an async adapter can be built;
- limits on concurrently active different-key operations;
- coordinator thread name and shutdown behavior;
- whether independent stores rooted at the same namespace coordinate with one another;
- deterministic shutdown or `close`/`flush` semantics;
- behavior when the caller drops all store handles while work is queued.

Requirements for any selected modes:

- same-key FIFO and cancellation semantics must not vary silently by mode;
- blocking filesystem work must not run on async worker threads by accident;
- concurrency must be bounded or its unbounded behavior explicitly justified;
- runtime shutdown must not turn an acknowledged commit into lost work;
- options must be validated at construction;
- `Debug` output must describe the mode without leaking sensitive keys or paths unnecessarily.

Prefer one well-specified default over several lightly tested modes. If caller-provided runtime support
cannot preserve the existing construction-runtime independence, expose that difference in the type or
method contract rather than a footnote.

## 8. Non-MQTT Tests and Examples

Rewrite core tests so their fixtures and terminology are generic. Required examples include:

- saving and restoring an application configuration snapshot;
- maintaining independent keyed worker-state snapshots;
- inspecting and quarantining an intentionally corrupted blob;
- using two domain identities under a shared trusted root without cross-reading data.

Required tests include:

- opaque binary and non-UTF-8 key bytes;
- empty, small, maximum-sized, and over-limit blobs;
- deterministic filenames for fixed domain/key/suffix inputs;
- invalid domain, namespace, and suffix rejection;
- wrong-domain and wrong-version failures;
- replacement returns old-or-new complete data under interruption tests;
- clear returns old-or-absent complete state under interruption tests;
- same-key FIFO, different-key concurrency, dropped-future behavior, and coordinator shutdown;
- corruption, truncation, trailing bytes, and bounded-allocation behavior;
- quarantine and maintenance behavior on Unix and Windows;
- no cross-process guarantee, documented through a compile/run example or an explicit test limitation;
- golden fixture compatibility independent of the MQTT adapters.

Retain platform-specific tests for actual platform implementations. Do not claim Windows correctness
solely from Unix tests or vice versa. Run documentation tests with no rumqtt crates in dev-dependencies
so the generic API cannot accidentally rely on MQTT types.

## Migration Architecture

Perform the extraction in stages:

1. Freeze current session-store golden fixtures and observable behavior.
2. Introduce neutral internal names while keeping the current adapter tests passing.
3. Define the new format-identity and options types, initially configured by the MQTT adapter to
   reproduce existing bytes and paths.
4. Move legacy detection entirely into the adapter.
5. Split the generic crate/package only after its public API and documentation contain no adapter
   policy.
6. Add non-MQTT examples, fixtures, and platform tests.
7. Point the merged adapter from `TODO7.md` at the extracted crate and rerun session compatibility
   tests.
8. Publish an alpha/pre-release and solicit use outside MQTT before declaring a stable format/API.

If the generic crate is published separately, its release cadence and changelog must be independent.
The MQTT adapter should depend on a compatible version range and must not re-export the entire generic
API indiscriminately; re-export only types that are intentionally part of the adapter's contract.

## Validation Commands

At minimum, run:

```bash
cargo fmt --manifest-path session-store-file/Cargo.toml --all --check
cargo test --manifest-path session-store-file/Cargo.toml --workspace
cargo clippy --manifest-path session-store-file/Cargo.toml --workspace --all-targets --all-features
cargo doc --manifest-path session-store-file/Cargo.toml --workspace --all-features --no-deps
```

Add targeted builds for every coordination mode and supported platform configuration. Run the existing
persistence benchmarks before and after extraction, recording envelope throughput, save/load latency,
allocation behavior, and concurrent-key scaling. A neutral API is not sufficient justification for a
material performance regression.

Use a repository-wide text audit as a release gate for the generic crate:

```bash
rg -n -i 'rumqtt|mqtt|session|checkpoint' path/to/generic-crate
```

Every remaining match must be removed or explicitly justified as historical migration documentation.

## Completion Criteria

The core is ready to publish independently only when:

- its package and complete public API use neutral names;
- format magic/domain and filename suffix are safely configurable and validated;
- no legacy-session behavior remains in the base abstraction;
- errors, diagnostics, docs, examples, and tests are independent of MQTT;
- the on-disk format and semver compatibility policy are written and backed by immutable fixtures;
- the crate either provides genuine streaming I/O or explicitly and consistently presents itself as
  a bounded complete-snapshot store;
- coordinator/runtime ownership, concurrency bounds, cancellation, and shutdown are documented and
  tested;
- examples demonstrate useful non-MQTT applications;
- the rumqtt adapters reproduce their pre-extraction canonical paths and stored bytes;
- Unix and Windows platform behavior has appropriate evidence;
- the crate's independent maintenance and release cost is accepted as an intentional product
  commitment.

