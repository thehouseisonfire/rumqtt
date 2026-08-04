# Future Work

This backlog contains credible work extracted from the former V4 and V5 design
journals and checked against the current implementation. It deliberately omits
features that already exist, including tracked completion notices, graceful
disconnect barriers, immediate disconnect, static pending replay throttling,
and versioned session checkpoints.

## High Priority

- [ ] **Detect stalled protocol acknowledgements and reconnect safely (V4 and
  V5).** Add observation and policy around QoS publish, SUBSCRIBE, and
  UNSUBSCRIBE exchanges that make no acknowledgement progress for a configured
  interval. Treat the connection as unhealthy, close it, reconnect, reconcile
  `Session Present`, and replay only according to MQTT session rules. Do not
  republish a timed-out packet in place on an otherwise live connection: the
  broker may still own its packet identifier or may already have delivered it.
  Define interactions with keepalive, manual acknowledgements, graceful
  disconnect, and persistent checkpoints before enabling automatic action.

- [ ] **Finish the MQTT 5 negotiated-limit and connection-scope audit
  (`rumqttc-v5-next`).** Verify both directions of Receive Maximum, including
  detecting a server that exceeds the value advertised by the client; restore
  configured baselines when a later CONNACK omits a connection-scoped property;
  and cover Server Keep Alive, broker Receive Maximum, Maximum Packet Size,
  Retain Available, and Topic Alias Maximum across repeated reconnects to
  brokers with different properties. Control packets must remain sendable when
  the outgoing PUBLISH quota is exhausted. This work depends on an explicit
  model separating configured values from per-connection negotiated values.

- [ ] **Decouple application notification delivery from protocol-critical
  progress (V4 and V5).** Explore a bounded event buffer or split driver API in
  which keepalives, automatic acknowledgements, and QoS handshakes can progress
  even when application event consumption is temporarily slow. Specify memory
  bounds, overflow behavior, event ordering, manual-ack behavior, and shutdown
  semantics. The design must not hide an executor or silently drop incoming
  publications.

## Medium Priority

- [ ] **Add an explicit submission barrier or flush API (V4 and V5).** Provide
  a non-terminal way to identify all requests accepted before a barrier and
  wait until each has reached a documented milestone. Distinguish channel
  admission, local network flush, and broker acknowledgement. On timeout,
  report completed, rejected, still in-flight, and ambiguous operations; never
  claim that a flushed PUBLISH was not delivered. Reuse tracked notices where
  possible without turning the existing terminal disconnect barrier into a
  second, subtly different contract.

- [ ] **Add configurable reconnect policy as an application-facing layer (V4
  and V5).** Support exponential backoff, jitter, attempt limits, reset-after-
  stability, and error classification while retaining direct `EventLoop::poll`
  for callers that own policy. Authentication refusal, malformed protocol data,
  DNS/connect failures, peer closure, and transient I/O should not all receive
  the same default retry treatment. Reconnect policy must not bypass session
  reconciliation or discard pending tracked outcomes.

- [ ] **Pause application traffic without pausing MQTT (V4 and V5).** Define a
  control operation that stops admission or scheduling of application PUBLISH
  packets while continuing keepalives, inbound reads, automatic/manual
  acknowledgements already submitted, AUTH, and outstanding QoS handshakes.
  Specify whether SUBSCRIBE and UNSUBSCRIBE remain allowed and how bounded
  producer backpressure behaves. A full socket read/write pause is explicitly
  out of scope because it cannot retain a compliant live connection.

- [ ] **Support runtime publish-rate control (V4 and V5).** Extend the current
  static pending-replay throttle into a dynamically adjustable application
  publish limiter. Define whether limits apply to bytes, messages, or both and
  whether replay shares the same budget. MQTT control traffic and packets
  required to complete in-flight exchanges must never be throttled behind the
  application rate limit.

