# Optional native protocol observations

Wrapper-core and C can observe native packet processing and scheduling. Selection
is fixed at startup and defaults off. MQTT state transitions, authentication
responses, and tracked terminal outcomes retain their existing authority.

This API observes events actually emitted by the native clients. It is not a
wire capture: native duplicate suppression, packet rejection, topic processing,
and error cleanup still apply. It adds no CONNECT emission point or packet history.

## Selection

Rust sets `config.common.protocol_observations` with
`ProtocolObservationSelection { incoming_control, precise_outgoing, raw_auth }`.
C calls `rumqttc_config_set_protocol_observations()` with any combination of:

| Flag | Value | Selection |
| --- | --- | --- |
| `RUMQTTC_OBSERVE_INCOMING_CONTROL` | 1 | Previously filtered incoming control packets |
| `RUMQTTC_OBSERVE_PRECISE_OUTGOING` | 2 | Precise outgoing packet activity |
| `RUMQTTC_OBSERVE_RAW_AUTH` | 4 | Incoming AUTH packet observations |

Zero disables all new selections; unknown bits are invalid arguments. Flags
replace the previous selection. MQTT 3.1.1 accepts raw-AUTH selection without
emitting AUTH observations. Python/JavaScript do not expose these selections;
their existing event formats and default sequences are unchanged.

| Coarse outgoing enabled | Precise outgoing selected | Each native outgoing packet event |
| --- | --- | --- |
| false | false | No event |
| true | false | One existing coarse event |
| false | true | One enriched outgoing event |
| true | true | The same single enriched event |

`AwaitAck` is scheduling activity, not a packet; only the coarse flag selects it.
It never has precise packet metadata. PUBLISH, CONNACK, and broker DISCONNECT keep
their existing incoming event representations and accessors, without duplicate
observation events. Their existing accessors remain the way to inspect those
packets. AUTH lifecycle events and their existing broker-property accessors are
independent of raw-AUTH selection.

## Availability matrix

Identifiers are optional; QoS 0 and packets without identifiers have none.
Properties below mean decoded contents retained by native events, including
ordered duplicate User Properties and absent versus present-empty strings/data.

| Protocol | Direction and kind | Identifier | Scalar reason | Filter results | Properties |
| --- | --- | --- | --- | --- | --- |
| v4 | Incoming PUBACK, PUBREC, PUBREL, PUBCOMP | Yes | Unavailable | Unavailable | Unavailable |
| v4 | Incoming SUBACK | Yes | Unavailable | Ordered return codes | Unavailable |
| v4 | Incoming UNSUBACK | Yes | Unavailable | Unavailable | Unavailable |
| v5 | Incoming PUBACK, PUBREC, PUBREL, PUBCOMP | Yes | Exact decoded code | Unavailable | Optional Reason String and User Properties |
| v5 | Incoming SUBACK, UNSUBACK | Yes | Unavailable | Ordered reason codes | Optional Reason String and User Properties |
| v4/v5 | Incoming PINGRESP | No | Unavailable | Unavailable | Unavailable |
| v5 | Incoming AUTH, separately selected | No | Exact decoded code | Unavailable | Optional method, data, Reason String, User Properties |
| v4/v5 | Outgoing PUBLISH, SUBSCRIBE, UNSUBSCRIBE, PUBACK, PUBREC, PUBREL, PUBCOMP | Where native event provides it | Unavailable | Unavailable | Unavailable |
| v4/v5 | Outgoing PINGREQ, DISCONNECT | No | Unavailable | Unavailable | Unavailable |
| v5 | Outgoing AUTH | No | Unavailable | Unavailable | Unavailable |

The native outgoing enum also has PINGRESP; it is mapped if emitted, but normal
wrapper commands do not request it. An enum variant does not establish supported
native emission. Rejected/malformed packets and duplicate incoming QoS 2 PUBLISH
packets need not produce incoming events. Outgoing PUBCOMP metadata cannot reveal
`PacketIdentifierNotFound`, even when that was the reason actually encoded.

## Semantics, delivery, and ownership

Incoming events describe native processing. Outgoing events describe the native
scheduling boundary, which does not guarantee successful socket flush or peer
receipt. Successful PUBREC is intermediate; use the tracked publish completion
for its terminal outcome. Observation handles never grant ACK or AUTH authority.

Generation is captured at mapping from the accepted connection, starting at one.
It is absent during pre-CONNACK handshakes, including retries. Replayed events
after accepted CONNACK carry the new generation. Packet identifiers can be reused
within a generation; generation plus identifier is not a unique operation key.

All events share the existing bounded queue in native event-stream order.
Enabling observation increases queue pressure: full queues can pause MQTT polling
and terminate the driver with `EventBufferOverflow` after the delivery timeout.
Shutdown retains the existing interruption/discard semantics. This is not
nonblocking, best-effort telemetry. Ready terminal notices are processed before
and during blocked delivery, so applications can observe already available
completions while diagnostics remain queued. Further MQTT progress still requires
queue capacity. There is no total order between event and completion observation.

New packet details are immutable event-owned snapshots. Queue/staging clones share
storage; the disabled path does not copy packet properties for this feature.
Native terminal notices retain their existing independent ownership; no packet
cache or cross-channel matching history is introduced. Raw AUTH selection may
copy properties once to preserve both the existing lifecycle and raw event.
Properties and AUTH contents are redacted from automatic logs and Debug output.

C uses `rumqttc_event_protocol_observation()` with
`RUMQTTC_PROTOCOL_OBSERVATION_DETAILS_INIT`. The metadata accessor accepts new
incoming observations and outgoing events; coarse outgoing events return OK with
`present=0`. Other kinds return `INVALID_STATE`. `properties_available` distinguishes
unavailable contents from an available packet whose property bag is absent.
Result-count presence similarly distinguishes unavailable from an empty list.
Property accessors return `INVALID_STATE` when contents are unavailable; absent
properties otherwise return OK with an absent view or zero count. AUTH accessors
require an incoming AUTH observation.

String/binary views remain valid until event destruction, including after client
close/destruction. Use `rumqttc_string_copy()` and `rumqttc_bytes_copy()` for
independent copies. Accessors initialize supplied outputs before validation;
all-NULL optional outputs and invalid indices return `INVALID_ARGUMENT`. Metadata
writes respect the caller's declared size and preserve future extension bytes.

See [the C example](c/examples/protocol_observations.c) and
[allocation/retention measurements](wrapper-core/benches/protocol-observations.md).
