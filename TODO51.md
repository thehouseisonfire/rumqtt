# Owned Immutable C Publish Buffers

Status: planned.

## Goal

Allow C to publish retained immutable application storage without an obligatory
FFI payload copy, matching the Rust owned-buffer path where practical.

## Foundation and scope

Rust publish APIs accept owned `Bytes`/`Vec` payloads. C's `publish_command()`
always uses `Bytes::copy_from_slice()`. Add a separate owned-buffer path while
keeping existing borrowed-view publish calls and their copy semantics unchanged.
This is an ownership/performance feature, not an end-to-end zero-copy guarantee.

## Implementation requirements

- [ ] Add an opaque immutable buffer handle with retain/release and transactional
  creation from application-owned bytes plus an exactly-once release callback.
  Successful creation transfers the documented ownership; failed creation leaves
  it with the caller. Specify null/empty buffers, length bounds, and alignment.
- [ ] Back native payloads with a sound owned allocation, using `Bytes` ownership
  facilities such as `Bytes::from_owner` where supported. Do not manufacture a
  static borrow or assume a C allocator can be freed by Rust. Audit Send/Sync:
  the caller may neither mutate nor release bytes while any library owner exists.
- [ ] Add borrowed-handle publish APIs for immediate/tracked forms and integrate
  [TODO41.md](TODO41.md). Each call retains immutable storage; failed admission
  leaves the caller's reusable handle valid. Reusing a buffer must not duplicate
  transmission or transfer the same owner twice.
- [ ] Charge the full logical retained payload for each admitted publication
  under existing budgets, even when several publications share an allocation.
  Queues, replay, notices, and checkpoint operations must retain storage until
  their final use; completing an MQTT operation does not necessarily release a
  caller-retained buffer. Bound pending-admission retention separately.
- [ ] Define release callback threading, prompt return/no-unwind requirements,
  permitted nonblocking reentrancy, and restrictions on blocking wrapper calls.
  Final release may occur after cancellation, failed startup, or client teardown;
  host memory and callback code must remain available through final owner release.
- [ ] Keep MQTT 5 UTF-8 validation enabled. If an immutable buffer can cache a
  validated UTF-8 proof, compute it through a checked library operation and retain
  it with those exact bytes; never trust a caller flag or reuse proof after mutation.
- [ ] Do not add raw packet mutation, packet-ID control, or a validation-disable
  switch. Prepared/validated topic handles are a separate optimization if later
  measurements demonstrate a need; they are not required for this payload feature.
- [ ] Use additive C APIs and preserve existing ABI layouts, admission semantics,
  error/delivery classification, and ordinary publish completion behavior.

## Verification and completion

- [ ] Prove pointer/storage sharing on the native owned path and exactly-once
  release across repeated publishes, cloned buffers, rejected admission, replay,
  persistence, queue overflow, pending-admission cancellation, and shutdown.
- [ ] Cover zero-length data, maximum sizes, UTF-8 checks, concurrent release,
  callbacks reentering allowed APIs, and buffers outliving clients. Verify no
  release while native/replay/checkpoint work still references the payload.
- [ ] Benchmark realistic payload sizes against copying C calls, reporting
  allocation/copy cost and retention independently of network throughput. Avoid
  claiming that codecs, TLS, WebSocket framing, or kernels never copy.
- [ ] Update C header/exports, an ownership example, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run core/C feature checks, native ownership fixtures, and ABI checks.

Complete when C can safely reuse owned immutable payload storage through native
publish lifetimes with measured copy avoidance and explicit final release.
