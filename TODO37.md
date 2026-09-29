# C Wrapper Complete Structured Diagnostics

## Goal

Expose the useful native event-loop diagnostics through owned snapshots with
explicit optional fields, protocol differences, and snapshot freshness.

## Current foundation and feasibility

The Rust `EventLoopDiagnostics` models contain queue components, outbound state,
session-store lifecycle, batching configuration, and MQTT 5 redirect state.
Optional ordered-shutdown diagnostics add fence and deadline information.
The wrapper currently reduces this to a few counters and CONNACK diagnostics;
C exposes a fixed `rumqttc_diagnostics_t` plus selected redirect event accessors.

This is feasible as observation-only mapping. Retain the existing public record
layout under `docs/c-abi-compatibility.md`; add new handles or separately named
records rather than enlarging an existing struct in a compatible ABI line.

## Implementation requirements

- [ ] Inventory both native diagnostics structs and public outbound diagnostics.
  Classify each field as supported, optional/feature-specific, redundant, or
  intentionally unavailable with a reason in `PARITY.md`.
- [ ] Add a richer owned wrapper snapshot and C count/accessor or new-record
  interface. Keep the old diagnostics API behavior and field meanings intact.
- [ ] Preserve queue components separately: replay, scheduler queue, request
  channel, control channel, and immediate shutdown. Document sums to prevent
  double counting `pending_len` and its component fields.
- [ ] Expose connected/disconnecting/complete status, outbound quota and pending
  operations, store configured/loaded/clear-pending state, identity agreement,
  and raw/effective CONNACK session semantics.
- [ ] Expose configured/effective batching and MQTT 5 redirect lifecycle,
  attempts, target/candidate state, and optional SRV owner. Distinguish current
  snapshots from retained historical redirect events.
- [ ] Add ordered phase, fence sequence, remaining monotonic deadline, and
  native covered-queue observations when [TODO30.md](TODO30.md) is enabled.
  Never expose Rust `Instant` layouts or equate queue counts with completion.
- [ ] Include configuration revision and retry status when
  [TODO35.md](TODO35.md) and [TODO34.md](TODO34.md) land.
- [ ] Define capture generation, freshness, and consistency. If active native
  polling only permits cached snapshots, label them cached; do not claim atomic
  instantaneous agreement with concurrently changing producer queues.
- [ ] Preserve optional absence instead of inventing zero values for disabled
  features or the other protocol. Copy variable data and avoid secrets.
- [ ] Ensure diagnostic admission and delivery stay bounded and responsive
  during network waits, full application event queues, and close.

## Verification and completion

- [ ] Compare snapshots with native diagnostics at controlled lifecycle points,
  including queued/inflight work, pending store clear, and adaptive batching.
- [ ] Verify v4/v5 absence rules, retained data after client destruction,
  initialized failed-accessor outputs, and feature-disabled behavior.
- [ ] Prove repeated diagnostics cannot starve MQTT or bypass event backpressure.
- [ ] Update C header/exports, README, diagnostics example, `PARITY.md`, and
  root `CHANGELOG.md` with field definitions and freshness guarantees.

Complete when documented native diagnostics can be inspected from C without
access to mutable protocol state or dependence on internal Rust types.
