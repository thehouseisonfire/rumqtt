# Changelog

## [Unreleased]

### Added

- Add Unix and Windows file-backed persistent session stores for MQTT v4 and v5,
  backed by a shared protocol-neutral, checksummed, bounded, cancellation-safe
  core using `atomic-write-file` 0.3.0 on Unix and native `windows-sys` 0.61
  wide-path commits on Windows. Add inspection, quarantine, exact legacy
  detection, operator clear, and cancellation-safe owned-staging cleanup APIs.
- Add reproducible persistent-session envelope, codec, durable file-store,
  coordination, checkpoint-growth, and MQTT QoS 1/QoS 2 benchmarks with
  machine-readable latency distributions and persistence-disabled baselines.

### Changed

- Develop and release the file-store core and protocol adapters from their own
  workspace while retaining the existing package names and client compatibility.
