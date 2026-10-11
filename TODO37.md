# C Wrapper Complete Structured Diagnostics

## Goal

Expose the useful native event-loop diagnostics through owned snapshots with
explicit optional fields, protocol differences, and capture freshness. Let C
applications inspect why work is waiting without taking ownership of MQTT state.

## Foundation and feasibility

The Rust `EventLoopDiagnostics` models contain queue components, outbound state,
session-store lifecycle, batching configuration, and MQTT 5 redirect state.
Optional ordered-shutdown diagnostics add fence and deadline information.
Before this change, the wrapper reduced this to a few counters and CONNACK diagnostics.
C exposed a fixed `rumqttc_diagnostics_t`, selected redirect event accessors,
and separate ordered-shutdown, retry, and configuration observations.
[TODO30.md](TODO30.md), the core policy in [TODO34.md](TODO34.md), and the
supported update groups in [TODO35.md](TODO35.md) are already delivered; reuse
their field meanings, optionality, ownership, and activation contracts.

The field mapping is feasible without protocol changes, but responsiveness needs
an observation path independent of driver request servicing. Both drivers retain
an active native poll, which owns mutable event-loop state until it completes.
They serve cached diagnostics during that wait, and blocked application event
delivery does not service diagnostics requests. A completed native capture can
remain stale indefinitely on an idle connection with keepalive disabled.

Publish an immutable cache at safe native capture boundaries and provide an owned
C snapshot handle readable independently of event delivery. Retain the existing
public record layouts under [the ABI policy](docs/c-abi-compatibility.md); add
handles, functions, or separately named records. Keep legacy tracked diagnostics
admission, completion, field meanings, and close behavior intact.

## Snapshot contract

- One owned snapshot provides grouped accessors for lifecycle/queues, outbound
  state, session/store, batching, MQTT 5 redirects, ordered shutdown, retry, and
  configuration. Variable-length views borrow only from that snapshot; retaining
  it does not retain the client, callbacks, or mutable native state.
- Snapshot assembly time and underlying observation time are distinct. Expose
  native capture generation and monotonic age, snapshot age, and separate source
  ages for independently sampled retry, configuration, or wrapper shutdown
  observations; reuse their existing revision/generation identifiers. Repeated
  reads of a cached native sample preserve its generation and capture time;
  copying or overlaying newer wrapper state
  must not make native queue/protocol data appear newly captured.
- Native fields describe one completed capture, not the state of an active poll.
  Producer channel lengths are sequential observations even within that capture.
  Independently sampled groups do not imply one atomic cross-group instant.
  Identify each group's source and document these consistency limits.
- Distinguish available data, protocol-inapplicable or feature-disabled groups,
  and data not yet observed. A present zero remains a real value. Report wrapper
  lifecycle separately from native connected/disconnecting/disconnect-complete
  flags; wrapper termination is not proof of native disconnect completion.
- Capture initially and at safe completed-poll boundaries. Once the driver stops,
  retain the last published native sample and label its terminal cached status;
  publish a final native capture only where safely available. New reads remain
  possible while the client handle lives, including during closing and after
  termination. Already acquired snapshots survive client destruction. Neither
  reads nor capture requests start, cancel, or force a native poll.

## Implementation requirements

- [x] Inventory both native diagnostics structs and public outbound diagnostics.
  Classify each field as supported, optional/feature-specific, redundant, or
  intentionally unavailable with a reason in
  `native-wrappers/wrapper-core/PARITY.md`. Prefer useful grouped fields over
  exposing internal implementation details solely for exhaustive parity.
- [x] Add the richer wrapper snapshot, immutable published cache, and additive C
  snapshot acquisition/destruction and grouped accessors. Define availability
  and freshness metadata before implementing field mappings. Keep existing
  diagnostics accessors as their established projection of the mapped fields.
- [x] Preserve queue components separately: replay, scheduler queue, request
  channel, control channel, and immediate shutdown. Document sums to prevent
  double counting `pending_len` and its component fields. Preserve legacy
  `pending_requests` and `queued_requests` definitions rather than widening them.
- [x] Expose connected/disconnecting/complete status, outbound quota and pending
  operations, store configured/loaded/clear-pending state, identity agreement,
  and raw/effective CONNACK session semantics. Define overlapping outbound counts
  and the scope of `outbound_drained`; neither it nor queue depths substitutes
  for tracked operation or ordered-fence completion.
- [x] Expose configured/effective batching and MQTT 5 redirect lifecycle,
  attempts, target/candidate state, and optional SRV owner. Distinguish current
  snapshots from retained historical redirect events.
- [x] Integrate the existing optional ordered phase, fence sequence, remaining
  monotonic deadline, and native covered-queue observations. Preserve remaining
  duration at its stated capture time; distinguish it from any derived current
  estimate. Never expose Rust `Instant` layouts or refresh cached queue ages
  when wrapper fence/terminal state changes.
- [x] Integrate existing retry phase, attempts/budgets and failure status, and
  configuration desired/effective revisions and activation/attempt observations.
  Preserve independently dated effective batching samples from configuration
  status; label native batching at native capture separately. Reuse the supported
  field set and redaction contract rather than adding live configuration controls.
