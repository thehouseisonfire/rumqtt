# Changelog

## [Unreleased]

### Added

- Depend on independently maintained `atomic-blob-store` 0.1.2.
- Add `rumqttc-session-store-file-next`, whose independent additive `v4` and
  `v5` features support either or both clients while retaining their existing
  on-disk namespaces, key encodings, and checkpoint envelopes.
- Add Unix and Windows file-backed persistent session stores for MQTT v4 and v5,
  backed by a shared protocol-neutral, checksummed, bounded, cancellation-safe
  core using `atomic-write-file` 0.3.0 on Unix and native `windows-sys` 0.61
  wide-path commits on Windows. Add inspection, quarantine, operator clear, and
  cancellation-safe owned-staging cleanup APIs.
- Add reproducible persistent-session envelope, codec, durable file-store,
  coordination, checkpoint-growth, and MQTT QoS 1/QoS 2 benchmarks with
  machine-readable latency distributions and persistence-disabled baselines.

### Changed

- Re-license as MIT OR Apache-2.0 with separate `LICENSE-MIT` and
  `LICENSE-APACHE` files.
- Update MQTT 3.1.1 file checkpoints to the rumqttc v4 session format version 2,
  which removes connection-local packet-allocation and acknowledgement-frontier
  counters. Existing v1 v4 checkpoint files are rejected and must be cleared.
- Update MQTT 5 file checkpoints to the rumqttc v5 session format version 2,
  which removes connection-local allocator and acknowledgement-frontier state.
  Existing v1 v5 checkpoint files are rejected and must be cleared.
- Remove the obsolete "active Tokio runtime required" adapter error.
- Preserve the existing `RUMQSESS` envelope and `.session` paths through the
  generic store's explicit format identity.
- Consolidate the previously separate, unpublished v4 and v5 adapter packages
  into the shared feature-gated adapter package and update examples, benchmarks,
  CI, and release tooling accordingly.
- Develop and release the file-store core and protocol adapters from their own
  workspace while retaining the existing package names and client compatibility.
- Move the protocol-neutral blob store and its owned validation and benchmarks
  to its standalone repository.
