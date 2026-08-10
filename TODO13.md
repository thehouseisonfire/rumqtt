# Ordered Publish-Queue Disconnect Barrier

## Decision

Add an explicit ordered graceful-disconnect operation to both
`rumqttc-v4-next` and `rumqttc-v5-next`. The operation is a fence in the normal
publish request stream: every publish admitted before the fence must reach its
documented protocol completion milestone before MQTT `DISCONNECT` is written
and flushed.

Do not implement this by only routing the existing `Request::Disconnect` onto
the normal channel. The outbound scheduler currently allows ready control
requests to bypass blocked flow-controlled publishes. The new request must have
dedicated, non-bypassable fence semantics from client admission through event-
loop scheduling.

Keep the current low-latency graceful disconnect available. It intentionally
drains only work already admitted into MQTT protocol state and may overtake
channel-accepted publishes that flow control has not admitted. Breaking API
changes are permissible, but the three useful policies must remain explicitly
selectable:

- **drain admitted work**: the existing graceful behavior;
- **drain preceding queued publishes**: the new ordered behavior; and
- **disconnect immediately**: the existing `disconnect_now` behavior.

Use the same names and semantics in v4 and v5. Prefer names that describe the
ordering boundary, such as `disconnect_after_queued`, over another use of the
ambiguous word `graceful`. Final naming may be adjusted consistently during the
API review.

## Required guarantee

For a normally builder-created client, after an ordered-disconnect call has
successfully enqueued its fence:

1. every publish whose enqueue linearized before the fence remains before it;
2. no such publish may be bypassed by the fence because it is temporarily
   blocked by the outgoing inflight limit, packet-identifier availability,
   pending throttling, batching, or scheduler readiness;
3. QoS 0 publishes complete after their bytes have been successfully flushed
   through the transport contract used by the existing tracked-publish API;
4. QoS 1 publishes complete on successful receipt and processing of `PUBACK`;
5. QoS 2 publishes complete on successful completion of the exchange through
   `PUBCOMP`;
6. only after all preceding publishes have completed does the event loop write
   and flush MQTT `DISCONNECT`;
7. no application request ordered after the fence is executed on that
   connection; and
8. terminal success is reported only after the DISCONNECT flush succeeds.

“Before” and “after” refer to the library's admission linearization order, not
task spawn order or wall-clock intent. Concurrent callers using cloned clients
must receive one well-defined order. Document this explicitly.

This is not a durable application outbox or an end-to-end broker-consumption
guarantee. In particular, QoS 0 has no broker acknowledgement. A successful
QoS 0 milestone proves only the same transport flush currently promised by
`PublishNotice`.

## Scope

The required fence covers all preceding `Request::Publish` operations,
including tracked and untracked QoS 0, 1, and 2 publishes, replayed publishes
that precede the fence, and QoS 2 `PUBREL` work belonging to those publishes.

Do not accidentally claim ordering for independent inbound acknowledgements,
post-CONNACK MQTT 5 authentication, or arbitrary requests supplied to a bespoke
receiver. Decide during implementation whether preceding subscribe and
unsubscribe operations are also included. Including them is acceptable only if
the public contract clearly states their `SUBACK`/`UNSUBACK` milestones and the
scheduler enforces the same fence ordering. The minimum required and named
contract remains a publish-queue fence.

The operation applies to both async and blocking clients and must have
nonblocking `try_*` counterparts where the current disconnect APIs do.

## Public API

### 1. Preserve distinct shutdown policies

Expose three unambiguous policies. One acceptable shape is:

```rust
// Admission-only, low-latency graceful shutdown of protocol-admitted work.
client.disconnect().await?;

// Ordered publish fence; returns a completion handle.
let completion = client.disconnect_after_queued().await?;
completion.wait_async().await?;

// Priority shutdown without draining publish handshakes.
client.disconnect_now().await?;
```

Add timeout and `try_*` forms consistently:

