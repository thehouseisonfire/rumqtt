# C Wrapper Ordered Publish Shutdown

## Goal

Expose the native optional publish-admission fence implemented by
`disconnect_after_queued*`, including tracked completion and MQTT 5 DISCONNECT
properties. Keep ordinary graceful and immediate close semantics distinct.

## Current foundation and feasibility

Both client crates provide this feature behind `ordered-shutdown` in
`src/client_disconnect.rs`. `docs/recipes/ordered-shutdown.md` defines its
ordering, deadline, reconnect, persistence, and failure contracts.
Wrapper-core deliberately omits it and does not forward the Cargo feature.

This is feasible by admitting the native fence and observing its notice.
Waiting for a wrapper snapshot of outstanding completions is not an equivalent
implementation: the fence must use the native successful-admission order.

## Implementation requirements

- [ ] Forward an explicit `ordered-shutdown` feature through wrapper-core and
  C packaging, and report it through the capability query.
- [ ] Add separately named ordered-disconnect commands and C admission APIs,
  including nonblocking, tracked, finite-deadline, and v5-properties forms.
- [ ] Delegate fence installation to the native client gate. Producers admitted
  before the fence are covered; later producers receive an explicit closing
  error. A full nonblocking admission installs no latent fence.
- [ ] Bridge the native `DisconnectNotice` to one owned wrapper operation.
  Distinguish fence admission, collective completion, DISCONNECT flush, terminal
  persistence cleanup, and execution-owner join.
- [ ] Preserve QoS milestones, successful-admission ordering across callers,
  negative-ACK failures, and the publish-only completion scope of the native
  contract. Do not claim covered subscriptions or independent inbound ACKs.
- [ ] Integrate ordered close with existing shutdown ownership and idempotent
  destruction. Define repeated calls, conflicting close modes/payloads, and
  immediate escalation without weakening an admitted fence.
- [ ] Preserve the absolute native deadline across reconnect and persistence.
  Separate observer timeout and capacity wait from the operation deadline.
- [ ] Continue required terminal persistence polling after timeout when native
  cleanup remains pending. A timeout must not become success or imply that
  previously flushed packets were undelivered.
- [ ] Preserve session-loss, redirect, reset, and ambiguous-delivery failure
  reasons. Coordinate recovery commands in [TODO36.md](TODO36.md).
- [ ] Add fence/phase observations through [TODO37.md](TODO37.md) without making
  outgoing DISCONNECT events substitutes for terminal notices.

## Verification and completion

- [ ] Use concurrent C producers and broker barriers to prove admission order,
  closing rejection, QoS 0/1/2 completion, and no latent fence on backpressure.
- [ ] Cover reconnect, negative ACKs, persistence failure, zero/expired deadlines,
  observer cancellation, escalation, and pending cleanup after timeout.
- [ ] Update feature packaging, C header/exports, examples, README,
  `PARITY.md`, root `CHANGELOG.md`, and the ordered-shutdown recipe.

Complete when C provides the native fence guarantee and ordinary close retains
its existing admitted-protocol-work drain contract.
