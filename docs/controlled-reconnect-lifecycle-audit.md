# Controlled reconnect lifecycle audit

Audit date: 2026-10-09. Scope: the deferred extension in [TODO35](../TODO35.md),
both native protocols, and wrapper-core. This is a feasibility audit, not an
implementation or a formal verification of a future API.

## Finding

A narrow controlled reconnect is feasible. Existing native recovery provides
most of the required safety mechanisms, but no existing public operation
provides the whole transition. The extension cannot be implemented correctly
by cancelling a native poll, calling `clean()`, or using terminal disconnect.

The remaining guarantees can be established through a native-owned transition,
explicit wrapper admission and attempt ordering, and deterministic regression
tests. They are not established for controlled reconnect today. Recommend an
initial version that replaces an established origin connection, preserves
native recovery, and rejects concurrent reauthentication and redirected routes.
Defer draining all admitted operations and cancelling an establishment attempt.

## Existing mechanisms and remaining obligations

| Guarantee | Existing evidence | Obligation for controlled reconnect |
| --- | --- | --- |
| Work/notice ownership | Replay retains packet IDs and notices | Enter cleanup after request/read/flush completes |
| Durable recovery | Loss paths clean and checkpoint | Await save/clear; keep store failures terminal |
| Broker session state | CONNACK reconciles fresh/resumed state | Reuse reconciliation; promise no broker retention |
| Stale manual ACKs | Admission gate, generations, late drain | Invalidate before event delivery; retain producer gate |
| MQTT 5 aliases | Native repair and connection-scoped reset | Reuse cleanup, including late producer repair |
| AUTH owners | Guarded cancellation and host destruction | Initially exclude admitted reauthentication |
| Coherent profile | Prepared next-origin-attempt profiles | Order commands and retain the selected revision |
| Shutdown precedence | Gate, fence, absolute deadline | Recheck cutover/attempt start; resolve senders |
| Retry accounting | Cycle counts, budgets, backoff | Add an intentional transition; define budget use |
| Owner release/teardown | Guarded preparation and tracked work | Keep client/store lease; await owner destruction |

Source locations:

- [v4 event loop](../rumqttc-v4/src/eventloop.rs): `clean`,
  `handle_network_result`, `establish_connection_observed`, `select`, and
  `send_pending_disconnect`.
- [v5 event loop](../rumqttc-v5/src/eventloop.rs): `clean_with_notice_reason`,
  `checkpoint_after_connection_loss`, `handle_network_result`,
  `AuthenticationPollGuard`, `restore_redirect_origin`, and `poll_once`.
- [v4 state](../rumqttc-v4/src/state.rs) and
  [v5 state](../rumqttc-v5/src/state.rs): `clean_with_notices_for_reconnect`.
- [Wrapper handle](../native-wrappers/wrapper-core/src/handle.rs):
  `begin_connection`, `invalidate_connection`, ACK and reauthentication admission,
  and close admission.
- [Wrapper runtime](../native-wrappers/wrapper-core/src/runtime.rs):
  `wait_reconnect`, `deliver`, and `complete_shutdown`.
- [Configuration driver](../native-wrappers/wrapper-core/src/configuration_update.rs),
  [profile activation](../native-wrappers/wrapper-core/src/backend/configuration.rs),
  [retry controller](../native-wrappers/wrapper-core/src/reconnect.rs), and
  [store adapter](../native-wrappers/wrapper-core/src/backend/session.rs).

## Reproduced limits of existing APIs

Six exploratory tests ran against the public native APIs, three per protocol.
The local fixture and manifest are in the ignored
`target/todo35-verification/reconnect-probe/` directory; this directory is a local
audit artifact, not a checked-in test suite.

1. Establish a persistent session, send QoS 1 without its ACK, and make later
   store saves fail. Calling `clean()` neither calls nor awaits the store. A
   subsequent successful resumed handshake can complete without observing the
   failed loss checkpoint. This is consistent with `clean()` being synchronous;
   it is insufficient as a managed reconnect operation.
