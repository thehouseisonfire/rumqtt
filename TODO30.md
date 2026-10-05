# C Wrapper Ordered Publish Shutdown

## Goal

Expose the native optional publish-admission fence implemented by
`disconnect_after_queued*`, including tracked completion and MQTT 5 DISCONNECT
properties. Keep ordinary graceful and immediate close semantics distinct.

This task covers wrapper-core ownership and C exposure. Keep Python and
JavaScript bindings compatible with the shared-core changes; exposing ordered
shutdown through those language APIs is a separate follow-up. Keep the feature
disabled by default in wrapper manifests and standard package profiles.

## Current foundation and feasibility

Both client crates provide this feature behind `ordered-shutdown` in
`src/client_disconnect.rs`. `docs/recipes/ordered-shutdown.md` defines its
ordering, deadline, reconnect, persistence, and failure contracts.
Wrapper-core and C now forward the disabled-by-default feature and expose the
native notice through a distinct ordered-close policy.

This is feasible by admitting the native fence and observing its notice.
Waiting for a wrapper snapshot of outstanding completions is not an equivalent
implementation: the fence must use the native successful-admission order.

The wrapper lifecycle uses a separate ordered-close policy.
`ShutdownCoordinator::poll_error_action` treats ordinary graceful-close connection
errors as terminal, while `runtime::run_driver` cancels the driver when its graceful
timeout watcher wins. Reusing those policies would prevent supported ordered
reconnects and abandon terminal persistence work after expiry.

Enabling the native feature changes admission and publish observation even when
ordered shutdown is never requested. The ordered-shutdown recipe reports
31--68% lower synthetic admission throughput and about 10% lower broker-backed
MQTT v4 QoS 1 throughput in its measured workloads. These are native measurements,
not wrapper estimates: wrapper-core already serializes admission and tracks
publishes. Measure its incremental cost before making packaging decisions.

## Implementation requirements

- [x] Forward an explicit `ordered-shutdown` feature through wrapper-core and
  C packaging, and report it through the capability query. Enabling it must be
  explicit; document Cargo feature unification and the cost for clients that
  never request ordered shutdown. Keep C declarations and exports available in
  disabled builds, returning a documented unsupported configuration error with
  initialized outputs and no admission or lifecycle changes. The capability bit
  reflects callable C API support. Document that feature unification can enable
  native runtime costs even when the wrapper API feature remains disabled.
- [x] Add separately named ordered-disconnect commands and C admission APIs,
  including nonblocking, tracked, finite-deadline, and v5-properties forms.
  Use additive functions, constants, and opaque ownership under
  `docs/c-abi-compatibility.md`; preserve existing public record layouts and
  close/completion contracts. Make the finite-deadline form the primary example;
  document that the no-deadline form may wait indefinitely.
- [x] Delegate fence installation to the native client gate. Producers admitted
  before the fence are covered; later producers receive an explicit closing
  error. A full nonblocking admission installs no latent fence. Roll back wrapper
  operation allocation and closing state when native admission fails. Capacity
  waiters do not hold the wrapper admission gate across an await; cancellation
  before admission leaves the client open and installs no fence.
- [x] Bridge the native `DisconnectNotice` to one owned wrapper operation.
  Distinguish fence admission, collective completion, DISCONNECT flush, terminal
  persistence cleanup, and execution-owner join. Native notice success is the
  authority for ordered-operation success; `RequestsDone`, thread exit, an
  outgoing DISCONNECT event, or successful immediate cleanup cannot substitute
  for it. Observe and resolve the notice exactly once, even if its foreign
  observer is destroyed; retained completion handles remain usable after client
  destruction without retaining an unbounded completed-operation history.
- [x] Preserve QoS milestones, successful-admission ordering across callers,
  negative-ACK failures, and the publish-only completion scope of the native
  contract. Do not claim covered subscriptions or independent inbound ACKs.
