# Optional C File-Backed Session Stores and Administration

Status: implemented and validated locally. Native Windows/macOS execution remains
pending CI. Both stages share the same ownership, admission, and joined-teardown
contracts. See
[`native-wrappers/file-session-stores.md`](native-wrappers/file-session-stores.md)
for the supported API contract and deployment limits.

## Goal

Give C applications an optional maintained durable-session store, followed by
safe administration of inactive keys. Applications should not need to implement
crash-consistent storage callbacks or manipulate checkpoint files themselves.

## Foundation and scope

`session-store-file/adapter` already provides v4/v5 `SessionFileStore`, inspection,
quarantine, operator clear, and stale-file cleanup. Wrapper-core's
`rust_session_store::{from_v4, from_v5}` adapts native stores to the managed
checkpoint contract. The optional C API now adds maintained file stores alongside application store
callbacks.

The implementation extends that foundation with shared client/administration
leases, redacted typed failures, bounded worker execution, and explicit engine
close plus context join. MQTT execution teardown and store teardown remain
separate boundaries.

Keep the independent file-store workspace and optional dependency boundary.
Reuse its on-disk format and atomic-blob-store engine. Keep application callback
stores working unchanged. A second checkpoint format, generic codec or editable
session-state API, cross-process locking, and changes to MQTT recovery semantics
are outside this task. Root and ancestors must be trusted; cross-process
filesystem exclusion, encryption, tamper resistance, and universal power-loss
durability are not added.

## Stage 1: durable attachment and ownership foundations

- [x] Add an opt-in C Cargo/package feature and capability with an optional
  dependency on `rumqttc-session-store-file-next`. Keep default artifacts and
  wrapper-core's mandatory dependencies unchanged. Enabled profiles must support
  both explicit protocols with their separate namespaces.
- [x] Add opaque retained store handles and owned open options for trusted root,
  checkpoint-size bounds, and resource limits. Specify platform path encoding,
  copy inputs, and return owned/copyable paths. Require an existing root; no
  implicit lossy conversion or automatic creation of untrusted directory trees.
- [x] Coordinate opens by root and protocol namespace before attachment. Reuse
  one native owner and one wrapped identity, or reject a duplicate; differing
  options must never create an independent owner that bypasses coordination.
  Define root normalization, supported alias detection, and open/close races.
  Retain coordination through pending initialization and final teardown.
- [x] Establish one atomic lease authority shared by client attachment,
  redirects, recovery, and future administration. Do not implement an activity
  query followed by an unprotected mutation. Preserve exclusion through accepted
  store work that outlives a driver timeout or shutdown; release only when it
  settles. Cover failed starts and redirect target-key retirement. Refactor the
  private wrapper lease mechanism where needed without weakening callback stores.
- [x] Define attachment with protocol, scope, timeout, and wrapper checkpoint
  bounds. Specify whether each limit counts native serialized bytes, the
  wrapper's additional eight-byte envelope, or on-disk framing. Reconcile store
  and per-attachment limits, including rejection before avoidable allocation.
  Preserve native serialization, key encoding, and Rust namespace compatibility;
  the wrapper envelope must not become a new on-disk format.
- [x] Reuse and extend `rust_session_store::{from_v4, from_v5}` rather than
  duplicate persistence logic. Define typed redacted errors at both the client
  and administration boundaries: absence, corruption, unsupported version,
  protocol mismatch, size limit, I/O, resource rejection, in-use, and closed
  state must have explicit mappings. Do not rely on the bridge's current coarse
  mappings or expose native error strings, checkpoint bodies, or credentials.
- [x] Keep filesystem execution in the existing engine. Bound wrapper admission,
  queued operations, retained bytes, and observations separately from the engine's
  active-operation limit. Account for initialization and per-store threads.
  Audit serialization CPU work and intermediate allocations; keep substantial
  encode/decode work off MQTT execution workers using bounded tracked execution
  where necessary. Measure representative checkpoint sizes before choosing limits.
- [x] Give accepted auxiliary work owned inputs and tracked completion independent
  of its observer or driver future. A wait deadline limits observation; dropping
  or cancelling an observer does not prove a mutation did not commit. Preserve
  same-key ordering until underlying work settles. Never block a callback or
  execution worker waiting for its own progress, including during destruction.