```rust
disconnect_after_queued()
disconnect_after_queued_with_timeout(Duration)
try_disconnect_after_queued()
try_disconnect_after_queued_with_timeout(Duration)
```

The async client methods wait only for channel admission, matching
`publish_tracked`; they return a `DisconnectNotice` which separately awaits the
terminal result. Blocking clients return the same notice and provide its
blocking wait method. Mark the notice `#[must_use]`.

If retaining the existing admission-only return type is judged more consistent,
also provide an explicitly tracked form. Do not ship the feature without some
direct way for the initiating caller to learn whether the ordered barrier
actually completed; observing an event loop in another task must not be the only
option.

### 2. Completion result and errors

Add a common v4/v5 completion type. The exact names may follow the established
notice module, but it must distinguish at least:

- DISCONNECT written and flushed successfully;
- the total ordered-shutdown deadline expired;
- connection/transport failure before completion;
- MQTT state or protocol failure while completing preceding work;
- session persistence failure where persistence is required;
- shutdown superseded by `disconnect_now`;
- shutdown superseded by another terminal operation;
- event-loop/request receiver termination; and
- client/session reset or redirect where applicable.

Do not reduce all failures to a dropped one-shot sender. Formatted error strings
are diagnostic, not stable identifiers. Preserve the underlying
`ConnectionError`, MQTT 5 reason code, or another typed cause where doing so is
consistent with the existing public error taxonomy.

Successful channel admission is not successful disconnect. Method and type
documentation must maintain that distinction.

### 3. Request representation

Use a distinct public request variant for low-level request sinks, for example:

```rust
Request::DisconnectAfterQueued(Disconnect)
Request::DisconnectAfterQueuedWithTimeout(Disconnect, Duration)
```

MQTT 5 variants must carry the chosen reason code and properties without loss.
Do not infer ordered semantics from which channel happened to carry an existing
`Request::Disconnect`; the policy must survive request capture, logging, and
future channel refactoring.

Clients created with `from_senders` have no rumqttc-managed event loop, protocol
state, acknowledgement tracking, or flush completion. They may enqueue the new
public request in FIFO order, but rumqttc cannot issue a truthful
`DisconnectNotice` for them. Return `ClientError::TrackingUnavailable` from an
API that promises managed completion, or provide a clearly admission-only
low-level counterpart. Never claim that the external receiver honored the
barrier merely because it received the variant.

## Admission and concurrency

Define one admission linearization point shared by cloned client handles and by
sync, async, blocking, and `try_*` operations.

When the ordered fence successfully linearizes:

- publishes already accepted on the normal request stream precede it;
- later normal publishes cannot appear before it;
- later control requests cannot bypass it and execute on the closing
  connection; and
- new ordinary operations fail with an explicit closing/closed client error,
  or are accepted only into a documented next-session facility. Silently
  executing them before DISCONNECT is forbidden.

The admission gate and fence enqueue must be cancellation-safe. In particular,
an async call cancelled while waiting for bounded-channel capacity must not
leave the client permanently marked as closing unless the fence was actually
enqueued. Conversely, once the fence is enqueued, cancellation or dropping its
notice must not cancel shutdown.

It is acceptable to introduce shared lifecycle/admission state or sequence
numbers, and breaking changes are allowed. Do not rely on timing between two
independent Flume receivers to define cross-lane ordering.

For `try_disconnect_after_queued`, a full channel must leave the client open and
return the recoverable request in the existing `ClientError::RequestChannelFull`
style. The caller must be able to retry without a latent fence or closed gate.

## Scheduler and event-loop semantics

### 1. Add a real fence class

Extend the scheduler model with an explicit barrier/fence concept. Required
properties:

- the fence is never selected while any earlier covered request remains in the
  scheduler, pending/replay queue, or normal request receiver;
- a ready fence cannot use the current `Control` bypass rule to pass a blocked
  flow-controlled publish;
- later control traffic cannot pass the fence;
- control traffic required to complete an earlier publish, including outgoing
  QoS 2 `PUBREL`, is not blocked by the fence;