- [x] Integrate ordered close with existing shutdown ownership and idempotent
  destruction through a distinct shutdown policy. Define repeated calls and
  conflicting close modes/payloads in an explicit behavior table. Raw fence
  admission follows native first-successful-admission wins; any idempotent closer
  coalesces only matching ordered policies/payloads and retains the original
  deadline; later callers have independent observer budgets. Ordinary graceful
  close cannot replace or downgrade an admitted fence.
  Explicit immediate escalation fails the ordered operation as superseded and
  makes no successful-fence claim. Owner destruction/abandonment remains a
  documented immediate-abort boundary; a join timeout retains join ownership.
- [x] Allow native-supported persistent-session recovery while ordered close is
  active. Do not apply ordinary graceful close's fail-on-connection-error policy
  or recreate the event loop/fence. Let native recovery determine whether replay
  remains possible; terminal notice failures must stop retries rather than be
  converted into generic reconnect attempts or successful close.
- [x] Preserve the absolute native deadline across reconnect and persistence.
  Separate observer timeout, capacity wait, and join budget from the operation
  deadline. Repeated callers cannot extend or reset it. Validate duration overflow
  before admission; zero expires after successful admission. Observer timeout or
  cancellation never cancels admitted work or replaces its terminal result.
- [x] Keep deadline enforcement responsive while application event delivery is
  backpressured or an asynchronous host transport, handshake, authentication, or
  store future is pending. Do not reuse the graceful watchdog's driver-drop path
  or cancel and resume a partially progressed native poll. Coordinate event
  delivery waits with native deadline progress, preserving bounded buffering and
  documented overflow behavior. Synchronous foreign callbacks retain their
  existing prompt-return contract; do not promise forced callback preemption.
- [x] Continue required terminal persistence polling after timeout when native
  cleanup remains pending, until `RequestsDone` or a terminal cleanup failure.
  Resolve the operation timeout promptly while the driver retains cleanup and
  join ownership. Cleanup success cannot change that result to success, and later
  cleanup failure remains observable through the driver terminal outcome.
  Bound each caller's wait without claiming cleanup itself is deadline-bounded.
  A timeout must not imply that previously flushed packets were undelivered.
- [x] Preserve session-loss, redirect, reset, and ambiguous-delivery failure
  reasons. Coordinate recovery commands in [TODO36.md](TODO36.md).
- [x] Add fence/phase observations through [TODO37.md](TODO37.md) without making
  outgoing DISCONNECT events substitutes for terminal notices.
  Coordinate interfaces with TODO36/TODO37 without requiring their full delivery
  to expose the core fence. Existing recovery paths must retain honest failure
  reasons; richer recovery commands and diagnostics can land separately.

## Verification and completion

- [x] Use concurrent C producers and broker barriers to prove admission order,
  closing rejection, QoS 0/1/2 completion, and no latent fence on backpressure.
  Reproduce the inflight-one A/B case: ordinary graceful close may overtake queued
  B, while ordered close requires A completion, B completion, then DISCONNECT.
  Keep both behaviors as separate regressions for v4 and v5.
- [x] Cover reconnect, negative ACKs, persistence failure, zero/expired deadlines,
  duration overflow, cancellation while waiting for admission, observer timeout
  and destruction, retained observers after client destruction, and pending
  cleanup after timeout. Verify native QoS milestones, failure reasons, delivery
  ambiguity, and publication-only scope rather than merely observing DISCONNECT.
- [x] Exercise matching/repeated and conflicting close callers, ordinary-close
  attempts during an ordered fence, immediate escalation, owner destruction,
  abandonment, and independent observer/join budgets. Prove no deadline reset,
  duplicate resolution, successful ordered result after abort, or lost join owner.
- [x] Use controlled full event queues and pending asynchronous host futures to
  prove deadline responsiveness. After expiry, release a pending store operation
  and verify cleanup continues, the timeout result remains unchanged, and the
  driver joins only after cleanup finishes or terminally fails, unless an
  explicit abort relinquishes that cleanup under its documented contract.
