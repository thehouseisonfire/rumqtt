# C MQTT 5 Publish Recovery Status

Status: implemented and validated on Linux.

## Goal

Expose the existing publish-budget snapshot's `recovery_pending` flag to C so
applications can distinguish a checkpoint-recovery admission gate from ordinary
budget use, including zero usage while checkpoint loading is unresolved.

## Foundation and scope

Both the native MQTT 5 and wrapper-core `PublishBudgetSnapshot` types already
contain this flag. `rumqttc_client_v5_publish_budget_snapshot()` in
`native-wrappers/c/src/ffi.rs` returns usage and limits but drops it. This task
adds the missing observation; it does not change restoration or admission policy.
Keep this to one additive snapshot API/record. No new observer handles,
notifications, recovery controls, or changes to deferred admission are needed.
Existing publish errors and deferred admission remain the mechanisms for reporting
an actual attempt's rejection and waiting for admission, respectively.

## Implementation requirements

- [x] Add a separately named versioned publish snapshot API/record that returns
  count, retained bytes, limits, and recovery status from one existing coherent
  core snapshot. Preserve the existing function's signature and behavior under
  `docs/c-abi-compatibility.md`; do not bolt an argument onto an exported function.
- [x] Preserve the native meaning of `recovery_pending`: audit initialization,
  checkpoint loading/reservation, failure, reconnect, and teardown in
  `rumqttc-v5/src/publish_budget.rs`, `publish_admission.rs`, and `eventloop.rs`.
  True means the checkpoint-recovery admission gate has not cleared; it does not
  prove restoration is actively progressing or will succeed. Failed or cancelled
  loads can leave it true, including on a terminal client. Ordinary reconnects
  reusing already-loaded state do not rearm it. It is not equivalent to
  disconnected, reconnecting, or an explicit session-recovery operation being in
  progress. Applications must inspect lifecycle/errors separately.
- [x] Document that false means only this gate is clear. Negotiated capabilities,
  request-channel capacity, count/byte limits, and shutdown can still prevent
  admission. Do not present this flag as a general publish-ready predicate.
- [x] Keep acquisition independent of driver polling and application event
  consumption. Do not derive the flag from an older native diagnostics capture
  or inspect mutable restoration internals from C.
- [x] Define unsupported-protocol behavior, optional outputs, record size checks,
  reserved fields, and initialized error outputs using current C conventions.
- [x] Document that a snapshot does not reserve capacity or guarantee that the
  next admission succeeds. Counters remain charged retained data rather than RSS;
  zero usage while recovery is pending does not prove no checkpointed work exists.
- [x] Preserve intermediate native observations: replay reservations are charged
  before the gate clears, so nonzero usage with `recovery_pending=true` is valid.
  Coherence covers one budget accounting observation, not an atomic view of the
  entire restoration transaction or client lifecycle.

## Verification and completion

- [x] Use a barrier-controlled store load to observe pending restoration, then
  successful empty and nonempty restoration. Verify counters, limits, and flag
  are projected from one core snapshot, allowing intermediate charged usage while
  the gate remains set. Verify the old API retains its established projection
  with state held stable; do not require equality between separate reads while
  concurrent progress can change accounting.
- [x] Cover failed/oversized restoration, immediate close during load, reconnect,
  terminal clients, wrong protocol, short records, and initialized failure outputs.
  Assert retained gate state after failure/termination and that ordinary reconnect
  with already-loaded state does not rearm it. Use existing native/core lifecycle
  coverage where sufficient; focus new tests on the C projection and boundary
  contract rather than duplicating restoration suites.
- [x] Exercise the accessor from native C without issuing a publish as a probe;
  verify that reading it does not advance or cancel store operations.
- [x] Update C header/exports, publish admission documentation, `PARITY.md`, and
  `CHANGELOG.md`; run targeted core/C tests and ABI checks.

Complete when C can directly inspect the native coherent recovery flag alongside
publish usage and limits without affecting admission or restoration.


## Implementation and validation evidence

The additive `rumqttc_client_v5_publish_budget_snapshot_v2` accessor writes a
caller-owned `rumqttc_v5_publish_budget_snapshot_v2_t` from exactly one existing
core snapshot. Its size prefix, initialized failure fields, output-only reserved
bytes, and untouched extension bytes follow the C record conventions. The old
scalar accessor is unchanged. No native restoration/admission behavior, driver
commands, handles, notifications, or feature gates were added.

The native audit confirms initialization from the configured store/clean-start
policy, rearming before fresh checkpoint loads and target-session transitions,
clearing after successful restoration, and retention after failed/cancelled
loading. Native teardown closes producer admission without clearing this flag.
Replay reservation and gate clearance remain distinct accounting updates.

Five C adapter tests reuse the existing controlled transport harness and add
store barriers for empty/nonempty restoration, budget failure with checkpoint
preservation, immediate close, ordinary reconnect, and terminal observations.
Both dedicated and shared execution are covered. Stable barriers allow exact
comparison with core and the old scalar projection. Record tests cover NULL and
wrong-protocol arguments, short and extended records, canaries, reserved bytes,
and initialized errors. The standalone `native_publish_budget_snapshot` C test
holds store/connector callback tokens and observes the gate without publishing
or consuming events; repeated reads neither advance nor cancel loading.

Validation on Linux:

- Full core/C suites passed with `cargo test --locked --manifest-path
  native-wrappers/Cargo.toml -p rumqttc-c-next -p rumqttc-wrapper-core-next`,
  both with defaults and with `--no-default-features`. Existing ignored tests
  remain ignored. Final targeted `publish_budget_snapshot` tests also passed in
  both profiles.
- Native `rumqttc-native-publish-budget-snapshot` and
  `rumqttc-native-publish-admission-offline` passed against default and minimal
  builds. Native integration (including optional-error-output calls), broker
  publish-admission, and the updated offline-publishing example passed with
  defaults. Optional-error-output coverage passed for all 264 APIs.
- `native-wrappers/c/tests/abi/check.sh all` passed current generated/header,
  export, C11/C++17 consumer, and relocated static pkg-config checks.
- Strict C-crate Clippy passed with `--all-targets --no-deps -- -D warnings
  -W clippy::pedantic -W clippy::nursery`. The combined core/C invocation is
  blocked by the pre-existing `clippy::missing_const_for_fn` warning at
  `wrapper-core/src/runtime.rs:247` (`mark_terminal_seen`); that unrelated
  implementation was not changed. Wrapper formatting and `git diff --check`
  passed.

macOS/Windows execution remains in the existing CI platform matrix; no local
cross-platform execution or historical ABI comparison is claimed.
