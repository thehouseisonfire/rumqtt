# C First-Connection Observer

Status: implemented, including optional readiness integration. Linux validation
is complete, with one transient TLS test timeout documented below.

## Goal

Expose wrapper-core's repeatable first-successful-connection observation through
an independently owned C handle, without consuming application events.

## Foundation and scope

`ClientHandle::connection()` returns a cloneable `ConnectionHandle` with pending,
first success, and terminal-before-success states. Retryable attempt failures
leave it pending; later disconnects do not erase the first success. C previously
required applications to build this observation from their single event consumer.

This is a historical startup result. Observing it neither consumes events nor
changes connection attempts, reconnection, session recovery, or shutdown.

## Implementation requirements

- [x] Add C acquisition, retain/release, poll, and timed-wait APIs backed by the
  existing connection cell. Use `first_connection` in public observer/API naming
  to make the historical scope explicit. Return selected protocol and raw Session
  Present with required explicit state: pending, first success, or terminal
  failure before success. Use a distinct observer type rather than inventing a
  command, MQTT operation ID, or event-queue request. Result fields are valid only
  in the first-success state.
- [x] Add a core timed blocking observation primitive using a condition variable
  on the existing result cell, alongside its asynchronous notification. Publish
  each immutable result once and wake all blocking/asynchronous waiters. Use a
  monotonic caller deadline and handle spurious wakeups; do not allocate a
  runtime/thread per wait or repeatedly poll. Reject nonzero waits on execution
  workers and in callbacks, even when the result is already available. Zero-time
  observation must remain nonblocking.
- [x] Preserve first-success semantics, including MQTT 5 redirects and session
  recovery. Success is the native accepted connection boundary, not application
  event delivery, resubscription completion, or a currently-connected guarantee.
- [x] Return the existing terminal error if shutdown/failure occurs before first
  success. Polling a pending result returns `WOULD_BLOCK`; an expired timed wait
  returns `TIMEOUT` with state still pending. A cached terminal error returns the
  terminal-failure state, including when that error is itself `TIMEOUT`. Do not
  infer observation state from the status/error code alone. Independent timeout
  ends only that wait; releasing an observer never cancels startup or MQTT work.
  Multiple waiters and later acquisitions observe the same immutable result
  without resetting it.
- [x] Keep raw Session Present distinct from effective session resume. Reference
  existing CONNACK compatibility diagnostics; any added effective fields must
  be captured at that first connection rather than sampled from a later one.
- [x] Allow acquisition from a still-valid C client during closing and after
  driver termination, returning the same connection cell without requiring a
  running driver or command admission. Retained observers survive client
  destruction and retain only observation state, with no client, runtime,
  callback authority, or event consumer. Acquisition requires a live client
  handle; it may not race that handle's destruction. Document concurrent
  observation and independent retained ownership, including release rules.
- [x] Preserve event-backpressure behavior. Applications must continue draining
  events: a full event queue can prevent further connection progress or terminate
  the client. Once the accepted connection boundary resolves first success,
  blocked/failed delivery of its Connected event cannot erase that success.
  Independent observation does not promise progress through a stalled event queue.
- [x] Preserve public C layouts and error-output conventions with additive APIs,
  following [the C ABI policy](docs/c-abi-compatibility.md). Initialize all supplied
  outputs before validation and document their validity on observation errors.
  Retained observers do not authorize library unload: release them and finish
  concurrent API calls before unloading.

## Optional readiness integration

- [x] Add connection-observer registrations to [TODO42.md](TODO42.md) using its
  existing immutable-result registration machinery. Registration must neither
  consume the result nor retain client/runtime ownership. Cover registration
  after completion, acknowledgement/rearm, and retention after client destruction.
  Track this integration separately: basic observer completion and release do not
  depend on readiness integration or TODO42's outstanding acceptance gates.

## Verification and completion

- [x] Test both protocols with several failed attempts before success, terminal
  rejection/exhaustion, shutdown before connection, and success followed by loss.
- [x] Verify multiple concurrent observers, timeout/retry, acquisition after
  success/failure and during closing, and retention after client destruction.
- [x] Distinguish a wait deadline from a cached terminal `TIMEOUT`; race publication
  with waiting/deadline expiry and verify later observations retain the result.
  Verify releasing observers does not cancel startup or affect other waiters.
