# MQTT 5 publish admission

`V5Config::publish_admission_policy` is fixed at construction. C callers use
`rumqttc_config_set_v5_publish_admission_policy` before starting a client.
Setters update reusable configurations transactionally and affect future clients;
invalid values and MQTT 3.1.1 configurations return an error.

| Policy | Before CONNACK / during reconnect | While connected |
| --- | --- | --- |
| `RequireNegotiatedCapabilities` (wrapper default, C value 0) | QoS 1/2, retained and alias-bearing publishes wait for capabilities. Ordinary non-retained, alias-free QoS 0 can enter the queue. | Validate against the current broker capabilities before admission. |
| `EventLoopValidated` (C value 1) | Eligible publishes can enter the native queue, subject to channel and retained-work capacity. | Native processing performs the negotiated checks; tracked completion reports local rejection. |

Intrinsic validation stays eager in both modes: malformed topics, alias zero,
invalid properties and incompatible protocol options never enter the queue.
Admission means process-local ownership, not broker acceptance. QoS 0 completes
at local network flush; QoS 1 at PUBACK; QoS 2 at terminal PUBREC rejection or
PUBCOMP. A negative broker ACK retains its actual broker reason and ACK contents.
Local checks never manufacture broker ACKs or reasons.

`ClientHandle::try_admit` and both C publish entry points are nonblocking.
Backpressure proves that this attempt was not admitted. Wrapper-core's
`admit_async`/`admit` wait for native capacity/capability progress and wake on
shutdown. Cancellation before admission submits nothing. Dropping an admitted
completion observer never cancels work or releases its reservation. Concurrent
C callers can poll/wait on a completion repeatedly, using their own deadlines.

## Retained-work limits

Every wrapper MQTT 5 client defaults to **1,024 outstanding publishes and 16 MiB
of charged publish data**, under either admission policy. Configure nonzero
`V5Config::publish_budget` limits or call `rumqttc_config_set_v5_publish_budget`;
`rumqttc_config_reset_v5_publish_budget` restores these defaults. No unlimited
wrapper setting is provided. Direct native Rust builders retain their existing
unbudgeted default and may opt in with `publish_budget`.

`ClientHandle::publish_budget_snapshot` and the additive C
`rumqttc_client_v5_publish_budget_snapshot` return coherent current counters and
limits without polling the driver. C scalar outputs are optional, require at
least one non-NULL pointer, and are zeroed before validation. Wrapper-core also
reports whether checkpoint recovery is pending.

Native terminal senders own count/byte reservations. They cover requests in the
channel, scheduling queues, replay and protocol state. Moving requests, draining
channels on failed establishment, reconnecting, or dropping observers cannot
create capacity. QoS 2 retains its reservation through PUBREC to PUBCOMP.
Successful flush/ACK, local rejection, session loss, unrecoverable alias replay,
and native destruction release capacity exactly once. Wrapper completions are
retired as native progress is processed, including when observers are dropped.

The byte charge is the sum of logical retained data lengths:

- Payload and recoverable topic;
- Response Topic, Correlation Data and Content Type;
- All User Property key/value lengths plus `size_of::<(String, String)>()`
  per pair, including empty pairs;
- `size_of::<usize>()` per Subscription Identifier in restored/native data.

An alias-only publish reserves **65,535 topic bytes** before enqueueing so that
native reconnect repair can expand it without exceeding the charge. A concrete
topic with an alias pays its actual topic length. The reservation stays at its
original charge until termination, even if QoS 2 discards its payload early.
Restored PUBREL consumes one count slot and zero data bytes.

This bounds outstanding operations and charged data, **not process RSS**. Fixed
per-operation request/notice/registry metadata is bounded by the count limit;
protocol tracking arrays, packet-ID storage and alias caches have separate
native negotiated/configuration bounds. Cached alias topics can retain up to
65,535 bytes per negotiated alias. Encoded network buffers, temporary clones,
checkpoint decoding/encoding and store callbacks have separate packet/batch/
checkpoint limits. Rust `Bytes` views can retain larger backing allocations,
and strings/vectors can have spare capacity; the charge measures logical data,
not backing allocation capacity. C copies its supplied views. Applications must
separately bound producer concurrency, caller-owned commands awaiting admission,
caller-retained completed results and host callback allocations. These publish
limits do not constrain subscription/control operations or inbound traffic.

