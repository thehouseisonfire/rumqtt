# rumqttc-wrapper-core

Private Rust infrastructure shared by native rumqtt wrappers. The crate owns
the MQTT event loop on a dedicated thread and exposes owned, protocol-neutral
configuration, commands, completions, events, diagnostics, and errors.

## Protocol support contract

This crate supports both MQTT 3.1.1 and MQTT 5 through one shared API. Each
`NativeClient` instance explicitly selects exactly one protocol through its
`ClientConfig`, and that selection remains fixed for the client's lifetime. A
client does not negotiate, fall back to, or switch between protocol versions;
construct a new client to use another version.

Protocol-neutral commands and events share types only where their semantics
genuinely overlap. Observable protocol differences remain explicit:

- MQTT 3.1.1 uses `V311Config`, including `clean_session`;
- MQTT 5 uses `V5Config`, including `clean_start` and session expiry;
- MQTT 5 publish properties remain in `V5PublishProperties`; and
- broker reason information is present only where the selected protocol
  exposes it.

Supplying an option that is incompatible with the selected protocol is an
error. The core must not silently discard MQTT 5 properties for an MQTT 3.1.1
client or invent a common interpretation for settings whose behavior differs.
This includes CONNECT authentication: MQTT 3.1.1 requires a username whenever
a password is present, while MQTT 5 permits username-only, password-only, and
combined credentials. Client identifiers and usernames are validated as MQTT
UTF-8 strings, and their encoded lengths—as well as the binary password
length—must fit the protocol's two-byte field before the driver starts.
Client-originated MQTT 5 publishes also reject Subscription Identifiers. The
field is retained on normalized incoming publishes, but MQTT-3.3.4-6 forbids
clients from sending it to a server. Other publish properties are validated
before admission, including payload format, negotiated Topic Alias bounds,
Maximum QoS, Retain Available, Response Topic syntax, MQTT UTF-8 strings, and
two-byte binary/string lengths. Publish Topic Names for both protocols reject
U+0000 and values exceeding the MQTT UTF-8 string length before admission. A
publish rejected by one of these negotiated capabilities cannot establish or
rebind a Topic Alias in the wrapper's state.
Manual Topic Alias mappings are ordered with publish admission, may be reused
with an empty topic only after mapping, and are cleared for every new network
connection. An empty Topic Name without a nonzero Topic Alias is rejected before
admission. On connection loss, the wrapper repairs the narrow set of publishes
that can be admitted after rumqttc's cleanup drain but before the wrapper observes
the error under the same admission gate: recoverable publishes carry their full
Topic Name without an alias into replay, while unrecoverable tracked publishes
complete with an error rather than emitting an invalid packet. Repair starts from
rumqttc's alias mapping at the exact cleanup boundary and applies publishes admitted
after that boundary in order, so a later alias rebinding cannot change the topic of
an earlier alias-only publish.
Broker publish capabilities are unknown before the first MQTT 5 CONNACK and
again while reconnecting. During those intervals, nonblocking admission of a
QoS 1/2, retained, or Topic-Alias publish reports transient backpressure without
admitting the packet; asynchronous admission waits and retries when CONNACK
installs the new connection's capabilities. Alias-free, non-retained QoS 0
publishes remain admissible because every MQTT 5 server supports that form.
Native language wrappers should normally ship one package supporting both
protocols and translate their explicit protocol selector into `ClientConfig`.

The request and event channels are bounded. Applications must continuously
consume events: if the event buffer remains full beyond
`CommonConfig::event_delivery_timeout`, the driver terminates with an explicit
backpressure error rather than dropping incoming publishes. Terminal status
uses an independent channel and remains observable when the ordinary event
buffer is full.
Wrapper control traffic and MQTT polling use fair arbitration, so sustained
diagnostics or completion traffic cannot indefinitely suppress network and
keep-alive progress.

Admission is distinct from MQTT completion. Dropping or timing out a
`CompletionHandle` never cancels work already admitted to rumqttc. Immediate
shutdown interrupts an in-progress connection attempt and reports unfinished
admitted work as ambiguous; once connected, rumqttc observes its priority
disconnect at an event-loop scheduling point. Immediate shutdown remains a
persistent driver condition after its wake-up notification is consumed, so all
subsequent event deliveries bypass a full wrapper event buffer while the driver
closes. Graceful shutdown uses rumqttc's disconnect barrier and completes only
after the event loop closes.
For MQTT 5 QoS 2 recovery, a `PUBCOMP` with `PacketIdentifierNotFound` is a
successful terminal completion only when rumqttc identifies the corresponding
`PUBREL` as replayed; the same reason on an ordinary flow remains a broker
rejection.
In manual-acknowledgement mode, admission means the PUBACK or PUBREC entered
rumqttc's request channel, while `Completion::Acknowledged` is reported only
after the event loop flushes that packet to the network. Cancelling an
asynchronous acknowledgement while it is still waiting for request-channel
capacity restores its token for retry. Retransmissions of the same unacknowledged
incoming packet share one token; consuming it invalidates every copy, and a
retransmission received while its ACK is already queued needs no additional ACK
token. Reconnect processing advances the token generation and drains any late
connection-scoped ACK request under the same admission gate, preventing an ACK
that raced connection-loss cleanup from entering the replacement connection.

Request admission and the transition to `Closing` share one ordering gate. A
request that wins that gate is admitted before the disconnect barrier; a
capacity-waiting async request that loses it wakes and returns `NotAdmitted`
instead of entering rumqttc after shutdown has begun. Capacity waiters are also
woken when the driver reaches `Closed` or `Failed`, so a terminal driver cannot
leave synchronous or asynchronous admission blocked on a channel that will never
make further progress. Graceful shutdown also resolves diagnostics admitted
before its barrier with the final cached driver snapshot; immediate shutdown can
still leave unfinished diagnostics ambiguous.

## Admission modes and host threads

`ClientHandle::try_admit` is nonblocking and reports request-channel
backpressure immediately. `ClientHandle::admit_async` waits asynchronously for
capacity and is the normal choice when integrating with a host-language future
or promise. `ClientHandle::admit` is an explicitly blocking convenience API.

Never call `ClientHandle::admit` from a JavaScript event-loop thread, a Python
async-executor thread, or another latency-sensitive async thread. Doing so can
prevent the host runtime from processing unrelated work while the bounded MQTT
request channel is full. C APIs and other synchronous wrappers may expose it as
an explicitly blocking operation; async wrappers should use `admit_async` or
move the blocking call to a wrapper-owned worker thread. Use `try_admit` when
the host must remain nonblocking and prefers an immediate backpressure result.

This crate has no Python, JavaScript, C ABI, serialization, or host-runtime
dependencies and does not define a stable foreign ABI.
