# C Wrapper MQTT 5 Publish Admission Policy

## Goal

Make MQTT 5 publish admission configurable between native event-loop validation
with offline queueing and strict validation against negotiated capabilities.
Provide bounded process-local publish admission without inventing an application
outbox. Keep the wrapper's existing strict default.

## Native foundation and resource contract

The client builder accepts `PublishAdmissionPolicy` in
`rumqttc-v5/src/client.rs`; the enum lives in `src/publish_admission.rs`.
Wrapper-core defaults to `RequireNegotiatedCapabilities`; `V5Config` and the C
configuration setter can select either native policy.

The policy selector is a direct configuration addition, but the complete feature
also requires retained-work accounting, structured error mapping, and an alias
admission/replay audit. Do not assume the strict path's behavior is universal.

The native request channel is bounded, but reconnect cleanup drains requests
into the event loop's replay deque and frees channel slots without necessarily
completing those requests. Channel capacity alone does not establish a bound on
all retained publish work. Establish and enforce that bound before advertising
bounded offline admission; any required native accounting is a prerequisite,
not a reason to add a wrapper-owned payload queue.

`EventLoopValidated` defers negotiated-capability checks while connected as well
as offline. It changes rejection timing, not only permission to queue offline.
Strict producer admission likewise does not guarantee eventual MQTT success:
requests remain subject to native processing and later connection changes.

## Implementation requirements

- [x] Add an explicit v5 configuration selector in wrapper-core and an additive
  C setter. Map it directly to the native builder policy and preserve the strict
  default. Reject invalid selector values without mutating the configuration.
- [x] Select the policy at client construction. Do not expose live policy
  mutation until a native transition contract exists. Editing a reusable C
  configuration affects only subsequently constructed clients.
- [x] Preserve strict behavior before CONNACK and while reconnecting: QoS 1/2,
  retained, and alias-bearing publishes require known capabilities; ordinary
  alias-free non-retained QoS 0 remains admissible.
- [x] In event-loop-validated mode, admit eligible offline publishes into the
  existing bounded native channel and report later negotiated rejection through
  tracked completion. Apply the same deferred checks while connected. Admission
  must not be reported as broker acceptance; QoS 0 completion remains a local
  network-flush result rather than a broker acknowledgement.
- [x] Keep intrinsic packet validation eager in both modes. Expose stable
  machine-readable distinctions for invalid local packets, unknown negotiated
  capabilities, request-channel exhaustion, retained-work budget exhaustion,
  and rejection against known negotiated capabilities. Do not require callers
  to parse diagnostic strings or infer a reason from connection phase.
- [x] Map tracked negotiated rejections and alias failures to structured native
  reasons. Report known rejection before transmission separately from ambiguous
  delivery. Preserve actual broker ACK reasons separately from local rejection;
  do not invent a broker reason for an event-loop capability check. Define retry
  semantics for every reason, including which connection or capacity change
  makes another attempt meaningful.
- [x] Preserve the native topic-alias mapping and replay contract. Audit
  alias-only publishes before connection and across cleanup boundaries; reject
  unrecoverable replay with the native reason instead of repairing it in C.
- [x] Audit `try_*` and tracked admission for both policies so callers have a
  reliable retry signal. `try_*` must return immediately when admission cannot
  proceed, with a definite not-admitted result. Blocking/async admission must
  wait without spinning or missing capacity/capability wakeups, and wake on
  shutdown. Cancellation before admission must not enqueue work or commit alias
  state; dropping an observer after admission must not cancel admitted work.
- [x] Define and enforce finite per-client limits for total outstanding publishes
  and retained publish bytes, with explicit configuration/defaults and C access.
  Account for requests in the native channel, scheduling/replay queues, protocol
  state, and their tracking overhead; queue transfers must not replenish the
  budget. Include restored replay work in the resource contract. Document byte
  accounting and fixed overhead rather than treating slot count as a byte bound.
- [x] Acquire retained-work capacity transactionally with admission and release
  it on the native terminal outcome, including local rejection, flush/ACK
  completion, session loss, alias failure, and shutdown. Observer release must
  not release capacity early. Enforce accounting at the shared admission/native
  ownership boundary without duplicating MQTT behavior or adding a secondary
  payload queue. If this needs a native change, complete that prerequisite first.
- [x] State the limits of the resource contract: caller-owned commands awaiting
  admission and caller-retained completed results need separate application
  bounds. Do not claim that admission limits bound arbitrary producer concurrency
  or every allocation retained by the application.
- [x] Reject the v5 policy selector on v4 rather than ignoring it.
- [x] Document that offline admission is process-local queueing, not a guarantee
  of durable persistence before protocol processing. Session checkpoints remain
  recovery state rather than an application outbox. Explain connected rejection
  timing, strict retry behavior, and each mode's admission/completion boundary.

## Verification and completion

- [x] Before first CONNACK, while connected, and during reconnect, compare both
  policies using QoS 0/1/2, retained packets, topic aliases, full channels, and
  exhausted retained-work budgets in native C. Check the unchanged strict default,
  invalid selectors, v4 rejection, and configuration reuse without live mutation.