- incoming network acknowledgements continue to be read while a preceding
  publish is flow-controlled or in flight; and
- request batching stops at the fence and never admits a later request into the
  closing generation.

Add scheduler-level tests independent of the MQTT event loops. At minimum,
cover a fence behind one and several blocked publishes, controls needed by
earlier work, a later ready control, and multiple fences.

### 2. Separate pre-fence progress from drain

The event loop has two ordered-disconnect phases:

1. **Approach the fence:** continue normal admission and protocol progress for
   covered requests preceding it. This phase may need network reads and may
   admit additional preceding publishes as inflight slots become available.
2. **Drain protocol state:** after the fence reaches the front, stop application
   admission and wait for the preceding work that is now protocol-admitted to
   reach its completion milestones. Then send and flush DISCONNECT.

Do not enter the existing `pending_disconnect` state while a preceding publish
is still only in a client/event-loop queue. That would recreate the current
gap.

### 3. Later requests

Once the fence has linearized, later application requests must have a
deterministic disposition. Prefer rejecting them at the client admission gate.
Any request already racing in a channel must be identified as after-fence and
dropped without execution.

Resolve tracked notices for discarded work with an explicit shutdown reason,
such as `DiscardedAfterDisconnectBarrier`, rather than the generic receiver-
dropped error. Apply this to publish, subscribe, unsubscribe, and MQTT 5 auth
notices as relevant.

`disconnect_now` may supersede an ordered fence. It must complete its priority
behavior, fail the ordered `DisconnectNotice` explicitly, and fail any affected
tracked work explicitly.

## Timeout policy

The ordered timeout is a **total shutdown deadline** measured from successful
fence admission. It covers:

- waiting for the event loop to receive/admit the fence;
- waiting for flow-control capacity;
- transmitting all preceding publishes;
- waiting for their MQTT completion milestones;
- persistence needed for the terminal transition; and
- writing and flushing DISCONNECT.

This deliberately differs from the current graceful timeout, which starts when
the event loop observes its control request. Use an absolute monotonic deadline
stored with the ordered fence so channel and scheduling delays cannot reset or
extend it.

On expiry:

- do not claim success and do not claim that preceding publishes were not
  delivered;
- attempt no MQTT DISCONNECT unless the complete ordered precondition was
  already satisfied and only the bounded DISCONNECT flush is in progress under
  a separately documented rule;
- close/abandon the active transport consistently with terminal timeout
  semantics;
- resolve the ordered notice with `DisconnectTimeout` or a more specific
  ordered-shutdown timeout;
- fail later discarded notices explicitly; and
- preserve or clear replay/persisted session state according to MQTT session
  expiry and the existing persistence contract.

A zero timeout must have documented poll/immediate-expiry semantics. Reject
duration overflow. The no-timeout form may wait indefinitely for a broker that
never acknowledges QoS 1/2; state this plainly and recommend the timeout form
for application shutdown.

## Reconnection, replay, and persistence

An ordered shutdown is client-lifecycle intent, not an ordinary MQTT packet to
blindly replay. Define and test the following behavior:

- a recoverable connection loss before the fence is reached preserves the
  fence and its relative position after all preceding replayable publishes;
- the event loop may reconnect to finish preceding QoS work when the configured
  session/reconnect policy makes that possible and the total deadline has not
  expired;
- no later application request is admitted into that shutdown generation while
  reconnecting;
- a clean-session reset, MQTT 5 redirect, zero session expiry, unrecoverable
  protocol error, or missing replay information that makes the guarantee
  impossible fails the ordered notice explicitly rather than sending a
  misleading successful DISCONNECT; and
- persisted MQTT session state does not become a claim that every
  channel-accepted publish is a durable application outbox entry.

If preserving a fence across reconnection cannot be made truthful for a
specific configuration, fail it with a typed reason. Do not silently downgrade
to the current admitted-only disconnect.