- [x] Extend the maintained Rust file adapter with explicit ordered close and
  quiescence observation using the engine's existing lifecycle boundary. If that
  boundary is insufficient for initialization or teardown, extend the dependency
  rather than infer thread exit from a completed operation. Reject C store close
  while live clients are attached; once accepted, reject new attachments and
  work, drain prior work, and observe joining all store-owned execution. Shared
  handles observe the same closure. Retained configurations cannot reopen it.
- [x] Distinguish handle release, operation completion, client teardown, and full
  store quiescence. Provide owned repeatable polling/timed observation for close;
  include failed/cancelled opens, auxiliary tasks, and thread teardown before
  reporting safe unload. Observer timeout or release must not cancel teardown.
- [x] Review the ownership/state transitions and exercise timeout, duplicate-open,
  and close races before freezing additive C declarations. Preserve ABI layouts,
  initialized outputs, disabled-feature responses, and existing callback behavior.

Stage 1 is usable only with both-protocol restart compatibility, bounded work,
lease protection, and unload-safe teardown established. Administration may follow
separately; none of these foundations may be deferred to Stage 2.

## Stage 2: administration through the same authority

- [x] Expose inspection as metadata, including absence, size, and optional
  modification time. Inspection does not decode or validate a session and does
  not guarantee a later operation sees unchanged data.
- [x] Expose quarantine and operator clear with typed owned results. Acquire
  exclusive same-key authority atomically against client admission and pending
  work; reject an in-use key. Hold authority through actual mutation settlement,
  including after observer timeout. Never clear a live session outside the
  managed recovery transaction or automatically delete a corrupt checkpoint.
- [x] Expose stale cleanup only within its existing global before-use barrier
  and supported platform scope: it targets store-owned Windows staging files,
  not generic Unix temporary files. Coordinate against all affected opens and
  work; do not silently turn unsupported cleanup into success.
- [x] Reuse Stage 1 operation admission, ownership, errors, and close observation.
  Document path-alias limits and the requirement for external exclusion from
  other processes and independently driven Rust store users. Rust/C cross-open
  compatibility is sequential interoperability, not concurrent-writer safety.

## Verification and completion

- [x] Use native C process-restart fixtures to recover subscriptions, outgoing
  QoS 1/2 and incoming QoS 2 for both protocols. Cross-open Rust/C checkpoints.
- [x] Verify atomic replacement interruption, wrong/corrupt/oversized checkpoints,
  envelope-limit boundaries, typed redacted failures, and absence handling.
- [x] Race duplicate opens with conflicting options, starts with administration,
  timeout/shutdown with key reuse, redirects with target-key administration, and
  close with new attachments. Prove no lease gap or late mutation crosses into a
  replacement client's ownership. Preserve explicit session recovery behavior.
- [x] Cover quarantine/clear, absent inspection, cleanup barriers and unsupported
  platforms, root/path encodings, and supported aliases. Verify shared handles
  and retained configurations cannot bypass limits, exclusion, or closure.
- [x] Test retained observations, timed waits followed by eventual mutation,
  queue/byte/operation limits, stalled filesystem work, and peer MQTT progress.
  Record serialization/allocation cost separately from network throughput.
- [x] Prove unload safety after successful and failed opens, timeout, shutdown,
  and close: operation completion alone must not count as joined auxiliary work.
  Run existing callback-store ownership, cancellation, and exclusion regressions.
- [x] Update feature/package consumers, C examples, READMEs, `PARITY.md`, and root
  `CHANGELOG.md`. Adapter changes also require `session-store-file/CHANGELOG.md`.
  Run relevant tests in both workspaces, package profiles, and C ABI checks.

Complete when an optional C artifact can reuse the Rust durable store and safely
administer inactive keys without reimplementing persistence or weakening leases.

## Established implementation decisions

- `file-session-store` is optional in wrapper-core and C. Disabled C artifacts
  retain initialized-output stubs and omit the capability bit.
- Duplicate independent opens return `InUse`, including conflicting options.
  Retained handles share the owner. Canonical roots use filesystem identity on
  Unix and Windows; external writers still require application exclusion.
