# TODO: Fix MQTT v5 Topic Alias Handling Across Reconnect
## Summary

There is a correctness bug in `rumqttc-v5` around reconnect and pending outbound `PUBLISH` replay when MQTT 5 topic aliases are in use.

The bug is not "lack of public access to pending packets". That is only one possible escape hatch. The actual problem is that topic alias state is scoped to a single network connection, while this crate can preserve outbound requests across disconnect and replay them on a later connection.

The implementation needs to make reconnect replay correct for MQTT 5 topic aliases without weakening correctness elsewhere.

Per the official specification, in section 3.3.2.3.4 (Topic Alias):

“Topic Alias mappings exist only within a Network Connection and last only for the lifetime of that Network Connection. A receiver MUST NOT carry forward any Topic Alias mappings from one Network Connection to another.”

## What Is Happening Today

`EventLoop::clean()` collects work that should be retried after a connection failure and stores it in the internal pending queue. Later, after reconnect, the event loop prioritizes replaying that queue before fresh outbound requests.

That replay behavior is generally correct for retransmission, but topic aliases create an extra constraint:

- MQTT 5 topic alias mappings exist only for the lifetime of one network connection
- outbound `PUBLISH` packets may carry `PublishProperties.topic_alias`
- some pending outbound publishes may rely on alias state from the previous connection
- after reconnect, the broker no longer has that alias mapping unless it is re-established on the new connection

As a result, a pending publish that was valid on the old connection may no longer be valid if replayed unchanged on the next one.

## Why This Matters

This is a protocol-correctness issue, not just an API ergonomics issue.

If reconnect replay sends a pending publish whose meaning depends on alias state that died with the old connection, the client can emit a packet sequence that is no longer semantically valid for the new connection.

That means the fix must be judged by whether reconnect replay is correct, not by whether external code can reach the queue.

## Constraints

Any acceptable fix should satisfy all of these:

- Pending replay semantics should remain correct for requests that are valid across reconnect.
- MQTT 5 topic alias lifetime rules must hold across reconnect boundaries.
- Connection-scoped state from the old connection must not silently leak into the next one.
- Tracked notices must still behave deterministically if a pending request cannot validly be replayed.
- Replay ordering should remain stable for requests that survive reconnect cleanup.
- The fix should respect existing crate invariants around the event loop and request replay path.

## Things To Investigate Carefully

- What exactly can enter `EventLoop`'s pending queue today.
- Which pending requests are replay-safe across reconnect and which are not.
- How outbound topic alias usage is represented in this fork.
- Whether the current code retains enough information to make replayed alias-bearing publishes valid on a new connection.
- Whether any existing connection-scoped alias-related state is retained longer than it should be.
- How notice/failure semantics should work if some pending request is no longer valid to replay.

## Scope

This issue is about MQTT v5 reconnect correctness.

It is not primarily about:

- making internal queue fields public
- introducing a broader public mutation API unless that is independently justified
- redesigning the whole event loop
- MQTT v4 behavior

## Evidence The Final Fix Should Address

The implementer should be able to explain, using actual code paths in this fork:

- how pending replay works before the fix
- what alias-related state exists before the fix
- why the previous behavior is incorrect for at least one reconnect case
- why the new behavior is correct for both:
  - pending publishes that can still be replayed validly
  - pending publishes that cannot validly be replayed as-is

## Test Expectations

The final change should come with focused tests that demonstrate correctness around reconnect and topic alias usage.

At minimum, the tests should cover:

- a reconnect case where pending replay remains valid
- a reconnect case where alias-related pending state would have been invalid before the fix
- correct tracked-notice behavior when replay is no longer possible
- correct handling of connection-scoped alias state across reconnect

The important point is not the exact test names or structure, but that the new behavior is provably correct and regression-resistant.

## Acceptance Criteria

This issue is complete when:

- reconnect replay is correct with MQTT 5 topic aliases
- connection-scoped alias semantics are preserved across disconnect/reconnect
- invalid replay of stale alias-dependent pending publishes is prevented
- tracked notices remain coherent
- the solution is justified by the crate's actual invariants and code paths, not just by exposing internals