2. Run the same setup but close the peer transport normally. Native polling
   invokes the loss checkpoint and returns `ConnectionError::SessionStore`.
   This confirms the recovery path that the new transition must reuse.
3. Stall an underlying flush after a tracked QoS 0 publication reaches the
   peer. Drop the pending poll. The publication notice resolves with `Recv`,
   while diagnostics still show a connected transport and another poll can
   return a buffered outgoing event. Calling cleanup afterward cannot recover
   that already-destroyed notice sender or supply its missing flush outcome.

Terminal disconnect is also structurally unsuitable: both native
`send_pending_disconnect` paths discard unprocessed requests and set
`disconnect_complete`; ordered builds terminate admission as well. Later polls
return `RequestsDone`. A reversible reconnect must avoid these terminal actions.

## Smallest useful transition and its proof boundary

Use a distinct native control operation consumed inside the event loop. Its
presence must wake the existing connected select when keepalive is disabled.
It must not cause the wrapper to cancel an arbitrary pending poll. A control
wake cannot interrupt a request batch, network flush, checkpoint, or host
callback already being awaited; those operations must finish or take their
existing error/timeout path first.

The initial contract should end the current transport without waiting for all
queued and inflight application work to complete, then use ordinary native
recovery. This is a planned transport replacement, not a collective delivery
fence. Unsent work stays with the same event loop; replayable protocol state
keeps native notices and packet IDs. QoS 0 and fresh-session outcomes retain
their existing limits. A later drain mode needs its own admission sequence,
cutoff, deadline, and treatment of continuing producers.

For intentional disconnection, send a normal MQTT DISCONNECT at a safe boundary
using a nonterminal native path, then end the transport and normalize recovery.
Do not override MQTT 5 Session Expiry. A failed or timed-out write has an
uncertain peer outcome; it must not claim that the broker received DISCONNECT
or suppressed the Will. Define whether such a failure proceeds through ordinary
retry or fails the reconnect request, without rolling back the selected profile.

The ordering to establish is:

1. Admit a bounded reconnect request under the wrapper admission gate, excluding
   closing state and any admitted reauthentication. Prevent a new
   reauthentication from crossing this reservation. Permit at most one pending
   reconnect initially.
2. Serialize it with configuration updates on the driver. Select a committed
   profile revision only after relevant preparation finishes. Admission order
   across today's separate channels is not enough. Hold the selected prepared
   profile until the attempt consumes it, or delay later profile commits until
   that boundary; no unbounded revision history is needed.
3. At the native control boundary, check the actual connection route and phase
   again. Reject establishment/redirect transitions and redirected targets in
   the first version. A broker redirect racing admission must never cause origin
   credentials to be applied to its target.
4. Complete the native transport-end and recovery transition, including the
   checkpoint. Yield a distinct result before starting another connection.
   This gives the wrapper a safe boundary to invalidate ACK tokens, update
   observations, and apply the selected origin profile. Update these internal
   states before awaiting application event delivery.
5. Recheck closing state and retry eligibility at the actual attempt-start
   boundary. The attempt-start decision and shutdown admission need a defined
   order; merely checking an atomic flag earlier leaves a race. A reconnect
   request must never undo closing state. Ordered shutdown may still use its
   existing recovery attempts to finish its fence.
6. Retain the selected profile across retryable failure. Distinguish admission,
   transport-transition completion, attempt start, and successful connection.
   Observer cancellation or timeout does not retract admitted work. Terminal
   paths resolve every reconnect sender before waiting for registered futures.

The retry controller currently treats `Connected`/`Attempting` as already started
and creates waiting state only through `failed`. It needs an explicit intentional
transition, not a fabricated network error. Choose whether a requested cycle
consumes the existing retry budget or uses a separately documented allowance;
in either case, never reset a spent budget implicitly. Check eligibility before
cutting a healthy connection so rejection does not unnecessarily take it down.

## Guarantees that must stay conditional

Safety can require exactly-once local completion, coherent profile selection,
native recovery, no stale-token reuse, no new reconnect after shutdown wins, and
teardown completion only after owned work is destroyed. Successful connection
and a fixed wall-clock completion bound cannot be unconditional promises.

