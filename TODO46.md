# C First-Connection Observer

Status: planned.

## Goal

Expose wrapper-core's repeatable first-successful-connection observation through
an independently owned C handle, without consuming application events.

## Foundation and scope

`ClientHandle::connection()` returns a cloneable `ConnectionHandle` with pending,
first success, and terminal-before-success states. Retryable attempt failures
leave it pending; later disconnects do not erase the first success. C currently
requires applications to build this observation from their single event consumer.

## Implementation requirements

- [ ] Add C acquisition, retain/release, poll, and timed-wait APIs backed by the
  existing connection cell. Return selected protocol and raw Session Present
  with explicit state. Use a distinct observer type rather than inventing a
  command, MQTT operation ID, or event-queue request.
- [ ] Add a core timed blocking observation primitive if needed; reuse the
  shared result and wakeups instead of allocating a runtime/thread per wait or
  repeatedly polling. Reject nonzero waits on execution workers and in callbacks;
  zero-time observation must remain nonblocking.
- [ ] Preserve first-success semantics, including MQTT 5 redirects and session
  recovery. Success is the native accepted connection boundary, not application
  event delivery, resubscription completion, or a currently-connected guarantee.
- [ ] Return the existing terminal error if shutdown/failure occurs before first
  success. Independent timeout ends only observation. Multiple waiters and later
  acquisitions observe the same immutable result without resetting it.
- [ ] Keep raw Session Present distinct from effective session resume. Reference
  existing CONNACK compatibility diagnostics; any added effective fields must
  be captured at that first connection rather than sampled from a later one.
- [ ] Retained observers survive client destruction and retain no client, runtime,
  callback authority, or event consumer unnecessarily. Define acquisition during
  closing and after termination. Integrate optional [TODO42.md](TODO42.md) readiness
  without making it a dependency for basic polling/timed waits.
- [ ] Preserve public C layouts and error-output conventions with additive APIs.

## Verification and completion

- [ ] Test both protocols with several failed attempts before success, terminal
  rejection/exhaustion, shutdown before connection, and success followed by loss.
- [ ] Verify multiple concurrent observers, timeout/retry, acquisition after
  completion, a full event queue, and retention after client destruction.
- [ ] Cover redirects, raw/effective Session Present compatibility, forbidden
  blocking contexts, output initialization, and unload ownership.
- [ ] Update header/exports, C startup example, READMEs, `PARITY.md`, and
  `CHANGELOG.md`; run core/C observer suites and ABI consumers.

Complete when every C observer can independently await and reread the first
successful connection or prior terminal failure without stealing connection events.
