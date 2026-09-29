# C Wrapper Reconnect Policy and Attempt Control

## Goal

Let C applications control reconnect timing, retry budgets, and recoverable
error decisions without replacing the native MQTT event loop or manually
reconstructing clients after every transport failure.

## Current foundation and feasibility

Rust consumers control when they next call `EventLoop::poll`. Wrapper-core's
`backend/v4.rs` and `backend/v5.rs` own polling and choose reconnect internally.
[TODO20.md](TODO20.md) already proposes application-facing reconnect policy;
share its vocabulary and any native implementation that lands.

This is feasible as a bounded wrapper policy around connection attempts. It
must not become another packet scheduler, replay engine, or protocol retry path.

## Implementation requirements

- [ ] Define one reconnect configuration for both protocols: initial/max delay,
  multiplier, jitter, finite or explicit unlimited attempt budget, and reset
  after a documented stable-connection interval. Preserve existing defaults.
- [ ] Use monotonic deadlines and injectable time/randomness for verification.
  Clarify whether the first connection counts against the reconnect budget.
- [ ] Classify native errors into retry-eligible and terminal failures. Store
  corruption/write failure, protocol violations, and rejected authentication
  must not be silently converted into endless transport retries.
- [ ] Optionally expose an owned prompt decision callback with typed failure
  context and attempt information. It may select stop, delay, or retry; it must
  not bypass session reconciliation or suppress a terminal shutdown.
- [ ] Provide explicit pause-next-attempt and resume/request-attempt commands
  where needed for host scheduling. Pausing retries does not pause a healthy
  connection's keepalive, ACK, or AUTH processing.
- [ ] Wait through cancellable driver timers while continuing diagnostics,
  operation observation, callback cleanup, and immediate shutdown. Do not use
  blocking sleeps on driver threads or shared runtime workers.
- [ ] Preserve absolute ordered-shutdown and connection/authentication deadlines
  across retry waits. A new attempt cannot reset an existing total deadline.
- [ ] Preserve native pending state and notices across recoverable failures.
  Reconnect delegates replay and Session Present reconciliation to the client.
- [ ] When retries stop, resolve every unfinished operation with defined
  delivery status and expose terminal policy exhaustion separately from an
  observer's wait timeout.
- [ ] Coordinate explicit endpoint/configuration changes in
  [TODO35.md](TODO35.md) and emit useful attempt observations through
  [TODO37.md](TODO37.md) without disclosing credentials.

## Verification and completion

- [ ] Verify backoff bounds, jitter, budgets, stable reset, and cancellation
  deterministically, then exercise broker loss/recovery with native C callers.
- [ ] Cover pause/resume, authentication/store failures, close during delay,
  retained pending work, and ordered deadline expiry across retries.
- [ ] Demonstrate independent policies for multiple clients sharing a runtime.
- [ ] Update C header/exports, README, retry example, `PARITY.md`, and root
  `CHANGELOG.md`; cross-reference the shared policy work in TODO20.

Complete when reconnect decisions and their consequences are configurable and
observable without application ownership of MQTT recovery state.
