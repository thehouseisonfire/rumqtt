# Owned Immutable C Publish Buffers

Status: implemented and validated on Linux. The ownership/performance gate passes.
Native Windows/macOS execution is configured in the existing CI matrix and has
not been run locally.

## Goal

Allow C applications to publish immutable application-owned storage without an
obligatory FFI payload copy, with explicit final release and unchanged MQTT
admission/completion behavior. Target large payloads and repeated use of the same
storage; existing copying calls remain the simple default.

## Foundation and scope

Rust publish APIs accept owned `Bytes`/`Vec` payloads. Wrapper-core's
`PublishCommand` already owns `Bytes` and passes them into native publish APIs;
C's `publish_command()` introduces an unconditional `Bytes::copy_from_slice()`.
The workspace's `bytes` dependency supports `Bytes::from_owner`. Reuse these
paths instead of introducing a second native publication or scheduling mechanism.

This is an ownership/performance feature, not an end-to-end zero-copy guarantee
or an allocation-free API. Owner/handle bookkeeping can allocate and reference
counting has a cost. Both codecs copy payloads into encoding buffers; checkpoint
construction also copies payloads. TLS, WebSocket framing and the kernel have
their own costs. Small single-use messages may not benefit.

Keep the first public implementation limited to a whole immutable payload buffer,
retain/release, and immediate/tracked/deferred publish variants. Topics and MQTT 5
properties retain their current copying and validation behavior. UTF-8 proof
caching, buffer slicing, mutable buffer builders, prepared topics, raw packet
mutation, packet-ID control, new memory-budget policies and validation-disable
switches are outside this task. A later UTF-8 cache requires separate measurements
and a library-checked proof bound to the exact immutable bytes; a caller flag is
never sufficient.

## Stage 0: feasibility and measurement gate

- [x] Build a private C/native-path proof using an owned `Bytes` payload with a
  release callback. Prove shared payload pointers through native admission,
  cloning and replay, and exactly-once final release. Include failed admission,
  deferred cancellation and teardown before committing public declarations.
- [x] Audit where the final owner can be dropped, including error paths and
  wrapper/native locks. Establish callback thread safety, permitted reentrancy,
  and enforcement of the existing host-callback blocking restrictions. Do not
  defer this audit until after the API or ownership model is fixed.
- [x] Compare copying and owned-buffer calls in release builds for both protocols,
  using representative sizes such as 64 B, 1 KiB, 16 KiB, 256 KiB and 1 MiB.
  Include single-use and reused storage, successful/rejected admission and
  deferred waits. Separate steady-state publish cost from buffer creation and
  final release; also report the complete create/publish/release lifecycle.
  Count application input preparation consistently, including any copy needed
  to produce immutable storage. Report allocations, allocated bytes, copy cost,
  CPU/latency and peak retained backing storage independently of network throughput.
- [x] Record environment, workload, raw repeated samples and an acceptance
  criterion chosen before evaluating results. Proceed only with a repeatable,
  practically useful benefit for a documented large-payload or reuse workload,
  acceptable retention and callback costs, and no material regression to existing
  copying calls. Pointer sharing alone is insufficient. If the gate fails, record
  the evidence and park the public feature without exporting a speculative ABI.

## Stage 1: ownership and API contract

Stage 1 starts only after Stage 0 establishes both sound ownership and useful
measured benefit. Preserve existing ABI layouts, admission semantics,
error/delivery classification and ordinary publish completion behavior through
additive C APIs using [the C ABI policy](docs/c-abi-compatibility.md).

- [x] Add an opaque immutable buffer handle with retain/release and transactional
  creation from application-owned bytes, a required release callback and opaque
  userdata (which may be NULL).
  Success transfers release responsibility for that storage lifetime and returns
  one owned handle; failure returns no handle, never invokes the callback, and
  leaves responsibility with the caller. Initialize failure outputs consistently.
  The caller may read but may neither mutate nor free the bytes until final release.
  Re-adopting the same storage must not create competing release responsibilities.
- [x] Accept NULL data only for zero length; accept non-NULL zero-length storage
  with the same ownership and exactly-once callback rules. Require only byte
  alignment for payload storage, enforce addressable length bounds, and document
  the caller's obligation to supply one valid stable region. Publish admission
  uses the existing MQTT packet/configured limits at their established admission
  or processing boundaries; creation does not prove publishability. An empty payload
  must not lose its owner through conversion to an unowned empty `Bytes`.
- [x] Back payloads with a sound Rust owner using `Bytes::from_owner`. The owner
  retains the pointer, length and callback context through every payload clone;
  only its final destruction invokes the callback. Do not manufacture a static
  borrow, use Rust allocation reconstruction for C memory, or copy payloads as a
  hidden fallback. Audit Send/Sync, shared reads and callback/context thread safety.
