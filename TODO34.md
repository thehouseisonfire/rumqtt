# C Wrapper Reconnect Policy and Attempt Control

## Goal

Let C applications configure reconnect timing and retry budgets without
replacing the native MQTT event loop or manually reconstructing clients after
every transport failure. Deliver the core policy first; custom decisions and
host scheduling controls are follow-up work.

## Current foundation and feasibility

Rust consumers control when they next call `EventLoop::poll`. Wrapper-core's
`backend/v4.rs` and `backend/v5.rs` own polling and choose reconnect internally.
[TODO20.md](TODO20.md) already proposes application-facing reconnect policy;
share its vocabulary and any reusable native implementation that lands.

Before the core policy, drivers generally polled again immediately after connection errors.
Their terminal checks do not consistently follow the exposed `retryable` flag.
The legacy error mapping is insufficient as a reconnect classifier: missed
PING responses and some state-level I/O failures fall under `Protocol`, while
broker refusal reasons are uniformly marked nonretryable.

This is feasible as shared wrapper-core policy around connection establishment,
with protocol-specific error adapters. It must not become another packet
scheduler, replay engine, or protocol retry path. Keep direct Rust
`EventLoop::poll` policy ownership intact.

## Core policy: first release

Implemented in wrapper-core and the additive C API. Legacy remains the
unconfigured default; Python/JavaScript retain their existing public controls.
Live updates, custom callbacks, and host scheduling remain separate follow-ups.

- [x] Add one owned reconnect configuration for both protocols, with initial
  delay, maximum delay, multiplier, documented jitter distribution, finite or
  explicit unlimited retry budget, and reset-after-stability interval. Validate
  ranges and overflow; apply the maximum to the final jittered delay.
- [x] With no new policy configured, preserve existing reconnect behavior,
  including immediate retries and current terminal checks, through a documented
  legacy default. Make the new classified policy opt-in and recommend it in the
  retry example. Legacy behavior and stricter classification are different
  contracts; do not claim both preserve current retry decisions. Existing
  mandatory terminal failures remain terminal in either mode.
- [x] The first connection cycle starts immediately and does not consume the
  retry budget. Every subsequent cycle consumes one retry when it starts,
  including retries before the first successful connection. A zero budget
  therefore permits only the initial cycle. Success alone does not reset the
  budget or backoff: reset after an uninterrupted established connection reaches
  the configured stability interval. Count elapsed monotonic time even when no
  application events arrive. A zero stability interval resets on successful
  CONNACK; document that this permits indefinitely repeated short connections.
- [x] Count connection establishment cycles, not `poll` calls or emitted events.
  A cycle starts when native establishment begins and ends on successful CONNACK
  or establishment failure; draining buffered native events is not a new cycle.
  MQTT 5 redirect/SRV progression remains native-owned and may include several
  candidate dials in one cycle. Document that the wrapper budget does not bound
  every physical dial. Strict per-dial limits or delays require separate native
  integration; wrapper policy must not replace native redirect authority,
  fallback, session isolation, or redirect limits.
- [x] Audit native error variants, nested causes, broker reasons, and failure
  phases in both protocols. Do not derive reconnect eligibility solely from
  `ErrorKind` or the current `retryable()` flag. Distinguish transient DNS/connect
  and I/O failures, peer closure, missed PING responses, and temporary broker
  unavailability from malformed protocol data, invalid configuration, rejected
  authentication, permanent TLS verification failures, and store corruption or
  write failure. Under the classified policy, terminal classifications stop the
  driver rather than consume further retries. Publish the classification table
  and reconcile exposed retry metadata with the new policy, documenting
  legacy-mode differences.
- [x] Use one shared policy state machine with monotonic deadlines and
  injectable time/randomness. Keep protocol-specific error mapping and native
  event-loop ownership in their existing backends. Give each client independent
  policy state even when clients share an execution context.
- [x] Wait through cancellable driver timers while continuing diagnostics,
  operation observation, callback cleanup, and immediate shutdown. Do not use
  blocking sleeps on driver threads or shared runtime workers. Enter the wait
  after native failure cleanup; do not cancel and later resume a live native
  poll to insert backoff.
- [x] Separate per-attempt connection/handshake timeouts from absolute total
  deadlines. New attempts receive fresh per-attempt timeouts; existing graceful
  or ordered shutdown and any explicitly configured total connection deadline
  continue running through retry waits. Do not turn an observer's connection
  wait timeout into a total driver deadline. Authentication exchange deadlines
  retain their exchange scope; a retry must not extend an existing exchange.
