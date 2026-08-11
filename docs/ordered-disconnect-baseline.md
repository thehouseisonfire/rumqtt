# Ordered graceful-disconnect baseline

Measured 2026-08-11, before an ordered disconnect API exists.

## Verified semantics

- Builder clients route publishes through the bounded normal-request channel, graceful disconnect through a separate
  control channel, and `disconnect_now()` through a dedicated immediate channel. Successful `publish().await` means
  request-channel admission, not MQTT-state admission or delivery.
- The scheduler classifies requests by readiness. QoS 1/2 publishes are blocked by a full outgoing window, while ready
  control work can pass them. Once `disconnect()` is observed, the loop stops later normal admission and waits for
  `MqttState::outbound_requests_drained()`.
- The drain predicate covers protocol-admitted QoS 1 through PUBACK, QoS 2 through PUBCOMP, packet-id/collision state,
  and admitted tracked subscribe/unsubscribe work. It excludes the normal receiver, scheduler queue, and pending
  throttle queue. Thus graceful DISCONNECT may overtake an accepted but flow-control-blocked publish.
- Protocol-admitted QoS 0 has no acknowledgement handshake; its boundary is successful request-batch network flush.
  Graceful disconnect flushes already-admitted QoS 0 writes before terminal DISCONNECT.
- `disconnect_now()` is checked before normal scheduling and first in the biased poll select. It writes and flushes
  DISCONNECT without waiting for QoS 1/2, but cannot interrupt in-progress polling work, connection setup, or an event
  loop that is not polled.
- One cloned sender retains channel order. Concurrent clones have no API-level global order. Graceful and immediate
  disconnect also use different channels from publishes, so enqueue-time order across classes is not a fence.
- Resumable reconnect replays protocol-admitted unacknowledged QoS 1/2 first (DUP set; QoS 2 PUBREL keeps its packet
  id), oldest outstanding publish first. Requests still in normal client queues remain queued for later admission.
  Clean-session/clean-start reconnect drops old protocol pending state.
- `from_senders` owns no event loop: all requests share the caller's `Sender<Request>`, including disconnect variants.
  It provides no priority, tracking, reconnect, persistence, replay, or acknowledgement semantics automatically.

Important implementation areas are `src/client.rs` (sender routing and disconnect methods), `src/eventloop.rs` (`poll`,
admission batching, classification, and disconnect drain), and `src/state.rs` (publish readiness, tracking/replay, and
`outbound_requests_drained`) in both protocol crates.

Existing evidence includes the paired reliability tests named `requests_are_blocked_after_max_inflight_queue_size`,
`requests_are_recovered_after_inflight_queue_size_falls_below_max`,
`bounded_publish_backpressure_is_preserved_while_inflight_is_full`,
`control_request_bypasses_blocked_publish_without_ack_progress`,
`graceful_disconnect_completes_qos2_handshakes_before_disconnect`,
`disconnect_now_sends_disconnect_without_waiting_for_qos2_completion`, and the reconnect/resend/clean-session tests.
Client unit tests `disconnect_now_is_not_prioritized_on_plain_request_channel` establish the external-sender boundary.

## Semantic-gap tests

The passing `graceful_disconnect_does_not_wait_for_unsent_flow_controlled_publish` tests fix inflight and request batch
at one. A enters MQTT state, B remains flow-control-blocked, the control-channel disconnect becomes the drain barrier,
A is acknowledged, and DISCONNECT is observed before B. Shared helpers also preserve the former v4 timeout and v5
properties-timeout variants as independently named passing tests.

The ignored `ordered_disconnect_waits_for_flow_controlled_publish_before_disconnect` tests require A/PUBACK,
B/PUBACK, then DISCONNECT. Today they deliberately call `disconnect()` as a compiling, failing placeholder. Replace
only that call with the future ordered operation, remove `#[ignore]`, and await its completion notice if applicable.

## Admission benchmark

`rumqtt-bench client admission` uses real cloned `AsyncClient` producers, bounded Flume channels, the event loop, MQTT
codec, an in-process duplex transport, and an immediately acknowledging peer. Each latency sample surrounds
`publish().await`; throughput ends when all calls are admitted. CPU is process user+system time from `getrusage`.
With `alloc-metrics`, a counting system allocator reports allocations during the measured interval. Payload is 64
bytes and each result below is one 20,000-message release run.

Inflight=100 results (throughput in thousands of admissions/s; latency in microseconds):

