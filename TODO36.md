# C Wrapper Explicit Session Abandonment and Fresh Recovery

## Goal

Expose one tracked, driver-owned operation that abandons the current session
and establishes a fresh broker session. Resolve discarded work explicitly and
preserve honest delivery outcomes. This is operator recovery, not ordinary
reconnect or cancellation of effects already observed by the broker.

## Current foundation and feasibility

Both event loops provide `reset_session_state`, `drain_pending_as_failed`, and
manual-ACK cleanup hooks, but these are building blocks, not complete recovery
transactions. Drain covers replay and scheduler queues, not request/control
channels or active protocol ownership. Reset marks store clearing pending; its
return does not prove durable clearing. The wrapper preserves active native
poll futures because they can mutate protocol state before awaiting I/O.

The first implementation accepts recovery only for a running, disconnected
client, including one waiting to reconnect. Connected, closing, closed, and
failed clients are rejected without side effects. Disconnected status alone
does not prove quiescence: an attempt or store callback may still be active.
Add a safe native transition before exposing the C operation.

Persistence failures currently terminate the wrapper driver. This operation
does not revive terminal clients or automatically erase unreadable checkpoints.
Those cases require explicit store repair/administration and a new client; see
[TODO39.md](TODO39.md). Routine ACK/alias cleanup remains managed automatically.

## Implementation requirements

- [x] Audit the native recovery methods against persistence, active transport,
  request receivers, control queues, ordered shutdown, ACK ownership, and alias
  admission. Consult both local MQTT specs and their requirement indexes.
- [x] Define one typed abandonment-and-fresh-recovery command. Its scope includes
  all unfinished MQTT work admitted before the recovery barrier: replay,
  scheduler, request/control channels, outbound protocol state, incoming QoS 2,
  and affected ACK/AUTH operations. Preserve already terminal outcomes and
  unrelated observers. Do not expose independent reset/drain primitives.
- [x] Serialize recovery admission with producers, connection establishment,
  and shutdown admission. Close MQTT admission and suspend automatic reconnect
  before committing abandonment. Concurrent producers must either belong to
  the discarded set or receive a typed, definitely-not-admitted result.
  Asynchronous producers may wait for recovery but must not enqueue across it.
- [x] Establish driver quiescence through an audited native transition. Safely
  finish or explicitly abort any active attempt/poll and retain callback work
  until cancellation/cleanup is observed. A wrapper control wakeup must not
  merely drop a poll future whose protocol or persistence effects are pending.
- [x] Require transport teardown before discarding protocol ownership. Prevent
  new packet-ID or alias ownership throughout reset; never reuse identifiers
  against the abandoned broker session. Reject connected recovery initially;
  a future connected variant must explicitly define controlled abort and Will
  effects rather than silently disconnecting.
- [x] Resolve affected publish/subscribe/unsubscribe and ACK/AUTH notices exactly
  once with explicit reset/discard reasons. Preserve native transmission history
  and release publish reservations exactly once. Definitely unsent work may be
  rejected; previously transmitted work remains ambiguous and must not be
  retried automatically. Abandonment cannot undo broker effects.
- [x] Invalidate relevant acknowledgement tokens and connection generations.
  Delegate alias invalidation and repair to the managed client; never maintain
  a second map or expose raw state, packet-ID mutation, or deprecated hooks.
- [x] Complete native store clearing/checkpoint obligations before reconnecting.
  Retain their owners and observe outcomes; failed or cancelled clearing must
  block reload of the abandoned checkpoint and reopening of MQTT admission.
  Define bounded retry or safe terminal failure without reviving failed clients.
  Document required store administration before a replacement client starts
  when clearing was not completed. Never report success at local reset return.
- [x] Own an explicit fresh-session handshake policy within the recovery
  transition; local reset alone does not erase broker state. For MQTT 5, use
  Clean Start with the intended session expiry. For MQTT 3.1.1, distinguish a
  clean session from recovery back into persistent mode: if persistent mode is
  retained, complete the clean-session connect/disconnect and subsequent
  persistent connect before releasing traffic. Validate fresh-session CONNACK
  semantics at each required boundary using the existing compatibility policy.
