# C MQTT 5 Publish Recovery Status

Status: planned.

## Goal

Expose the existing publish-budget snapshot's `recovery_pending` flag to C so
applications can distinguish checkpoint restoration from ordinary budget use.

## Foundation and scope

Both the native MQTT 5 and wrapper-core `PublishBudgetSnapshot` types already
contain this flag. `rumqttc_client_v5_publish_budget_snapshot()` in
`native-wrappers/c/src/ffi.rs` returns usage and limits but drops it. This task
adds the missing observation; it does not change restoration or admission policy.

## Implementation requirements

- [ ] Add a separately named versioned publish snapshot API/record that returns
  count, retained bytes, limits, and recovery status from one existing coherent
  core snapshot. Preserve the existing function's signature and behavior under
  `docs/c-abi-compatibility.md`; do not bolt an argument onto an exported function.
- [ ] Preserve the native meaning of `recovery_pending`: audit initialization,
  checkpoint loading/reservation, failure, reconnect, and teardown in
  `rumqttc-v5/src/publish_budget.rs`. It is not equivalent to disconnected,
  reconnecting, or an explicit session-recovery operation being in progress.
- [ ] Keep acquisition independent of driver polling and application event
  consumption. Do not derive the flag from an older native diagnostics capture
  or inspect mutable restoration internals from C.
- [ ] Define unsupported-protocol behavior, optional outputs, record size checks,
  reserved fields, and initialized error outputs using current C conventions.
- [ ] Document that a snapshot does not reserve capacity or guarantee that the
  next admission succeeds. Counters remain charged retained data rather than RSS;
  zero usage while recovery is pending does not prove no checkpointed work exists.

## Verification and completion

- [ ] Use a barrier-controlled store load to observe pending restoration, then
  successful empty and nonempty restoration. Assert all snapshot fields come from
  the same core observation and the old API retains its established projection.
- [ ] Cover failed/oversized restoration, immediate close during load, reconnect,
  terminal clients, wrong protocol, short records, and initialized failure outputs.
- [ ] Exercise the accessor from native C without issuing a publish as a probe;
  verify that reading it does not advance or cancel store operations.
- [ ] Update C header/exports, publish admission documentation, `PARITY.md`, and
  `CHANGELOG.md`; run targeted core/C tests and ABI checks.

Complete when C can directly inspect the native coherent recovery flag alongside
publish usage and limits without affecting admission or restoration.