| proto/qos | channel | producers | k/s | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|---:|
| v4/0 | 4096 | 1 | 939 | 0.20 | 7.93 | 8.78 |
| v4/0 | 1 | 1 | 529 | 1.51 | 4.01 | 5.13 |
| v4/0 | 4096 | 8 | 955 | 8.20 | 14.39 | 19.47 |
| v4/0 | 1 | 8 | 886 | 8.74 | 12.12 | 14.64 |
| v4/0 | 4096 | 32 | 942 | 32.90 | 53.52 | 61.59 |
| v4/0 | 1 | 32 | 842 | 36.89 | 44.04 | 53.79 |
| v4/0 | 4096 | 64 | 929 | 55.86 | 103.97 | 125.16 |
| v4/0 | 1 | 64 | 753 | 77.36 | 112.84 | 121.28 |
| v4/1 | 4096 | 1 | 886 | 0.20 | 7.86 | 11.79 |
| v4/1 | 1 | 1 | 452 | 1.22 | 6.91 | 8.51 |
| v4/1 | 4096 | 8 | 668 | 12.06 | 20.73 | 27.31 |
| v4/1 | 1 | 8 | 648 | 11.90 | 19.57 | 26.26 |
| v4/1 | 4096 | 32 | 753 | 43.00 | 58.63 | 76.05 |
| v4/1 | 1 | 32 | 590 | 46.43 | 71.37 | 104.89 |
| v4/1 | 4096 | 64 | 800 | 68.72 | 96.38 | 147.26 |
| v4/1 | 1 | 64 | 617 | 95.85 | 137.06 | 189.55 |
| v5/0 | 4096 | 1 | 1226 | 0.32 | 4.09 | 7.83 |
| v5/0 | 1 | 1 | 464 | 1.73 | 5.03 | 6.13 |
| v5/0 | 4096 | 8 | 954 | 8.70 | 12.22 | 15.92 |
| v5/0 | 1 | 8 | 627 | 12.93 | 16.98 | 20.17 |
| v5/0 | 4096 | 32 | 909 | 36.31 | 45.33 | 53.30 |
| v5/0 | 1 | 32 | 635 | 54.61 | 64.30 | 72.71 |
| v5/0 | 4096 | 64 | 911 | 58.68 | 80.99 | 102.92 |
| v5/0 | 1 | 64 | 700 | 83.68 | 124.57 | 133.88 |
| v5/1 | 4096 | 1 | 823 | 0.30 | 7.84 | 12.62 |
| v5/1 | 1 | 1 | 363 | 1.54 | 7.01 | 10.89 |
| v5/1 | 4096 | 8 | 593 | 12.36 | 24.46 | 29.92 |
| v5/1 | 1 | 8 | 531 | 13.94 | 24.10 | 33.37 |
| v5/1 | 4096 | 32 | 632 | 42.16 | 92.43 | 127.01 |
| v5/1 | 1 | 32 | 494 | 52.50 | 86.85 | 131.84 |
| v5/1 | 4096 | 64 | 689 | 74.70 | 108.23 | 228.42 |
| v5/1 | 1 | 64 | 559 | 104.04 | 146.74 | 245.29 |

Inflight sensitivity at 8 producers and channel capacity 1:

| proto/qos | inflight | k/s | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| v4/0 | 1 / 100 / 1000 | 767 / 886 / 777 | 9.77 / 8.74 / 9.71 | 14.52 / 12.12 / 14.46 | 17.62 / 14.64 / 17.24 |
| v4/1 | 1 / 100 / 1000 | 290 / 648 / 610 | 29.78 / 11.90 / 12.35 | 37.32 / 19.57 / 19.91 | 40.92 / 26.26 / 31.29 |
| v5/0 | 1 / 100 / 1000 | 683 / 627 / 805 | 10.93 / 12.93 / 9.50 | 16.70 / 16.98 / 13.90 | 19.11 / 20.17 / 16.67 |
| v5/1 | 1 / 100 / 1000 | 334 / 531 / 482 | 19.51 / 13.94 / 14.45 | 38.30 / 24.10 / 25.02 | 51.35 / 33.37 / 37.70 |

CPU cost was about 1.65--5.25 microseconds/admission. Allocated bytes/admission ranged from 435--975 (v4) and
785--1,916 (v5). These are whole-workload comparative figures, not request-object sizes.

```console
cargo run --release -p benchmarks --features alloc-metrics --bin rumqtt-bench -- \
  client admission --protocol v4 --messages 20000 --producers 8 \
  --channel-capacity 1 --inflight 100 --qos 1
```

Repeat across protocols `v4 v5`, QoS `0 1`, producers `1 8 32 64`, capacities `4096 1`, and inflight `1 100 1000`.
Environment: Linux 7.1.6, Intel i5-13500H, rustc/cargo 1.96.1, Tokio multi-thread runtime. No CPU pinning, governor
lock, isolated cores, or repeated confidence interval was used; scheduler activity, frequency scaling, allocator state,
and thermals are noise. Use interleaved repetitions on the same host for before/after comparisons.

## Later implementation constraints

The gap is before MQTT-state admission, while today's drain is protocol-state-only. The scheduler reorders ready
control work around blocked publishes, disconnect and publishes use different channels, and clones lack cross-sender
call order. Reconnect distinguishes normal queued work from protocol replay, and `from_senders` owns no receiver/event
loop. These are constraints, not a proposed design.
