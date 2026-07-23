# Changelog

## [Unreleased]

### Added

- Depend on the independently versioned `atomic-blob-store` pre-release crate.
- Add `rumqttc-session-store-file-next`, whose independent additive `v4` and
  `v5` features support either or both clients while retaining their existing
  on-disk namespaces, key encodings, and checkpoint envelopes.
- Add Unix and Windows file-backed persistent session stores for MQTT v4 and v5,
  backed by a shared protocol-neutral, checksummed, bounded, cancellation-safe
  core using `atomic-write-file` 0.3.0 on Unix and native `windows-sys` 0.61
  wide-path commits on Windows. Add inspection, quarantine, exact legacy
  detection, operator clear, and cancellation-safe owned-staging cleanup APIs.
- Add reproducible persistent-session envelope, codec, durable file-store,
  coordination, checkpoint-growth, and MQTT QoS 1/QoS 2 benchmarks with
  machine-readable latency distributions and persistence-disabled baselines.

### Changed

- Run exact legacy metadata probes on Tokio's blocking pool and only after the
  canonical checkpoint is confirmed absent.
- Move exact former-example filename detection into the MQTT adapter while
  retaining canonical-path precedence and the trusted-root boundary.
- Preserve the existing `RUMQSESS` envelope and `.session` paths through the
  generic store's explicit format identity.
- Consolidate the previously separate, unpublished v4 and v5 adapter packages
  into the shared feature-gated adapter package and update examples, benchmarks,
  CI, and release tooling accordingly.
- Develop and release the file-store core and protocol adapters from their own
  workspace while retaining the existing package names and client compatibility.