Persist enough internal ordering metadata to restore a fence relative to work
that the session store already promises to restore, or explicitly document and
enforce that an ordered fence is process-local and fails on process/event-loop
loss. Do not serialize completion senders or expose internal channel details in
the persistence format.

## MQTT protocol considerations

- MQTT requires DISCONNECT to be the final MQTT control packet on the
  connection. Once it is written, no later queued request may be encoded.
- QoS 1 success means a successful PUBACK completion. For MQTT 5, a PUBACK with
  an error reason code must not be reported as successful publish delivery.
- QoS 2 success means the exchange completed through a successful PUBCOMP.
  Handle negative PUBREC/PUBCOMP reason codes consistently with
  `PublishNotice::wait_completion`.
- QoS 0 success remains local transport flush only.
- MQTT 5 DISCONNECT reason code and properties supplied by the caller must be
  preserved exactly, subject to existing validation.
- A graceful DISCONNECT suppresses the Will only if it is actually received and
  processed by the server. A local flush is the strongest client-side claim;
  documentation must not promise server receipt.
- Timeout or transport failure may produce ambiguous delivery. Surface that
  ambiguity rather than describing all preceding messages as failed.

## Notices, events, and diagnostics

Emit the existing outgoing DISCONNECT event only when the packet has actually
entered the normal outgoing state transition. Resolve `DisconnectNotice`
after the subsequent transport flush succeeds, not merely when that event is
queued.

Add diagnostics sufficient to distinguish:

- no shutdown requested;
- admitted-work graceful drain;
- approaching an ordered fence;
- draining after an ordered fence;
- DISCONNECT flush in progress;
- ordered shutdown completed;
- ordered shutdown timed out; and
- ordered shutdown superseded or failed.

Expose the count of covered requests still ahead of the fence where this can be
done without an expensive or misleading snapshot. Do not represent racy
channel lengths as an exact completion count.

Tracing/logging should include the shutdown policy, fence admission sequence or
generation, total deadline, phase, remaining queued work, outbound protocol
diagnostics, and terminal reason. Avoid payload or credential logging.

## Required tests

Mirror behavioral tests in v4 and v5 unless the behavior is genuinely
protocol-specific.

### Unit and scheduler tests

- [ ] A fence cannot pass a blocked QoS 1 or QoS 2 publish.
- [ ] Multiple preceding blocked publishes progress in FIFO order as capacity
      becomes available.
- [ ] QoS 0 preceding the fence is flushed before DISCONNECT.
- [ ] QoS 2 PUBREL can progress although the fence blocks later application
      work.
- [ ] A control request ordered after the fence cannot bypass it.
- [ ] Request batching stops at the fence.
- [ ] A failed `try_*` admission leaves no fence and does not close the client.
- [ ] Cancelling an async fence admission before enqueue leaves the client
      usable.
- [ ] Dropping `DisconnectNotice` does not cancel shutdown.
- [ ] Later tracked notices receive an explicit discarded-after-barrier error.
- [ ] `disconnect_now` supersedes and explicitly fails an ordered notice.
- [ ] Two ordered-disconnect calls have deterministic first-wins behavior and
      the second receives a typed result.

### Broker/reliability tests

- [ ] Rework the existing “graceful disconnect does not wait for unsent
      flow-controlled publish” test to retain coverage for the current
      admitted-only policy.
- [ ] Add its ordered counterpart with inflight capacity 1 and request batch 1;
      the broker must observe publish 1, acknowledge it, observe publish 2,
      acknowledge it, and only then observe DISCONNECT.
- [ ] Repeat with a backlog larger than both request-channel capacity and
      outgoing inflight capacity.
- [ ] Cover mixed QoS 0/1/2 publishes and verify each completion boundary.
- [ ] Verify that no later publish or control packet follows DISCONNECT.
- [ ] Verify MQTT 5 negative PUBACK, PUBREC, and PUBCOMP handling.
- [ ] Verify timeout while the fence is waiting behind an unacknowledged
      publish; the deadline must include time before fence observation.
