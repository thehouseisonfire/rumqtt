# Event-loop/request-channel deadlock investigation

**Scope:** `rumqttc-v4-next` and `rumqttc-v5-next`

## Executive conclusion

Both protocol clients can self-block when one task alternates between
`EventLoop::poll()` and an awaited bounded-channel client method such as
`AsyncClient::publish()`.

The behavior reproduces with request capacity 10 and otherwise default batching.
The effective initial read batch is 50 for MQTT 3.1.1 and 128 for MQTT 5, so a
broker burst can leave more than ten incoming PUBLISH events buffered. If the
application queues one outgoing PUBLISH for each event, the eleventh
`publish().await` waits for channel capacity while the only event loop able to
free that capacity is owned by the same suspended task.

No production scheduler change is recommended. The public API already documents
this progress contract and provides two bounded alternatives:

- drive the event loop in a genuinely independent task; or
- use `try_publish()` when dropping a forwarded publish is the intended overload
  policy.

Draining blocked requests from the bounded channel into the internal scheduler
would move the backlog rather than solve it, weakening meaningful bounded-memory
backpressure. This investigation therefore recommends regression coverage and
more explicit forwarding documentation.

## Current implementation

`AsyncClient` builders create separate request paths for flow-controlled
publishes, control requests, and immediate disconnect. Normal and control
channels use the configured bounded capacity by default. Immediate disconnect
has a narrow unbounded priority path.

`publish().await` validates and owns the packet, then awaits Flume
`send_async()`. Its success means channel admission only; it does not mean that
the packet was processed, written, flushed, or acknowledged. `try_publish()`
uses `try_send()` and returns `ClientError::RequestChannelFull` without queueing
the request when no slot is immediately available.

After connection establishment, both `EventLoop::select()` implementations:

1. return the front of `state.events`, when present;
2. check immediate disconnect;
3. process ready internally scheduled work;
4. admit control and normal channel requests before a fresh network read; and
5. read another network batch only when earlier scheduling points do not return
   an event.

The first rule is decisive: buffered protocol events are returned without
admitting a request.

### Read batching

An explicitly configured positive read batch is clamped to 1–128. A configured
zero uses:

```text
max(max_request_batch, outgoing_inflight / 2, 8)
```

also clamped to 1–128. When pending or queued request work is already visible,
the adaptive result is capped at 16.

That pending-work cap does not apply to the initial quiet network read because
the forwarding requests do not exist until the application receives the
resulting events.

- MQTT 3.1.1 defaults to outgoing inflight 100, yielding an initial batch of 50.
- MQTT 5 starts at an outgoing limit of 65,535 unless configuration or CONNACK
  constrains it, yielding the maximum batch of 128.

`readb()` waits for the first packet, then processes already available framed
packets up to the limit. Each incoming QoS 0 PUBLISH normally adds one incoming
event. With automatic acknowledgements, each QoS 1 PUBLISH adds the incoming
event and a derived outgoing PUBACK event.

Automatic PUBACK is written during `readb()` and flushed while the read batch is
completed, before the first buffered event is returned to the application.

## Deterministic reproduction

The investigation harness used a local Tokio TCP broker, a single write
containing each incoming burst, active broker-side reads, and timeout-wrapped
operations. Keepalive was disabled so a later broker keepalive timeout could not
be confused with the initiating condition.

Command used with the temporary, untracked investigation harness:

```text
timeout 30s cargo run --quiet \
  --manifest-path .investigation-deadlock/Cargo.toml
```

The committed regression cases can be run without that harness:

```text
timeout 90s cargo test -p rumqttc-v4-next --test reliability forwarding -- --nocapture
timeout 90s cargo test -p rumqttc-v5-next --test reliability forwarding -- --nocapture
```

| Scenario | MQTT 3.1.1 result | MQTT 5 result | Classification |
| --- | --- | --- | --- |
| Adaptive defaults, capacity 10, QoS 0 | batch 50; send timed out at event 11; receiver length 10 | batch 128; timed out at 11; receiver length 10 | Self-deadlock |
| Adaptive defaults, capacity 10, QoS 1 | timed out at 11; all 40 incoming publishes already PUBACKed | same | Self-deadlock, not ACK blocking |
| Capacity 2, explicit batch 8, QoS 0 | timed out at 3 | same | Self-deadlock |
| Capacity 10, explicit batch 32, QoS 1 | timed out at 11 | same | Self-deadlock |
| Capacity 10, explicit batch 10, QoS 0 | completed 30 forwards | same | Recovered at the next empty-buffer scheduling point |
| Capacity 3, batch 16, continuous input, `try_publish()` | polling continued; 859 accepted and 1,430 full-channel drops | polling continued; 859 accepted and 1,446 drops | Explicit overload policy |
| Capacity 3, batch 16, continuous input, dedicated polling | event loop and publisher remained active | same | Independent progress |
| Outbound QoS 1 window 1, broker withholds PUBACK | only one forward sent; all 12 incoming publishes processed and ACKed | same | Inflight backpressure, not event-loop deadlock |

The self-blocking cases used a 100 ms timeout around the final
`publish().await`. At timeout, diagnostics showed a full normal request receiver,
an empty internal scheduler, and buffered events still waiting. The same task
owned the only receiver and could not call `poll()` while awaiting the send.
Without cancellation or dropping an endpoint, no participant could break that
cycle.