- [x] Verify later rejection when the broker advertises lower QoS or no retain,
  including changed capabilities after reconnect. Check structured reasons,
  retryability, delivery status, and exact terminal outcomes after session loss
  or alias replay failure. Local negotiated rejection must not abort unrelated
  valid operations or masquerade as a broker ACK.
- [x] With small count/byte limits and continuously active producers, repeat
  connection loss and session resume while preventing MQTT completions. Assert
  that total outstanding work and retained bytes stay within the defined bounds
  as requests transfer through native queues. Cover failed establishment,
  sustained outages, large payloads/properties, and restored replay work; checking
  request-channel length alone is insufficient.
- [x] Verify capacity is reclaimed exactly once on every terminal path. Cover
  cancelled admission waits, dropped completion observers, concurrent producers,
  alias binding on failed admission, and shutdown while blocked. Use deterministic
  broker/callback barriers for wakeup and queue-transfer races.
- [x] Demonstrate offline queueing in an example and document its memory and
  durability boundaries alongside strict retry behavior.
- [x] Update C header/exports, configuration docs, `PARITY.md`, and root
  `CHANGELOG.md` without changing existing defaults.

Complete when C callers can select either native admission policy and observe
the correct distinction between local admission and MQTT completion, with
machine-readable failure/retry semantics and an enforced retained-work bound
demonstrated across repeated reconnect cycles. A working selector or a bounded
request channel alone does not satisfy completion.

## Implementation and verification record

The implementation uses optional native RAII reservations on terminal senders,
with wrapper defaults of 1,024 outstanding publishes and 16 MiB of charged data.
Checkpoint restoration reserves PUBLISH/PUBREL before opening new admission;
failed preflight leaves the checkpoint intact. Separate progress notification
avoids cleanup lock recursion, and channel rollback does not wake its own retry.
Conservative native transmission history separates fresh local rejection from
ambiguous replay rejection. See
[`publish-admission.md`](native-wrappers/wrapper-core/publish-admission.md)
for the public accounting and retry contract.

Contract coverage now includes:

- `native_publish_admission`: both policies before CONNACK, while connected and
  during reconnect; QoS 0/1/2 and retain combinations; bound and alias-only topics;
  channel/count/byte exhaustion; strict defaults, invalid selectors, v4 rejection
  and configuration reuse. Broker barriers verify failed alias binding rollback,
  observer release, QoS 2 through PUBCOMP, changed QoS/retain capabilities,
  session loss and unsent alias replay failure. Structured reason, delivery and
  retry fields are checked without inventing broker reasons; valid work continues.
- Wrapper `publish_admission` tests: continuously active producers retain large
  payloads/properties across repeated loss, failed establishment and session
  resume, with separate count/byte saturation and a smaller request channel.
  Snapshots remain within limits, replay retains identifiers/data, cancelled waits
  change no accounting, and ACK progress wakes blocked admission.
- Native `publish_budget`/`publish_budget_regressions` and recovery unit tests:
  repeated queue transfers, concurrent producers, rendezvous readiness and
  cancellation, checkpoint count/byte preflight, mixed PUBLISH/PUBREL restoration,
  and recovery gating after redirects or public session-key changes. Failed or
  cancelled loads stay gated; failed preflight preserves the checkpoint for retry.
- Native notice tests: every terminal success/error variant and sender destruction
  reclaim capacity exactly once, including dropped observers. Success/error results
  release before observation, and reading a result cannot release a subsequent
  reservation. Wrapper/C tests cover blocked shutdown,
  failed admission rollback, repeated completion reads and QoS 2 lifetime.
  Sustained registration traffic tests preserve MQTT/authentication/shutdown progress.

Completed Linux validation on 2026-10-06:

- Default v4/v5 client suites: 970 passed; wrapper-core/C suites: 292 passed.
  The separately enabled ordered-shutdown v5 library suite passed 575 tests.
  The optional real-Mosquitto wrapper will test also passed.
- `cargo hack test --each-feature --exclude-all-features` passed all 36 client
  configurations and all 33 wrapper-core/C configurations, including SCRAM.
- Client-library Clippy passed all 36 feature configurations with `--no-dev-deps`,
  `--lib`, `--no-deps`, `-D warnings`, `clippy::pedantic` and `clippy::nursery`.
  The native wrapper workspace passed the same strict lints with `--all-targets`.
- The native C suite and examples passed all 63 tests using the C library built
  with `tls12,use-native-tls,proxy,auth-scram`. The expanded admission fixture
  passed repeated targeted runs. `native-wrappers/c/tests/abi/check.sh all` passed
  header/source parity, exports, C/C++ consumers and relocated static pkg-config.
- Main and session-store-file workspace checks passed. Rust formatting checks,
  Python fixture compilation and `git diff --check` passed.

The C suite passed serially. An initial parallel run failed the existing stress
and invalid-auth fixtures; both passed isolated reruns and two complete serial
runs. Existing skipped documentation examples and legacy ordered-disconnect
placeholders remain ignored; ordered-shutdown feature tests execute separately.
These results replace the earlier socket/SCRAM restrictions. macOS/Windows native
execution remains in the platform CI matrix.