- [ ] **Provide finite-batch completion reporting (V4 and V5).** Build an
  aggregate helper over tracked requests for bounded publishing jobs. Results
  should preserve input identity and report QoS-specific completion, broker
  rejection, local validation failure, persistence failure, timeout, and
  ambiguous in-flight status. Cancellation and timeout must not imply that a
  packet already written to the network was not delivered.

## Long-Term Architectural Work

- [ ] **Design a durable disk-backed application outbox (V4 and V5 or a
  companion crate).** Persist requests before submission, remove them only at a
  configured QoS milestone, and coordinate replay after restart. This is
  separate from `SessionStore`, which persists only MQTT protocol state already
  admitted by the event loop. Define crash consistency, duplicate-delivery
  behavior, compaction, quotas, payload ownership, checkpoint/outbox ordering,
  and single-writer or fencing requirements.

- [ ] **Evaluate generic request-source adapters (V4 and V5).** Allow a custom
  bounded stream or receiver to feed the event loop without an extra forwarding
  task, while preserving the current scheduler's distinction between
  application publishes, control work, replay, tracked notices, and immediate
  disconnect. The existing `from_sender`/`from_senders` APIs only expose a
  request sink and do not attach an arbitrary source to a normal event loop.
  Any adapter must define cancellation safety and end-of-stream shutdown.

- [ ] **Separate standalone drain and explicit transport abort operations (V4
  and V5).** The current APIs provide terminal graceful disconnect and immediate
  DISCONNECT. Evaluate a reusable non-terminal drain primitive and an explicit
  abort that closes transport without promising MQTT DISCONNECT. Name and
  document these operations so callers cannot confuse queue admission, drain,
  graceful MQTT shutdown, and abrupt cancellation.

## MQTT 3.1.1-Specific Work

- [ ] **Define the policy for broker session loss with locally retained work
  (`rumqttc-v4-next`).** The current event loop discards local session protocol
  state when `Clean Session = 0` receives `Session Present = 0`. Evaluate
  whether an opt-in application policy should reclassify eligible publishes as
  new work, with new packet identifiers, rather than fail their tracked
  outcomes. The default must remain conservative because previous broker
  delivery can be ambiguous and subscription state may also have disappeared.

## MQTT 5.0-Specific Work

- [ ] **Complete a session-recovery boundary matrix (`rumqttc-v5-next`).** Add
  end-to-end coverage for Clean Start, effective Session Expiry Interval,
  Session Present, restored local checkpoints, broker-only resume policy,
  incoming QoS 2 state, and DISCONNECT expiry overrides across live reconnect
  and process restart. Existing checkpoint/replay behavior is substantial; the
  goal is to identify and close remaining boundary gaps rather than replace it.
  Cases with no broker session must never replay old packet identifiers as if
  the session survived.

- [ ] **Handle broker redirects explicitly (`rumqttc-v5-next`).** Interpret
  `Use Another Server` and `Server Moved` reason codes together with Server
  Reference from CONNACK or DISCONNECT, then expose a policy hook rather than
  silently changing endpoints. Validate references, prevent redirect loops,
  bound attempts, define TLS identity and credential behavior, and decide
  whether session checkpoints are scoped to the old endpoint, a broker cluster,
  or the redirected target.

## Open Design Questions

- [ ] **Choose completion semantics for barriers and batches.** Decide whether
  the primary contract should stop at local flush, await MQTT acknowledgement
  where one exists, or expose selectable milestones. The result type must
  represent partial and ambiguous completion without encouraging unsafe retry.

- [ ] **Choose ownership for reconnect timing and policy.** Compare a reusable
  wrapper that decides when to poll again with configuration embedded in
  `EventLoop`. A wrapper preserves the event loop's executor-neutral design;
  embedded policy may provide a simpler default but makes cancellation, error
  reporting, and dynamic reconfiguration part of the core contract.

- [ ] **Choose backpressure boundaries for split protocol and application
  progress.** Determine which events may be buffered, whether inbound publishes
  can ever be flow-controlled without stopping socket reads, and how manual
  acknowledgements interact with a saturated application queue. The answer is
  a prerequisite for notification decoupling and safe pause controls.