- [x] Publish APIs borrow a live buffer handle and retain storage for each
  accepted use. A failed attempt leaves the caller's handle reusable. Each
  successful publication is a distinct MQTT operation, even when storage is
  shared; retry/arbitration must never submit one attempt twice. Retain/release
  supply independent references, not duplicate adoption of the application owner.
  A release must not race access through that same handle reference; independently
  retained references may be used/released concurrently.
- [x] Integrate [TODO41.md](TODO41.md)'s existing deferred admission. Preflight
  logical size and reserve pending capacity before retaining storage or copying
  metadata; roll back failed construction. Reuse its cancellation/admission
  arbitration and release task-owned inputs before joined cleanup succeeds.
  Observer timeout/release does not cancel admitted MQTT work.

## Retention and accounting

- [x] Preserve full logical per-publication charging wherever publish budgets
  exist, even when publications share one allocation. The current native retained
  publish count/byte budget is MQTT 5-specific; do not imply equivalent MQTT 3.1.1
  accounting. Both protocols retain existing queue/packet limits and TODO41's
  separate pending-count/byte limits. This task adds no universal RSS bound.
- [x] Charge each deferred attempt's logical retained data under pending limits
  even if storage is shared. Transfer through existing native admission budgets
  without a reservation gap or bypass; release pending reservations only after
  their covered storage is released or handed into the admitted path.
- [x] Keep an owner while any queue, replay, native operation, notice or checkpoint
  work still references its bytes. An independent encoded/checkpoint copy owns
  its own storage and need not retain the original. Preserve actual use lifetimes
  without retaining application buffers unnecessarily through unrelated work.
- [x] Document logical length versus retained backing allocation: a small region
  may pin a much larger pool block or mapping. Caller-held handles and host pool
  capacity remain application-managed; publish/pending counters do not measure
  their physical storage. Bound producer concurrency and illustrate pool pressure
  during disconnects, replay and stalled admission in the ownership example.

## Release callbacks and teardown

- [x] Permit final callbacks on the thread that drops the last owner, including
  an API caller, execution worker or cleanup thread; make no host-thread affinity
  promise. Callbacks must be thread-safe, return promptly, and never unwind or
  `longjmp` across Rust. Host code may enqueue reclamation to its own executor,
  but it owns that queue's capacity and final reclamation/unload boundary.
- [x] Invoke callbacks inside the existing host-callback guard so blocking wrapper
  waits/joins are rejected, including on ordinary caller threads. Publish a tested
  whitelist of permitted reentrant APIs; include buffer retain/release through
  other live references and explicitly identify any permitted observation calls.
  Nonblocking naming alone does not establish safe reentrancy. Do not invoke a
  callback while holding a wrapper/native lock that a permitted call can acquire,
  and never reenter through the reference currently being destroyed.
- [x] Distinguish MQTT completion, client/context join, final buffer release and
  completion of host reclamation. Buffers own no client/runtime and may outlive
  them; MQTT completion or successful join cannot invalidate caller-held storage
  or prove its callback has run. Final release may follow cancellation, failed
  startup or client teardown. Keep storage immutable and userdata valid until
  callback entry; the callback then controls their reclamation. Callback code
  must remain loaded through return. Finish all handles, API calls and host
  reclamation before unloading the relevant library/module. Do not add an
  untracked reclamation task or per-buffer thread to conceal lifecycle work.
- [x] Keep existing MQTT 5 payload-format UTF-8 checks on every applicable publish,
  including deferred retries. No caller-provided validity claim, validation switch
  or cached proof is part of this initial API.

## Verification and completion

- [x] Prove pointer/storage sharing on the native owned path and exactly-once
  release across repeated publishes, cloned buffers, rejected admission, replay,
  persistence, queue overflow, pending-admission cancellation, and shutdown.
- [x] Cover failed creation without ownership transfer, NULL/non-NULL empty data,
  addressable/packet-size boundaries, invalid UTF-8, concurrent independent owners,
  and full pending/count/byte budgets. Prove pending rejection precedes retention
  and that shared storage is charged per attempt/publication under applicable limits.
- [x] Exercise QoS 0/1/2, reconnect/replay, local and broker rejection, persistence
  success/failure, failed startup, cancellation races, and client destruction on
  dedicated/shared execution for both protocols. Verify no release while original
  bytes remain referenced, and permit release after independent copies take over.
- [x] Deterministically test every whitelisted callback reentry and blocked wait,
  final release on caller/worker/cleanup paths, and retained buffers after MQTT
  completion and client/context join. Include a native C unload fixture that
  releases all owners and finishes callback/reclamation work before unloading.
- [x] Rerun Stage 0 measurements against the final public APIs, including small
  single-use messages and existing copying calls. Record costs and workload limits
  accurately; do not infer network throughput gains from pointer sharing or claim
  that codecs, persistence, TLS, WebSocket framing or kernels never copy.