- [x] Saturate the event queue before connection acceptance and after first
  success. Verify existing backpressure termination and immutable success even
  when the Connected event cannot be delivered; prove no competing event consumer.
- [x] Cover redirects, raw/effective Session Present compatibility, forbidden
  blocking contexts (pending and completed), zero-time observation, output
  initialization, and unload ownership.
- [x] Update header/exports, C startup example, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run core/C observer suites and ABI consumers. The startup
  example must preserve the event stream and explain the continuing drain duty.

Basic observer work is complete when every C observer can independently await
and reread the first successful connection or prior terminal failure without
stealing connection events, with unambiguous deadline/state and ownership rules.
Optional readiness integration is completed and reported separately.

## Implementation and validation evidence

The C API provides `rumqttc_client_first_connection`, independent retain/release,
repeatable poll and timed wait, and `RUMQTTC_CAP_FIRST_CONNECTION`. Explicit states
are `UNOBSERVED`, `PENDING`, `SUCCEEDED`, and `FAILED`: rejected observations leave
`UNOBSERVED`, while a wait deadline and a cached terminal timeout have different
states. Protocol and raw Session Present are valid only on success; effective
session fields are not added. The core uses the existing cell with a condition
variable and asynchronous notification, with no per-wait execution resources.

Optional readiness is implemented separately as immutable `FirstConnection`
registrations using the existing registration limits, generation checks,
acknowledgement/rearm, and close/quiescence protocol. It retains observation state
without extending client/runtime/callback ownership. TODO42's outstanding
performance and native-platform acceptance items remain independent.

Linux validation passed default, minimal-with-readiness, and composed core/C
suites, shared-execution connection/recovery/reconnect scenarios, and the focused
observer/readiness suites. Tests cover terminal timeouts versus wait deadlines,
multiple waiters, publication races, stale registrations, in-flight close,
retry/redirect/recovery boundaries, raw/effective compatibility, event overflow
before/after acceptance, callback blocking rejection, closing/failed acquisition,
last-observer release, and retained results after client destruction.

C11/C++17 headers, generated declarations, exports, static pkg-config consumers,
and native observer/readiness/unload consumers passed on Linux, as did the updated
startup and readiness examples and pedantic/nursery Clippy. All 20 core feature
profiles passed across the initial and resumed validation runs. The environment
refresh cleared the initial build artifacts/logs after the first 11 profiles;
the remaining nine profiles were rerun. Ring initially hit a one-second driver
join timeout in the unchanged TLS cancellation test
`deferred_pending_hooks_cancel_without_blocking_driver_join_and_drop_owned_work`.
That test passed in isolation and the full Ring suite passed on rerun without
code changes. This transient failure is recorded rather than treated as a clean
first-pass matrix result. Native macOS/Windows execution is left to the existing
CI jobs; Linux evidence does not claim execution on those platforms.

Key validation commands (from the repository root):

```sh
cargo check --manifest-path native-wrappers/Cargo.toml --workspace
cargo test --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next -p rumqttc-c-next
cargo test --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next -p rumqttc-c-next --no-default-features --features readiness
cargo test --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next -p rumqttc-c-next \
  --features readiness,tls12,use-native-tls,websocket,proxy,ordered-shutdown,tracing-log-compat
cargo hack test --manifest-path native-wrappers/Cargo.toml \
  --each-feature --exclude-all-features -p rumqttc-wrapper-core-next
cargo clippy --manifest-path native-wrappers/Cargo.toml \
  -p rumqttc-wrapper-core-next -p rumqttc-c-next --no-default-features --features readiness \
  --lib --tests -- -D warnings -W clippy::pedantic -W clippy::nursery
RUMQTTC_C_FEATURES=readiness native-wrappers/c/tests/abi/check.sh
ctest --test-dir native-wrappers/target/rumqttc-c-native \
  -R '(native-first-connection|native-readiness$|native-unload|example-readiness|example-event_polling)' \
  --output-on-failure
```

The resumed feature profiles used the same matrix command with
`--exclude-no-default-features` and the completed features excluded. After the
Ring timeout, the full `--no-default-features --features use-rustls-ring` suite
was rerun successfully; `--no-default-features --features websocket` completed
the final profile. Resumed-run logs are under
`native-wrappers/target/validation/todo46-*.log` (build artifacts, not committed).