A configured session store gates new publish admission until checkpoint loading
and reservation succeed, even in event-loop-validated mode. Restored PUBLISH and
PUBREL reserve together before replay state is installed. A checkpoint exceeding
either limit terminates recovery with `PublishRestoreBudgetExceeded` /
`RUMQTTC_STORE_FAILURE_PUBLISH_BUDGET_EXCEEDED`; it is left intact for retry with
larger limits. Native redirects that change a reused session's scope or client
identity rearm the admission gate before loading the target checkpoint. Every
fresh store load also rearms the gate, including loads triggered by changes to
the native event loop's public client-ID or store-scope options. Cancelled and
failed loads keep admission gated until recovery succeeds; ordinary reconnects
that reuse already-loaded state remain open.
Temporary redirect restoration applies the same preflight.
Checkpoint size limits still bound decoding separately.

Offline queueing survives ordinary reconnects only while the process and native
event loop remain alive. A session checkpoint contains protocol recovery state;
it does not durably record every accepted application submission. Applications
needing that guarantee must own a durable outbound queue.

## Failure and retry contract

Wrapper errors carry `PublishFailure`; C uses `rumqttc_error_publish_failure` with
optional `present`/`reason` outputs. Read `rumqttc_error_context` for delivery and
`rumqttc_error_flags` for retryability. Unknown reason values should remain
observable to applications. The following names abbreviate
`RUMQTTC_PUBLISH_FAILURE_*`.

| Failure | Delivery and retry |
| --- | --- |
| `CAPABILITIES_PENDING` | NotAdmitted, retryable after CONNACK establishes capabilities. |
| `RECOVERY_PENDING` | NotAdmitted, retryable after successful checkpoint recovery. |
| `REQUEST_CHANNEL_FULL` | NotAdmitted, retryable when native processing frees a channel slot. |
| `COUNT_EXHAUSTED`, `BYTES_EXHAUSTED` | NotAdmitted, retryable after outstanding work terminates. Queue transfers alone do not help. |
| `TOO_LARGE` | NotAdmitted, non-retryable with these limits; reduce data or construct a client with larger limits. |
| `TOPIC_ALIAS_ZERO` | NotAdmitted, non-retryable; correct the intrinsic packet. |
| `RETAIN_UNAVAILABLE`, `MAXIMUM_QOS`, `TOPIC_ALIAS_MAXIMUM`, `TOPIC_ALIAS_UNMAPPED` | Strict: NotAdmitted. Deferred: Rejected if never handed toward network output, otherwise Ambiguous. Non-retryable unchanged; use compatible options, bind the alias, or explicitly reconsider after a capability change. |
| `TOPIC_ALIAS_REPLAY_UNAVAILABLE` | Rejected if never handed toward output, otherwise Ambiguous; non-retryable unchanged. A later application attempt needs a concrete topic. Native aliases remain scoped to one connection. |
| `SESSION_RESET` | Rejected and retryable only when definitely unsent; otherwise Ambiguous and non-retryable automatically. Application policy must account for possible duplicate delivery. |
| `REDIRECTED`, `BROKER_ONLY_SESSION_RESUME` | Rejected if definitely unsent, otherwise Ambiguous; no automatic retry. Reconsider the target/session policy explicitly. |
| `QOS0_NOT_FLUSHED`, `PERSISTENCE` | Conservative transmission history determines Rejected versus Ambiguous; no automatic retry. Resolve the network/store failure and duplicate-delivery policy first. |
| `RECEIVER_TERMINATED` | Ambiguous; no automatic retry, because transmission history is unavailable. |
| `RESTORE_BUDGET_EXCEEDED` | Persistence failure before replay admission; no automatic retry. Restart with sufficient limits or explicitly discard the checkpoint using application policy. |

C reports `RUMQTTC_BACKPRESSURE` for retryable admission waits and
`RUMQTTC_LOCAL_REJECTED` (16) for known negotiated admission rejection or definitely
unsent tracked local rejection. Previously transmitted replay failure reports
`RUMQTTC_AMBIGUOUS`. Broker rejection remains `RUMQTTC_BROKER_REJECTED` with a
broker reason. Ordinary validation errors retain their existing status. Every
failed producer attempt has delivery NotAdmitted; every tracked outcome retains
its operation ID. Timeout is an observer deadline and never a terminal release.

JavaScript and Python inherit the strict policy and finite defaults through
wrapper-core. This change adds no host-language policy or limit selectors.