The broker continuously read client output, so the result was not caused by
network-write backpressure. The harness used Tokio timers rather than
`std::thread::sleep`, excluding runtime-worker starvation. Default reproduction
diagnostics showed no outbound inflight work, excluding QoS inflight pressure.

## MQTT delivery and dropping

QoS 0 transfers the message according to the capabilities of the underlying
network. It has no acknowledgement or retry. This does not define a broker
policy to discard messages whenever an application callback is busy.

For QoS 1, the receiver accepts ownership and sends PUBACK. MQTT explicitly does
not require completion of application delivery before PUBACK. With rumqttc's
default automatic acknowledgement mode, the broker can therefore receive
PUBACK before the application consumes the corresponding buffered event.

Consequently:

- TCP pressure is not a per-message application drop policy.
- Broker queue limits and expiry behavior are broker-specific.
- MQTT 5 Receive Maximum limits unacknowledged QoS 1/2 packets, not QoS 0, and
  automatic acknowledgements replenish it.
- Silently discarding an already decoded event would lose a message whose
  ownership the client may already have accepted.
- `try_publish()` is the appropriate existing decision point for dropping an
  outgoing forward.

## History

Local history was sufficient; no upstream browsing was needed.

- `0543660` (2020) introduced the event API and returned buffered outgoing and
  incoming events before selecting new work.
- `0e036d2` deliberately prioritized ready user requests over fresh network
  reads in MQTT 3.1.1, but retained buffered-event-first behavior.
- `fa1d9c7` added configurable/adaptive read batching for throughput and
  fairness, replacing a hardcoded batch of 10. It did not claim to make
  same-task bounded sends safe.
- `e8ea555` added the shared protocol-aware outbound scheduler and separate
  request lanes while intentionally preserving bounded backpressure under
  publish flow control.
- `334e471` documented same-task bounded-channel self-blocking and recommended
  independent polling or `try_publish()`.

No local commit claimed to eliminate this issue. At the investigated commit, no
test encoded same-task bounded publishing as a supported progress guarantee.

## Candidate assessment

| Candidate | Result |
| --- | --- |
| Keep the documented progress contract | Preserves bounded backpressure; does not make the unsafe pattern progress |
| Improve documentation and tests | Makes the supported patterns and failure boundary explicit without runtime risk |
| Dedicated event-loop task | Correct when it never waits on the publisher or a blocking handoff |
| `try_publish()` | Prevents self-blocking and provides an explicit bounded drop signal |
| Keep read batch no larger than channel capacity | Mitigates a clean one-forward-per-event burst, but does not cover other producers, capacity zero, inflight pressure, or network stalls |
| Process one ready request before each buffered event | Helps while the request is sendable; changes event ordering and cannot solve blocked inflight/network work |
| Admit a bounded request batch | Same limitation with more event latency |
| Drain every currently queued request | Can starve event delivery/network processing and expand internal backlog |
| Admit blocked requests into the internal scheduler | Frees channel slots by moving the backlog; weakens bounded-memory behavior |
| Fairness budget | Improves latency balance but cannot guarantee send completion without moving blocked work |
| Unbounded request channel | Replaces the wait with potentially unbounded memory growth |
| Global mailbox/backpressure redesign | Could define a true total-memory budget, but is broad, complex, and behaviorally breaking |

## Invariants constraining a scheduler change

Any future design must preserve:

- MQTT packet and acknowledgement ordering;
- application PUBLISH FIFO;
- QoS inflight quotas and packet-identifier ownership;
- collision, pending, and reconnect replay order;
- protocol-valid control bypass and immediate-disconnect priority;
- graceful-disconnect barriers;
- CONNECT/CONNACK establishment order;
- persistence checkpoint and completion-notice milestones;
- fairness between reads, protocol responses, and application requests;
- meaningful bounded memory;
- cancellation safety; and
- MQTT 3.1.1/MQTT 5 parity.

Buffered-event-first scheduling is not itself mandated by MQTT, but it is
longstanding observable API behavior. Incoming-before-derived-outgoing ordering
is also intentionally tested and documented in the state machines.

## Recommendation and resulting changes

Do not change production scheduling.

Add mirrored MQTT 3.1.1 and MQTT 5 regression tests that:

1. create one eight-packet broker burst in a single transport write;
2. verify that capacity-three same-task forwarding waits on the fourth send
   while diagnostics show a full receiver and untouched buffered events; and
3. verify that `try_publish()` consumes all eight buffered incoming events while
   accepting three forwards and explicitly rejecting five;
4. reproduce the default-capacity QoS 1 case; and
5. verify that every automatic PUBACK from the burst reaches the broker before
   application event draining reaches the blocked eleventh forward.

The backpressure recipe should distinguish a genuinely independent polling task
from merely moving publication to another task. Its forwarding example should
use a bounded application queue with a nonblocking handoff and an explicit drop
policy. Lossless forwarding requires an independently driven durable queue or
another design whose consumer does not depend on event-loop progress.

Production source, configuration defaults, protocol behavior, and changelog do
not need to change for this recommendation.