- [x] Wake on shutdown admission and deadline expiry during backoff. Preserve
  ordinary graceful-close failure semantics and ordered-close recovery rules.
  On ordered expiry, drive the required native terminal cleanup rather than
  waiting for the backoff to finish or merely expiring a wrapper observer.
  Define precedence among mandatory terminal failure, shutdown, exhaustion,
  and a ready retry timer; no new attempt may start after terminal shutdown.
- [x] Preserve native pending state and notices across recoverable failures.
  Reconnect delegates replay and Session Present reconciliation to the client.
  Document existing bounded request admission and event-delivery overflow
  behavior during outages; backoff does not add an unbounded application queue
  or remove the requirement to drain events.
- [x] Add a machine-readable terminal exhaustion result distinct from an
  observer's wait timeout, preserving the last sanitized failure/context and
  attempt counts. Use existing terminal reconciliation to resolve unfinished
  operations and first-connection observers while retaining completed outcomes.
  Preserve ambiguous delivery where non-delivery cannot be proven; exhaustion
  must not imply that resubmitting a publication is safe.
- [x] Expose minimal owned retry observations: policy mode, attempt/cycle
  counters with documented meanings, last failure, stability/reset state,
  remaining delay at capture, and terminal stop reason. Preserve existing C
  record layouts using additive APIs under `docs/c-abi-compatibility.md`.
  Credentials and raw error source chains must not enter automatic diagnostics.
- [x] Coordinate next-attempt configuration activation with [TODO35.md](TODO35.md)
  and richer observations with [TODO37.md](TODO37.md), without requiring either
  full feature to deliver the core policy. Define whether configuration changes
  affect counters; they must not silently reset a spent budget.

## Optional follow-up: custom decisions and host scheduling

- [ ] Add an owned asynchronous decision registration only after demonstrating
  a use case beyond the core configuration. Provide typed sanitized failure
  context and attempt information, bounded decision time, cancellation, stale
  response rejection, exactly-once owner cleanup, and documented reentrancy.
  Host callbacks must return promptly and must not block shared runtime workers.
- [ ] Decisions may stop, delay, or retry only eligible failures. They cannot
  override mandatory terminal errors, a spent budget, total deadlines, shutdown,
  or native session/redirect reconciliation. Define timeout and abandonment
  behavior explicitly, with no implicit unlimited retry fallback.
- [ ] Add pause-next-attempt, resume, and request-attempt as serialized commands.
  Pause gates future cycles without cancelling an attempt already underway or
  pausing a healthy connection's keepalive, ACK, or AUTH processing. Resume
  removes the gate and honors the existing backoff deadline; request-attempt
  advances one eligible cycle, bypassing pause/backoff only as explicitly
  requested. Neither command resets or bypasses budgets or terminal decisions.
- [ ] Keep admission bounded while paused, keep absolute total deadlines active,
  and make commands rejected after terminal shutdown observable. If cancellation
  of an active attempt is later required, specify it as a separate transition
  that preserves native cleanup rather than overloading pause.

## Verification and completion

- [x] Verify backoff bounds, jitter, budgets, stable reset, and cancellation
  deterministically, including zero budget, pre-CONNACK failures, repeated short
  connections, arithmetic limits, and legacy default behavior. Then exercise
  broker loss/recovery with native C callers for both protocols.
- [x] Cover transient broker refusal, missed PING responses, authentication,
  protocol, TLS, and store failures; close during delay; retained pending work;
  stable terminal outcomes; and ordered deadline expiry across retries. Verify
  that retry metadata and actual decisions agree under the classified policy.
- [x] Verify MQTT 5 redirect/SRV candidate progression against the documented
  cycle budget and retained native redirect limits.
- [x] Demonstrate independent policies for multiple clients sharing a runtime.
  A client waiting in backoff must not stall another client's MQTT progress.
- [ ] For each optional follow-up, cover decision timeout/abandonment, stale and
  reentrant responses, owner release, pause/resume/request races, active-attempt
  behavior, shutdown while paused, and inability to override terminal limits.
- [x] Update C header/exports, README, retry example, `PARITY.md`, and root
  `CHANGELOG.md`; cross-reference the shared policy work in TODO20.

Core validation passed on Linux: full default and ordered-shutdown core/C
suites, all 19 core feature configurations for tests and strict production
Clippy, all 10 installed C feature profiles with native fixtures, C/C++ header
compilation, exported symbols, and compatibility with the previous C contract.
Behavioral coverage lives in `wrapper-core/tests/reconnect_policy.rs` and
`c/tests/native/native_reconnect.c` under `native-wrappers/`, alongside the
deterministic engine and classifier unit tests.

The core is complete when reconnect timing, eligibility, budgets, and terminal
consequences are configurable and observable without application ownership of
MQTT recovery state. Optional callbacks and scheduling controls have separate
completion criteria and do not block that release.