- [x] Update C header/exports, an ownership example, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run core/C feature checks, native ownership fixtures, and ABI checks.

Complete when the measurement gate passes and C can safely reuse immutable
application payloads through existing native publish lifetimes, with tested
exactly-once final release, explicit unload boundaries, unchanged copying APIs
and documented measured benefits and limits. A parked proof is evidence, not a
completed public feature.


## Established implementation and evidence

The C API adds `rumqttc_publish_buffer_create`, `retain` and `release`, plus
`rumqttc_client_try_publish_buffer`, `publish_buffer_tracked` and
`publish_buffer_deferred`. `RUMQTTC_CAP_OWNED_PUBLISH_BUFFER` is unconditional
(bit 23). Existing records, ABI version and copying entry points are preserved.
The C crate's `bytes` minimum now matches wrapper-core's existing 1.12.1.

Each opaque handle owns immutable `Bytes::from_owner` storage. A final owner
returns the original pointer/length and userdata to the required callback inside
the existing host-callback guard. NULL/non-NULL empty storage retains its owner;
failed creation transfers nothing. Handles retain neither clients nor execution.
Byte alignment and addressable lengths are checked/documented independently of
existing packet validation. No C allocation is reconstructed as a Rust allocation.

Owned calls reuse the existing topic/options parser and native admission paths.
Deferred preflight reads length without cloning storage, reserves pending capacity,
then constructs the command through the existing supervisor. Every applicable
attempt/publication pays its logical charge even when backing storage is shared.
The native retained-publish budget remains MQTT 5-specific. No UTF-8 proof cache,
mutable buffer, new queue/budget policy or reclamation executor is introduced.

Release callbacks permit retain/release through other live buffer references and
library version/capability queries. Blocking waits/joins are guarded even on
ordinary caller threads; client/operation observation reentry is unsupported.
Callback code remains loaded through return; any host reclamation queue has its
own bounded lifetime and unload boundary. Independent checkpoint/encoding copies
need not pin the original application allocation.

The [measurement report](native-wrappers/c/benches/publish-buffer.md) records the
private proof, final public API results, raw samples, environment and artifact
hashes. Both protocols avoid the payload-sized FFI allocation in successful,
rejected and deferred large/reused workloads. The isolated external C comparison
passes the agreed 5% copying-path margin, including a longer follow-up for two
initially inconclusive 256 KiB cases. Small single-use overhead and diagnostic
comparisons that did not establish equivalence remain visible in the evidence.
No network-throughput or end-to-end zero-copy improvement is claimed.

Validation on Linux:

- Full default/minimal core and C suites passed. The composed file-store,
  readiness, ordered-shutdown, tracing-log-compat, Ring and WebSocket profile
  passed 604 test executions, including the new buffer persistence/rejection tests.
- `cargo hack test --manifest-path native-wrappers/Cargo.toml --each-feature
  --exclude-all-features -p rumqttc-wrapper-core-next -p rumqttc-c-next` passed
  all 41 profiles (9,854 passing test executions). The private C owner fixture
  and the public APIs are covered without requiring proof features in consumers.
- Ownership tests cover pointer sharing at the C/core boundary, native queued
  retention and reconnect replay, QoS 0/1/2, concurrent independent handles,
  pending cancellation/limits, native count/byte charges, invalid UTF-8, broker
  rejection, checkpoint copies/save failure, and caller/worker callback reentry.
  Existing native packet-limit tests continue to exercise the shared publish path.
- The native C buffer fixture exercises every new API for both protocols on
  dedicated/shared execution. The one-block ownership example and dynamic unload
  fixture pass; retained buffers survive client/context join and release before
  unloading. All 294 optional C error outputs have success/failure coverage.
- `native-wrappers/c/tests/abi/check.sh all` passed generated/header consistency,
  exports, C11/C++17 and relocated package consumers. The new native fixtures are
  included in the existing platform/sanitizer CMake test matrix.
- Rust 1.88 checks passed for core/C all targets with minimal/proof features.
  The complete native-wrapper workspace checks and core/C documentation builds
  passed. Packaging includes implementation/proof inputs and excludes raw data.
- Core/C minimal/proof Clippy passed with warnings denied and pedantic/nursery
  enabled, retaining the existing `double_must_use` / `option_if_let_else`
  exceptions. Composed standard Clippy passed with warnings denied. An additional
  composed pedantic/nursery run reports pre-existing lints in unchanged
  `wrapper-core/src/file_store.rs`, its tests and `backend/session.rs`; those
  unrelated implementations are outside this change.
- The performance evidence validator passes and rejects missing/duplicate samples.
  Rust formatting, Python lint/format checks and `git diff --check` pass.

Windows/macOS execution remains CI evidence to collect, not a local validation
claim. The public feature is implemented. Application pool/backing storage remains
host-managed and is not bounded by logical publish counters.