- [x] Copy variable data and retain only redacted observation values. Do not
  capture credentials, TLS material, callback owners, or application payloads.
  Initialize new failed-accessor outputs using existing C output-size rules;
  document borrowed string lifetime and explicit absence.
- [x] Make cached snapshot acquisition independent of MQTT/event queues, network
  waits, configuration preparation, and close progress. Publish with a short
  synchronization boundary; never hold cache synchronization across I/O, event
  delivery, callbacks, or large copies. Preserve event-backpressure termination
  and shutdown semantics; snapshot acquisition must not bypass event backpressure.
- [x] Retain one latest native cache per client, without completed-snapshot
  history. Share immutable owned data where practical. Accessors and repeated
  acquisition must not rescan native tracking vectors/bitsets or enqueue driver
  work. Reuse existing native captures and audit incremental copying/allocation
  cost; keep the legacy bounded diagnostic request path and fair arbitration.

## Verification and completion

- [x] Compare snapshots with native diagnostics at controlled lifecycle points,
  including each queue component, queued/inflight work, pending store clear,
  adaptive batching, redirect/SRV transitions, and ordered shutdown. Compare
  retry/configuration groups with their existing source observations.
- [x] Hold a native poll pending with keepalive disabled or stalled I/O. Verify
  repeated reads keep the native generation unchanged and its age increasing,
  even when retry/configuration or wrapper fence observations change. A later
  completed native capture advances generation; snapshot assembly does not.
- [x] Verify v4/v5 absence rules, retained data after client destruction,
  initialized failed-accessor outputs, borrowed-view lifetimes, feature-disabled
  behavior, and acquisition before first connection, during close, and after
  termination. Do not assert native disconnect-complete on wrapper-only abort.
- [x] Fill the application event queue and stall delivery. Verify cached reads
  remain responsive and do not drain/drop events, extend delivery deadlines, or
  suppress the existing overflow outcome. Flood concurrent reads on dedicated
  and shared execution; verify MQTT/keepalive, close, and a shared peer still
  progress without driver work proportional to read count.
- [x] Measure incremental capture/publication and repeated-read costs against
  current diagnostics with representative small and large protocol state.
  Verify bounded library-owned retention; distinguish caller-retained snapshots
  from library history. Run relevant wrapper-core/C tests, feature checks, and
  ABI/header/export/native-consumer checks, including existing language bindings
  affected by shared snapshot changes.
- [x] Update C header/exports, README, diagnostics example, `PARITY.md`, and
  root `CHANGELOG.md` with field definitions and freshness guarantees.

Complete when the inventoried supported diagnostics can be inspected through
owned C snapshots during waits, event backpressure, and teardown, with explicit
availability and source freshness, preserved legacy behavior and ABI, and
verified bounded overhead. No mutable protocol access or internal Rust layout
is part of the C contract.


## Delivery and verification

Delivered the protocol-neutral owned model, backend field mappings, one latest
immutable native cache, and additive owned C snapshot/group accessors. Publication
happens during preparation and immediately after completed native polls, before
failure handling or event delivery can block. Cancelled pending polls retain the
previous native sample. Driver termination records wrapper status independently.
Legacy tracked diagnostics, C layouts, and Python/JavaScript outputs are preserved.

The [field inventory](native-wrappers/wrapper-core/PARITY.md#structured-diagnostics-todo37),
[C contract](native-wrappers/c/README.md#structured-diagnostic-snapshots), and
[retained-snapshot example](native-wrappers/c/examples/diagnostics.c) document
absence, overlapping counts, independently dated sources, borrowed views,
redaction, and ownership. [Release measurements and raw results](native-wrappers/wrapper-core/benches/diagnostics.md)
record capture/publication, repeated acquisition, fixture limits, and retention.
In the measured no-default-features fixture, Rust acquisition averaged 140 ns
at both 16 and 4,096 inflight messages for both protocols. The library retains
one latest capture; older captures persist only while callers retain them.

Linux verification passed:

- Full native-wrapper workspace tests, including Python/JavaScript Rust tests.
- All 35 wrapper-core/C feature profiles (19 core, 16 C), using
  `cargo hack test --each-feature --exclude-all-features`.
- Core/C tests under shared execution with the composed AWS-LC, native TLS,
  WebSocket, proxy, SRV, SCRAM, tracing, ordered-shutdown, and core panic-testing
  features.
- Strict workspace Clippy with pedantic/nursery warnings denied, for both default
  and ordered-shutdown builds; formatting and `git diff --check`.
- ABI/header/export checks, C11/C++17 consumers, relocatable static pkg-config
  linking, and containment against the header contract from the original tree.
- Seven broker-backed native consumer/example checks, including the new retained
  diagnostic example, shared execution, reconnect, ordered shutdown, and runtime
  credential rotation.
- Release-mode cost measurements and weak-reference cache-retention tests.

The existing backend-source CI guard still flags pre-existing protocol references
in other core modules; this change introduces no new violations. macOS/Windows
execution and MSRV verification were not run locally. Measurement results do not
claim C allocation cost, contended tail latency, or end-to-end MQTT throughput.
