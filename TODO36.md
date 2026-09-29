# C Wrapper Explicit Session Recovery Commands

## Goal

Expose intentional recovery actions such as resetting local session state and
failing selected pending replay work through tracked, driver-owned commands.
Keep destructive recovery explicit and preserve honest delivery outcomes.

## Current foundation and feasibility

Both event loops provide `reset_session_state`, `drain_pending_as_failed`, and
manual-ACK cleanup hooks. MQTT 5 additionally has an older external topic-alias
repair hook. The wrapper's managed generation and admission machinery already
performs routine ACK/alias cleanup automatically.

Explicit recovery is feasible only at audited disconnected or quiescent
boundaries. Do not expose raw `MqttState`, packet-ID mutation, or deprecated
repair hooks that compete with the managed native path.

## Implementation requirements

- [ ] Audit the native recovery methods against persistence, active transport,
  request receivers, control queues, ordered shutdown, ACK ownership, and alias
  admission. Consult both local MQTT specs and their requirement indexes.
- [ ] Define separate typed operations for failing retained replay work and
  resetting a session. State exactly which queues and native notices each
  operation covers; do not describe a replay drain as all queued-work removal.
- [ ] Add a driver quiescence/admission barrier before destructive actions.
  Ensure no producer can acquire packet-ID or alias ownership against state
  being discarded. Add a native transition API first if existing hooks cannot
  establish that boundary safely.
- [ ] Require transport teardown before discarding active protocol ownership.
  Define whether recovery is rejected while connected or includes an explicit
  controlled abort. Never reuse packet identifiers on the same live session.
- [ ] Reconcile broker session semantics after reset: resetting local state
  alone does not erase broker state. Require a fresh session or another explicit
  native reconciliation policy before resumed traffic can proceed.
- [ ] Retain store clearing/checkpoint work until its outcome is observed.
  Failed clearing blocks unsafe reload/reconnect; do not erase corrupt data
  automatically or report reset success before durable obligations finish.
- [ ] Resolve affected publish/subscribe/unsubscribe, AUTH, and shutdown notices
  exactly once with explicit reset or discarded-work reasons. Already flushed
  data may have been delivered; draining is not cancellation of broker effects.
- [ ] Invalidate relevant acknowledgement tokens and connection generations.
  Delegate alias repair to the managed client; never maintain a second map.
- [ ] Add additive C recovery admission/completion APIs with no unsafe raw state
  access. Define retained observers, timeout, retry, and shutdown interactions.
- [ ] Keep automatic reconnect recovery unchanged. Document operator recovery
  separately from normal cleanup and file-store administration in
  [TODO39.md](TODO39.md).

## Verification and completion

- [ ] Exercise reset/drain with mixed queued, replayed, inflight, incoming QoS 2,
  and persistent work, including stale ACK and topic-alias attempts.
- [ ] Cover broker Session Present mismatches, store-clear failure, concurrent
  admission, reconnect, ordered fences, and ambiguous previously flushed data.
- [ ] Update C header/exports, README, operator example, `PARITY.md`, and root
  `CHANGELOG.md`; document unsupported unsafe native hooks explicitly.

Complete when recovery can be requested from C without violating native
session ownership or hiding discarded operations and durable failures.