- [x] Check enabled and disabled feature builds, capability bits, unified native
  features, header/export parity, and packaged C consumers. Run shared-core
  Python/JavaScript lifecycle and completion regressions without adding new
  public language API requirements.
- [x] Benchmark wrapper publish admission throughput and p50/p95/p99 latency,
  broker-backed QoS 0/1/2 throughput, and shutdown latency with the feature enabled
  and disabled. Include enabled clients that never request ordered shutdown,
  single and concurrent producers, bounded queues, and representative inflight
  limits. Use interleaved repeated release runs on the same host, report variation
  and configuration, and record allocation/memory cost where practical. Publish
  the incremental wrapper cost and explicit packaging decision; investigate
  disabled-build regressions rather than attributing them to native figures.
- [x] Update feature packaging, C header/exports, examples, README,
  `PARITY.md`, root `CHANGELOG.md`, and the ordered-shutdown recipe.

Complete when C provides the native fence guarantee and ordinary close retains
its existing admitted-protocol-work drain contract, including with the feature
enabled. Default builds retain their existing admission behavior; optional
builds expose honest timeout/cleanup ownership and measured runtime costs.

## Implementation and validation evidence

The additive C API is in `native-wrappers/c/src/ordered.rs` and the checked header.
Wrapper ownership, policy, deadlines and notice bridging are in wrapper-core
`ordered.rs`, `shutdown.rs`, `handle.rs` and `runtime.rs`; protocol conversion
remains in `backend/`. Native notices expose immutable admitted sequence/deadline
metadata. The C README contains the close-policy behavior table.

Behavior tests: `wrapper-core/tests/ordered_shutdown.rs` covers both protocols,
QoS 0/1/2, the ordinary-versus-ordered inflight-one regression, concurrent
admission, full/cancelled admission, reconnect/session loss, observer lifetime
and deadlines, full event queues, typed rejection, pending persistence cleanup,
and explicit abort, including abort after expiry closes native admission while
terminal cleanup remains pending. A publication-only regression leaves SUBACK and an inbound
PUBACK outstanding. Shutdown unit tests preserve a native result already ready
when destruction races its observer. A panic-containment regression preserves
native receiver termination on retained fences while the driver reports its
internal panic. Backend tests retain typed failure reasons
and redact source text.

`c/tests/native/native_ordered_shutdown.c` covers concurrent C producers, QoS
milestones, copied v5 properties, repeated/conflicting closes, retained
completions, typed rejection, truncated diagnostics and disabled output/admission
contracts. The enabled native C suite passed all 60 tests on Linux. Default
consumers, header/export/C++/pkg-config checks and installed minimal, ordered,
Rustls-ordered and core-only-unified profiles passed. Standard release profiles
remain disabled. Cross-platform CI runs the new Rust and C cases; macOS/Windows
execution is not claimed from this Linux session.

Both native client crates passed full tests and their 36-build Cargo feature
matrix, plus the feature Clippy matrix. The wrapper-core/C 35-build feature
matrix, default/enabled workspaces, minimal ordered build, combined TLS/proxy/
authentication features and strict enabled/disabled Clippy passed. Python passed
118 unit/API tests and 69 broker/lifecycle tests; Node passed 15 tests, the typing
check and broker behavior suite with unified ordered core support. No new public
Python or JavaScript ordered API was introduced.

Performance evidence and reproduction commands are in
[`wrapper-core/benches/README.md`](native-wrappers/wrapper-core/benches/README.md).
The 48-configuration release matrix uses seven interleaved timing repetitions
and separate allocation samples per profile. Enabled-unused median completion
throughput is 9.9% lower across configurations, with 16–27% lower QoS 1/2 rates in
the single-producer, capacity/inflight-64 cases. Disabled median throughput is
1.1% above the baseline; the two largest initial slowdowns did not reproduce in
15-repetition follow-up runs. Optional diagnostics were boxed to remove
per-publication allocation growth; disabled admission retains its original
mutex. The CSV files retain observed variation. Standard packaging remains
disabled, and the evidence is synthetic loopback traffic on this Linux host.
