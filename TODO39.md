# C Wrapper Native File Session Store and Administration

## Goal

Allow C applications to select the existing Rust file-session-store adapter and
use its inspection, quarantine, clear, and stale-temporary cleanup operations.
Retain custom asynchronous store callbacks as an independent option.

## Current foundation and feasibility

The independent `session-store-file` workspace publishes
`rumqttc-session-store-file-next`. Its v4/v5 `SessionFileStore` APIs and
`FileStoreOptions` provide native stores and administrative operations.
Wrapper-core already has `rust_session_store::from_v4/from_v5` adapters that
translate native persisted sessions to its opaque checkpoint envelope.

This is feasible as an optional dependency and owned store handle. It must
reuse the adapter and its protocol formats instead of storing wrapper envelopes
directly into files that the Rust adapter expects to contain native checkpoints.

## Implementation requirements

- [ ] Add an opt-in file-store feature with both required protocol features and
  compatible package versions. Preserve the independent workspace boundary and
  published dependency graph; do not create a circular workspace dependency.
- [ ] Add opaque retained C store handles with asynchronous/tracked open and
  administration, or explicitly blocking alternatives outside driver callbacks.
  Supply the Tokio context required by native operations without creating an
  unowned temporary runtime or blocking the MQTT driver on filesystem work.
- [ ] Expose an existing trusted root, checkpoint-size bound, and concurrency
  limit using copied paths and versioned options. Define platform path encoding
  explicitly rather than assuming every native path is UTF-8.
- [ ] Construct native protocol stores and adapt them through
  `rust_session_store::from_v4/from_v5`. Reuse adapter identity for shared handles
  so wrapper key leases still exclude two clients using the same key. Audit
  separately opened handles referring to the same root and coordinate their
  active-key ownership; per-operation file coordination is not a client lease.
- [ ] Add configuration attachment with explicit protocol, stable scope, and
  native persistent-session prerequisites. Match load/save/clear failures to
  existing typed wrapper errors without silently clearing unreadable data.
- [ ] Expose native inspection state, size, modification time, checkpoint path,
  key/filename derivation, quarantine result, operator clear, and stale-temp
  cleanup reports through owned results and typed accessors.
- [ ] Define exclusivity between active clients and destructive administration.
  Reject or coordinate quarantine/clear on active keys; retain the adapter's
  before-use restriction for stale-temp cleanup.
- [ ] Preserve native atomic replacement, namespaces, limits, and durability
  guarantees. Do not add claims of cross-process locking, encryption, tamper
  resistance, or universal power-loss safety that the adapter does not provide.
- [ ] Document C/Rust interoperability using the same native store layout.
  Provide explicit handling for protocol/version mismatch and the distinct
  opaque-envelope format used by custom C store callbacks.
- [ ] Keep cancellation's potentially committed writes explicit. Store handles,
  administrative completions, and execution contexts outlive active work and
  release owners exactly once without detaching forgotten filesystem tasks.

## Verification and completion

- [ ] Persist mixed QoS/subscription state from C and recover it through Rust,
  then reverse the direction for both protocol namespaces.
- [ ] Cover limits, corrupt/version-mismatched files, interrupted replacement,
  quarantine/clear/cleanup reports, active-key exclusion, and owner teardown.
- [ ] Exercise optional-feature absence and installed C consumers with paths
  valid for each supported platform.
- [ ] Update C header/exports, packaging, README, a file-store example,
  `PARITY.md`, and root `CHANGELOG.md`. If adapter behavior changes, also update
  `session-store-file/CHANGELOG.md` and verify that workspace separately.

Complete when C can reuse the maintained file store and administrative tools
without reimplementing its persistence format or supplying Rust glue code.
