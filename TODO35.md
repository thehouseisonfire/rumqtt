# C Wrapper Runtime Configuration Updates

## Goal

Allow safe updates to supported client configuration after startup, including
credential/TLS rotation for later connections and runtime batching controls.
Expose effective and pending configuration rather than mutable native objects.
Implement the behavior in wrapper-core and expose it through additive C APIs.

## Current foundation and feasibility

Rust exposes options through the event loop and explicit network-option setters.
The wrapper now stages a bounded, audited set of runtime updates and reports
desired/effective revisions. Mutating a Rust options field does not prove that
all derived runtime state changes correctly, so setters need individual
applicability audits.

Updates are feasible as serialized driver commands with defined activation
boundaries. Protocol version, active packet-ID state, and queue ownership must
not change by replacing an entire options object while connected.

Both wrapper drivers retain a pending native poll across control wakeups because
polling can dequeue requests and mutate state before awaiting I/O. An update
must not cancel that poll and resume as though no work occurred. A poll boundary
can also be indefinitely delayed on an idle connection with keepalive disabled.
MQTT 5 temporary redirects retain and later restore previous options.

## Delivery scope

- Deliver audited batching/throttle updates and coherent next-attempt
  broker credentials, broker TLS profiles, and network settings. Keep broker
  endpoint, transport kind, and session identity fixed. Credential replacement
  is for rotation within the same application identity, not session migration.
- Defer replacement handshake/authentication authorities, proxy/redirect
  profiles, and other fields until their applicability and ownership are audited.
  Existing WebSocket callbacks already support per-attempt token refresh.

## Implementation requirements

- [x] Inventory native setters and classify each as construction-only,
  applicable at a safe poll boundary, next-attempt, or requiring a session reset.
  Check actual reads and derived state in both event loops, not just visibility.
- [x] Define separate runtime-tuning and connection-profile update groups.
  Initially audit `max_request_batch`, `read_batch_size`, and `pending_throttle`;
  retain native adaptive/clamping semantics and report configured versus effective
  values. Network changes apply to newly created sockets, not an existing socket.
- [x] Add typed partial update objects and tracked C commands. Copy all values
  and retain registrations at admission. Distinguish unchanged, replace, and
  clear; treat username/password as a coherent credential value. Serialize
  updates and validate each complete proposal against the latest committed
  desired configuration before changing either group. Invalid or unsupported
  updates change nothing, including when mixed with supported fields.
- [x] Bound pending update count and copied bytes, including TLS material and
  retained owners. Full capacity rejects admission with backpressure. Define
  supersession and release obsolete staged values without unbounded history.
- [x] Commit validated desired configuration atomically on the driver and assign
  a monotonic revision. Record accepted/staged status and effective revision
  separately for each group; mixed updates need not activate simultaneously.
  Define concurrent update ordering and which fields each revision supersedes.
- [x] Define tracked completion as driver staging or rejection, not successful
  connection or peer authentication. Expose activation observations that identify
  the attempt revision, successful connection revision, and superseded updates.
  Observer cancellation or wait timeout must not cancel an admitted update.
- [x] Receive and stage updates while a native poll is pending without cancelling
  it. Apply runtime tuning only at a proven safe boundary. Document potentially
  unbounded activation delay on an idle connection. Keep control admission bounded
  and fair, and document event-backpressure effects on progress.
- [x] An attempt already in progress retains its original coherent profile.
  Activate staged connection settings before the next attempt starts, including
  across retries; never combine credentials, TLS, or network inputs from different
  profile revisions. Staging alone neither starts nor cancels an attempt.
- [x] Separate local profile-preparation failure from connection/authentication
  failure. Preparation failure leaves desired configuration unchanged. Once a
  new profile is activated, retain it across retryable failures; never silently
  fall back to retired credentials or trust policy. Report failures with revision
  context and preserve existing terminal-error behavior.
- [x] Initially reject connection-profile updates during an active MQTT 5
  redirect transition or while using a redirected target. Define how a staged
  origin profile interacts with subsequent redirects and origin restoration:
  restored options must not lose a committed rotation, and origin credentials
  must not leak to isolated targets. Reject unsupported combinations before
  commit; add a native transition API if saved options cannot be updated safely.
- [x] Keep existing callback/secret owners until active work releases them and
  reject stale callback completions using the owning operation/attempt generation.
  Use zeroizing storage for wrapper-owned retired secret buffers, auditing broker
  password `Bytes` copies as well as TLS inputs. State the erasure boundary:
  native/TLS-library allocations and application-owned copies have independent
  lifetimes. Define TLS session-cache handling so rotation does not resume an
  old security identity or bypass the newly selected trust policy.