Wrapper store/authentication/transport adapters have configured timeouts, and
event delivery has a finite timeout. However, a host callback that blocks during
invocation, polling, or destruction cannot be preempted by an async timer. The
native store trait itself has no timeout. Event backpressure can terminate the
client rather than permit reconnection. A join timeout cannot mean that
outstanding callbacks or detached blocking preparation have been destroyed.

There is also a protocol-specific progress gap: v4 wraps normal network flushes
in its network timeout, while v5 `flush_network` awaits the framed flush without
a native flush deadline. A stalled v5 socket or custom flush can therefore keep
the native poll away from the control-consumption boundary. Waking an idle read
is achievable; promising a bounded cutover during arbitrary I/O requires more
native work. A command timeout must expire or withdraw the queued transition
without cancelling that poll and later resuming it. If timeout intentionally
terminates the client instead, expose that consequence explicitly.

The MQTT specs impose additional limits. Session Present depends on broker
state; lost/fresh sessions require reconciliation. QoS replay does not make
every channel-admitted request a durable application outbox. MQTT 5 aliases are
connection-scoped. Will suppression depends on the broker receiving the proper
DISCONNECT. See the checked-in [MQTT 3.1.1](spec/mqtt-v3.1.1.md) and
[MQTT 5](spec/mqtt-v5.0.md) references, especially session/CONNACK rules,
`MQTT-4.4.0-1`, MQTT 5 `MQTT-3.3.2-7`, and DISCONNECT/Will rules.

## Validation and release gate

Linux baseline checks passed:

- Native cleanup unit tests with no default features: v4 22, v5 36.
- Native ordered-disconnect integration tests: v4 12, v5 17.
- Native async-authentication integration tests: v5 5, including cancellation
  at Start, Continue, and Success and the original handshake deadline.
- Native partial-flush replay regression: v4 1. The identically named filter
  selected no v5 tests; v5 flush cancellation was exercised by the local probe.
- Wrapper integration tests without default features and with ordered shutdown:
  authentication 28, configuration updates 10, manual ACKs 3, ordered shutdown
  14, reconnect policy 8.
- Wrapper configuration integration tests with default Rustls/WebSocket features
  and ordered shutdown: 16, including TLS owner release, redirect retirement,
  event backpressure, and concurrent shutdown.
- Wrapper unit tests with the same features: 91, including preparation-owner
  destruction tracking and queued/preparing updates during shutdown completion
  draining.
- Exploratory public-API probes: 6.

These checks establish recovery foundations and rule out unsafe shortcuts. They
do not substitute for exercising a new reconnect command. Before exposing it,
require deterministic tests for the following boundaries in both protocols:

- Idle keepalive-zero wakeup, request flood fairness, and full event queues.
- Cutover during partial writes, flushes, store saves/clears, and QoS 0/1/2
  flows, including PUBREL, subscribe/unsubscribe notices, and clean/fresh versus
  resumed sessions. A failed checkpoint must not allow another dial.
- Manual ACK admission immediately before cleanup, during checkpointing, and
  after the new CONNACK; no old token or queued ACK may affect the new generation.
- MQTT 5 alias admission/rebinding during cleanup; queued and active AUTH
  exclusion; redirect admission racing reconnect, including temporary origin
  restoration and permanent-target retirement.
- Profile preparation in progress, a superseding update after reconnect
  admission, and a second update before the next attempt. Assert the peer sees
  exactly the revision the reconnect operation reports selecting.
- Shutdown winning before cutover, during checkpointing, before attempt start,
  and during backoff; ordered expiry must retain its original absolute deadline.
- Retry-budget exhaustion, finite observer waits, cancelled observers, callback
  destruction failures, blocked owner destruction, and detached preparation on
  shared execution. Every admitted operation must resolve once, and join must
  keep waiting for retained work.

Run the feature matrices and C ABI/consumer checks after the native/core command
is implemented. Cross-platform execution remains a separate verification need.
Leave TODO35's controlled-reconnect checkboxes open until this release gate is
satisfied.