- [ ] Verify timeout while waiting for bounded-channel admission, with the
      documented distinction that the deadline starts only after successful
      admission.
- [ ] Verify transport failure during preceding publish transmission and during
      DISCONNECT flush.
- [ ] Verify reconnect/replay preserves fence order or returns the explicitly
      documented typed failure for configurations where it cannot.
- [ ] Verify clean-session/session-expiry and session-store behavior.
- [ ] Verify graceful ordered shutdown suppresses a configured Will in the
      normal broker-backed case without claiming suppression on local timeout.
- [ ] Verify cloned-client races against the fence using controlled admission
      synchronization rather than timing sleeps.
- [ ] Verify sync, async, blocking, and `try_*` APIs.
- [ ] Verify `from_senders` exposes only the documented low-level semantics and
      never returns a false managed-completion guarantee.

Prefer deterministic in-process broker tests for ordering and timeout edges,
plus Mosquitto smoke coverage for on-wire terminal behavior.

## Documentation and migration

- [ ] Update `CHANGELOG.md` as a user-facing v4/v5 API addition or breaking
      shutdown API clarification.
- [ ] Update rustdoc for every disconnect method with a compact comparison of
      admitted-only, ordered, and immediate policies.
- [ ] State separately what method return, notice completion, outgoing event,
      QoS milestone, and transport flush each prove.
- [ ] Add a shutdown recipe showing a publish burst followed by
      `disconnect_after_queued_with_timeout` and notice waiting.
- [ ] Explain cloned-client concurrency and require producers to handle the
      explicit closing error after fence admission.
- [ ] Explain that `publish_tracked` remains preferable when callers need an
      individual result for each publish, especially MQTT 5 negative reason
      codes.
- [ ] Explain that the ordered fence is preferable when callers need one
      collective shutdown boundary for previously admitted publishes.
- [ ] Document `from_senders` limitations without implying that receipt of a
      request variant proves MQTT execution.

If method names or existing behavior change, provide a migration table in the
changelog. Avoid leaving `disconnect()` described merely as “graceful”; name
the exact queue and protocol-state boundary it honors.

## Validation commands

Run targeted tests while iterating, then the complete relevant suites:

```text
cargo fmt --all
cargo check --workspace
cargo test -p rumqttc-v4-next
cargo test -p rumqttc-v5-next
cargo test -p rumqttc-v4-next --test reliability -- --nocapture
cargo test -p rumqttc-v5-next --test reliability -- --nocapture
cargo hack --each-feature --exclude-all-features test -p rumqttc-v4-next -p rumqttc-v5-next
cargo hack clippy --each-feature --exclude-all-features --no-dev-deps -p rumqttc-v4-next -p rumqttc-v5-next
```

If session-store types or persistence behavior change, also run:

```text
cargo fmt --manifest-path session-store-file/Cargo.toml --all
cargo check --manifest-path session-store-file/Cargo.toml --workspace
cargo test --manifest-path session-store-file/Cargo.toml --workspace
```

Record any unavailable `cargo-hack`, broker, TLS, or platform prerequisites
rather than silently omitting their coverage.

## Acceptance criteria

The work is complete only when:

- both protocol clients expose equivalent ordered-disconnect APIs;
- a successfully admitted fence cannot overtake any preceding covered publish
  under flow control;
- later requests cannot execute on the closing connection;
- the completion handle distinguishes admission, protocol completion,
  DISCONNECT flush, timeout, supersession, and failure;
- timeout covers the entire post-admission ordered shutdown;
- reconnect, replay, persistence, and process-local limitations are explicit
  and tested;
- existing admitted-only and immediate shutdown policies remain selectable;
- QoS and MQTT 5 negative-ack semantics are truthful;
- `from_senders` makes no managed-execution guarantee;
- v4/v5 unit, reliability, smoke, feature-matrix, and lint checks pass; and
- `CHANGELOG.md` and user-facing recipes explain when to use the ordered fence
  instead of `disconnect()` or per-publish tracking.