- [x] Reject protocol, broker endpoint, transport-kind, ACK-mode, live queue-size,
  store-owner/scope, client-ID, and session-policy changes in the initial API.
  Reject enhanced-authentication authority replacement until native cached state
  and active exchanges have a safe transition. Route destructive changes through
  [TODO36.md](TODO36.md) or require a new client.
- [x] Reject unsupported fields with a typed error and preserve ABI record layouts.
  Do not accept a setter whose effect is only a changed diagnostic value.
- [x] Ship minimal owned revision/activation accessors with this API, then integrate
  them into [TODO37.md](TODO37.md). Expose redacted effective/pending values and
  explicit snapshot freshness; never return secrets or mutable native objects.
  Keep startup configuration handles independent of running clients.

## Verification and completion

- [x] Rotate broker credentials and TLS identities across natural reconnect and
  verify the peer sees one coherent profile revision. Cover TLS resumption, trust/pin
  changes, failed authentication, retry, and attempts already in progress.
- [x] Verify effective batching/throttle changes and network options on newly
  created sockets for both protocols, including adaptive/clamped batching.
- [x] Cover idle connections with keepalive disabled, slow connection attempts,
  full event queues, update flood limits, and fairness on shared execution.
- [x] Cover invalid-update rollback, concurrent/overlapping partial updates,
  mixed activation groups, supersession, observer timeout/cancellation, failed
  preparation/activation, shutdown races, and retired owner release.
- [x] Cover rejected redirected-target updates and rotation surviving temporary
  origin restoration without cross-target credential inheritance.
- [x] Update C header/exports, README, rotation example, `PARITY.md`, and root
  `CHANGELOG.md` with field applicability, activation timing, failure behavior,
  ownership/erasure limits, and completion meanings. Run relevant wrapper-core,
  C behavior, feature-matrix, and ABI checks for the supported field set.

This plan is complete for its documented supported field set when updates and
their activation are safely observable. The full TODO37 diagnostics expansion
has separate completion criteria. Do not advertise arbitrary live `MqttOptions`
mutation as a safe C capability.

## Delivery record

The supported updates are implemented in wrapper-core and the additive C API.
The field applicability, ownership, activation and snapshot contract is documented in
[native-wrappers/wrapper-core/runtime-configuration.md](native-wrappers/wrapper-core/runtime-configuration.md).

Linux verification:

- Full native-wrapper workspace tests and native core/v4/v5 client tests passed.
- Wrapper-core/C and native v4/v5 each-feature test matrices passed, as did the
  native client each-feature Clippy matrix.
- Strict native-wrapper workspace Clippy (`--all-targets -D warnings`, pedantic
  and nursery) and native core Clippy passed.
- The configuration integration suite passed all 15 tests on dedicated execution
  and shared execution, including composed Rustls/native TLS and ordered shutdown.
  Unit tests passed without optional features and with composed TLS/ordered shutdown.
  Regressions cover a busy blocking pool, vector-entry byte accounting and retired
  startup password/verifier owners with custom connectors for both protocols.
  Successful permanent redirects release obsolete origin declarations while
  preserving tuning and redacted snapshots. Cancelled preparation keeps teardown
  pending while captured/result owner destructors run for either protocol.
  Permanent-target success retires origin owners and receipts before a full event
  queue can time out Connected delivery.
  With one preparation and fifteen updates queued behind a busy blocking pool,
  MQTT v4/v5 graceful and ordered shutdown resolve configuration requests before
  completion draining, while teardown still waits for detached preparation cleanup.
- `native-wrappers/c/tests/abi/check.sh all` passed: generated-header parity,
  exports, C/C++ consumers and relocatable static pkg-config consumption. Comparing
  the current header with the original header confirmed containment of all existing
  function signatures and record layouts.
- Native C consumers and examples compiled with strict warnings. Initial full C
  runs encountered intermittent assertions in the existing publish-admission and
  invalid-authentication fixtures; each passed its isolated rerun. These results
  are retained in the verification logs rather than treated as clean initial runs.
  The final rebuilt-library run passed all 69 enabled C tests; the ordered-shutdown
  example was skipped because that C library feature was disabled. The run allowed
  one retry per test, but no retries were needed.

macOS/Windows execution remains pending. Activation on an idle connection with
keepalive disabled may remain pending indefinitely; snapshots make that state
observable. Connection profiles activate before the next natural origin attempt.
