# Changelog

## [Unreleased]

- Split configuration, errors, format, paths, filesystem backends, engine
  events, lifecycle, operations, scheduling, streams, and workers into private
  responsibility-focused modules without changing the public API or V1 bytes.
- Make deterministic close join the coordinator as well as all started workers,
  and preserve a shared `ShutdownFailure` outcome for concurrent close callers.
- Catch injected filesystem-job and maintenance panics at job boundaries,
  release admission after worker startup or dispatch failure, and transition
  coordinator loss to a deterministic terminal engine failure.
- Add facade conformance, streaming-boundary, infrastructure-failure,
  blocking-only dependency, and independent-consumer coverage.

## [0.1.0] - 2026-07-25

- Replace the private Tokio runtime with one executor-neutral coordinator and a
  lazily started, bounded, store-owned worker pool.
- Add `BlockingAtomicBlobStore`, move the async facade to the explicit optional
  `tokio::AtomicBlobStore` module, and make the default dependency graph
  Tokio-free.
- Add ordered `flush` and deterministic, idempotent `close`, distinct lifecycle
  errors, and best-effort last-handle drainage.
- Keep runtime-neutral facade and streaming transport types available on
  unsupported targets so those builds compile and return `UnsupportedPlatform`
  when opened.
- Add genuine bounded-memory `AsyncRead`/`AsyncWrite` streaming saves and
  validation-before-output streaming loads without changing the version-1
  envelope or atomic replacement guarantees.
- Retain complete-blob `Vec` methods as allocation-heavy conveniences and
  document streaming submission, backpressure, cancellation, and two-pass
  load behavior.
- Return coordinator storage failures promptly even when a streaming save
  source read or final EOF probe remains pending.

## [0.1.0-alpha.1] - 2026-07-23

- Extract the bounded atomic blob snapshot abstraction with configurable
  identity, bounded coordination, stable format documentation, and neutral API.