- `atomic-blob-store` remains at 0.1.2. Its blocking facade runs on explicitly
  owned file workers; the maintained adapter exposes ordered close. No sibling
  dependency changes or alternate checkpoint format are introduced.
- Store limits count native serialized bytes. Attachment limits also count the
  eight-byte wrapper envelope, which is removed before persistence. Output and
  decoded storage have explicit bounds; reservations adapt to actual checkpoint
  size and remain separate from operation, observation, store and worker limits.
- Accepted mutations retain ownership until settlement after observer timeout.
  Explicit shutdown and joined context workers establish the unload boundary.
- Inspection, quarantine, clear and platform-scoped cleanup reuse that authority.
  Rust/C compatibility is sequential, never concurrent writer coordination.

## Validation record

Local validation on Linux:

- Wrapper-core/C follow-up tests pass with default features (515 passing tests,
  two ignored), minimal `file-session-store` (469 passing, two ignored), and the
  composed file-store/readiness/ordered-shutdown/tracing/Rustls/WebSocket profile
  (608 passing, three ignored). These totals include doctests.
- Both protocols have deterministic paused-time regressions for the full load
  deadline, including worker contention after loading. They verify timeout at the
  original deadline, retained key exclusion after timeout, and reuse after work
  settles. The MSVC restart target explicitly enables C11 atomics.
- The independent file-store workspace passes all tests (51), with separate
  protocol-free checks and v4-only/v5-only adapter tests (10 passing tests).
- Native C abrupt process-restart tests recover both protocols, the maintained
  store example runs, and the dynamically loaded library joins store execution
  before unloading. Seven existing native callback-store ownership, cancellation,
  atomic-write, recovery and race regressions also pass with file support enabled.
  The optional-error-output audit covers all 289 APIs.
- Native interoperability coverage cross-opens C checkpoints with the Rust file
  adapter for both protocols, verifies pending subscriptions, outgoing QoS 1/2
  and incoming QoS 2, then saves distinct payloads that C must recover on the wire.
  CI enables this test explicitly. The production source-boundary check now parses
  Rust syntax, excludes test-only modules and allows the maintained storage bridges.
  All four Linux native checks (restart, cross-open, unload and example) and all
  26 Python fixture tests pass. Default pedantic/nursery and optional/composed
  warnings-as-errors Clippy checks, package contents and formatting checks pass.
- C ABI/header/export checks and C/C++ consumers pass. Installed static/shared
  CMake and pkg-config consumers pass all seven checks in each of
  `file-store-minimal`, `file-store-core-only`, and `file-store-composed`.
- Optional/composed Clippy passes with warnings denied. Default wrapper Clippy
  also passes with pedantic/nursery lints. Native-client each-feature Clippy
  passes all 43 configurations.
- The unchanged atomic-blob-store 0.1.2 engine passes three save/streaming/clear
  failpoint tests and its subprocess interruption/commit-state test.
- The Windows GNU target cross-check passes. Native Windows/macOS restart,
  cleanup, example and unload execution is configured in CI; it has not been
  executed locally. Filesystem guarantees remain platform-specific.
- The six-row checkpoint harness records conversion, save/load latency and
  cumulative allocation cost independently of MQTT/network throughput.

Both the client and native-wrapper workspaces pass `cargo check --workspace`;
optional file-store documentation builds, all three workspaces pass formatting,
and `git diff --check` is clean. All 43 native-client each-feature test
configurations pass, checked across independent build directories. Run the
matrix and its separate lint pass from the repository root:

```sh
cargo hack --each-feature --exclude-all-features test \
  -p rumqttc-v4-next -p rumqttc-v5-next
cargo hack clippy --each-feature --exclude-all-features --no-dev-deps \
  -p rumqttc-v4-next -p rumqttc-v5-next -- -D warnings
```

Keep the lint and test matrix runs sequential: `--no-dev-deps` temporarily
changes manifests while running. The checkpoint cost harness and its raw evidence
are documented in
[`native-wrappers/wrapper-core/benches/file_store.md`](native-wrappers/wrapper-core/benches/file_store.md).