- [x] Preserve configured identity, store scope, protocol, and long-term session
  policy. Specify transient CONNECT overrides and failed-attempt retries so an
  incomplete broker reset cannot accidentally resume abandoned state. Keep the
  native transition independent of arbitrary live configuration updates in
  [TODO35.md](TODO35.md); do not repeatedly reset an established fresh session.
- [x] Add additive C admission/completion APIs with retained observers and a
  bounded control path responsive during reconnect waits. Success requires
  completed local/durable cleanup and establishment of the intended fresh
  session; expose phase and failure information so partial progress is honest.
  Observer timeout/drop does not cancel or roll back committed abandonment.
- [x] Reject recovery during committed graceful/ordered/immediate shutdown and
  reject overlapping recovery initially. Define shutdown admitted during recovery
  as an interruption with exactly-once recovery completion and retained cleanup;
  never report fresh-session success or reopen admission after shutdown wins.
- [x] Keep automatic reconnect recovery unchanged. Document operator recovery
  separately from normal cleanup and file-store administration in
  [TODO39.md](TODO39.md).

## Deferred scope

Selective replay discard is not part of the first implementation. Resuming a
session requires retransmission of unfinished QoS PUBLISH/PUBREL exchanges;
dropping those exchanges is session abandonment, not safe per-message
cancellation. Any later selective operation must either participate in fresh
session recovery or be restricted to work proven never transmitted and owning
no unfinished protocol exchange, with audited packet-ID and checkpoint updates.

## Verification and completion

- [x] Exercise abandonment with mixed queued, replayed, inflight, incoming QoS 2,
  and persistent work for both protocols. Verify queue scope, native notice and
  reservation reconciliation, stale ACK/alias rejection, and ambiguous delivery.
- [x] Verify MQTT 5 fresh recovery and MQTT 3.1.1 clean/persistent recovery on the
  wire, including unexpected Session Present, failed intermediate handshakes,
  resubscription needs, and absence of abandoned replay after restart.
- [x] Cover active attempts/polls, delayed callbacks, failed/cancelled store
  clearing, concurrent admission, reconnect races, shutdown interruption,
  observer timeout/drop, overlapping requests, and terminal-client rejection.
- [x] Prove rejected connected/closing recovery changes no state, ordinary
  reconnect still retains unfinished exchanges, and no traffic is admitted
  between abandonment and completed fresh-session establishment.
- [x] Update C header/exports, README, operator example, `PARITY.md`, and root
  `CHANGELOG.md`; document prerequisites, partial-failure handling, discarded
  subscriptions/session state, unsupported hooks, and deferred selective discard.

Complete when C can explicitly abandon a running disconnected client's session
and observe fresh-session recovery without violating native ownership, reloading
abandoned durable state, or hiding discarded operations and durable failures.

## Implemented contract

- Wrapper-core: `Command::RecoverSession`, `Completion::SessionRecovered`, retained
  `RecoverySnapshot` and typed `RecoveryFailure`. Recovery and producer/shutdown
  publication share one admission barrier; native establishment rechecks committed
  shutdown at each handshake boundary. Intermediate connections stay hidden.
- C: nonblocking operation-ID and tracked admissions, completion kind 13,
  `RUMQTTC_SESSION_RECOVERY_SNAPSHOT_INIT`, progress and error accessors. Existing
  statuses and record layouts are retained. No JavaScript/Python public API is added.
- Clearing failure is immediately terminal. Active native polls finish under their
  existing deadlines. Fresh establishment uses existing reconnect delays and the
  remaining retry budget; there is no extra free attempt or operation deadline.
  Redirect profiles are rejected during recovery before another scope is applied.
- Evidence: native queue/control/protocol ownership tests for both versions;
  `wrapper-core/tests/session_recovery.rs` covers wire policy, inflight/incoming QoS 2,
  persistent restart, clear failure/cancellation, producer races, observer lifetime,
  retry budget, redirects and shutdown. Native C recovery/clear-failure fixtures and
  the operator example run through the package profile harness. README, parity and
  root changelog document the operator and store-administration obligations.
