# MQTT 5 Client Design

This document contains the MQTT 5-specific design of the `rumqttc-v5-next`
package. Shared client, event-loop, scheduling, keepalive, completion,
persistence, and shutdown behavior is documented in the
[library design](../docs/design.md).

The version-specific implementation is concentrated in
[`src/mqttbytes/v5/`](src/mqttbytes/v5/), [`src/state.rs`](src/state.rs), the
enhanced-authentication lifecycle in [`src/auth.rs`](src/auth.rs), and session
reconciliation in [`src/eventloop.rs`](src/eventloop.rs).

## Connection Negotiation

A successful CONNACK can alter the current connection or session:

| MQTT 5 value | Implemented effect |
| --- | --- |
| Server Keep Alive | Replaces the keepalive interval used by the event loop. |
| Receive Maximum | Caps outgoing QoS 1/2 PUBLISH packets to the smaller broker or client limit. |
| Maximum Packet Size | Makes the encoder reject packets larger than the server accepts. |
| Topic Alias Maximum | Limits client-to-server aliases for this network connection. |
| Session Expiry Interval | Becomes the effective interval used for current-session persistence decisions. |
| Retain Available | Causes unsupported retained publishes to be rejected locally. |
| Assigned Client Identifier | Replaces an empty identifier when the server supplies a valid assignment. |

The client can advertise its Receive Maximum, Maximum Packet Size, and Topic
Alias Maximum in CONNECT. Configured incoming packet-size limits are enforced
by the decoder. The implementation enforces the broker's outgoing packet-size
and publish-quota limits.

The configured CONNECT Session Expiry Interval remains the application's
baseline request for every connection. A broker value returned in CONNACK is
tracked separately as the effective interval for the current connection and is
used for checkpoint and connection-closure decisions; it does not replace the
value requested on a later reconnect.

Baseline restoration and enforcement of all connection-scoped limits in both
directions remain under audit in [`TODO6.md`](../TODO6.md). Server Keep Alive,
broker Receive Maximum, Maximum Packet Size, Retain Available, and Topic Alias
Maximum must not become durable session checkpoint state. The effective Session
Expiry Interval is different: it is session-lifetime metadata and is
deliberately stored in the V5 checkpoint, including a CONNACK override.

## Connection-Scoped and Session-Scoped State

MQTT 5 assigns different lifetimes to closely related state:

- The socket, Server Keep Alive, Receive Maximum send quota, Maximum Packet
  Size, retained-message capability, and topic-alias mappings belong to a
  network connection.
- Packet-identifier ownership, incomplete QoS 1 and QoS 2 exchanges, pending
  SUBSCRIBE and UNSUBSCRIBE exchanges, and incomplete incoming QoS 2 processing
  belong to the MQTT session.
- Session Expiry Interval governs how long session state can survive after the
  network connection ends.

Connection cleanup explicitly resets topic-alias and authentication exchange
state. The remaining negotiated-value reset behavior is included in the audit
described above.

## Topic Alias Lifecycle

Topic aliases never cross a network-connection boundary, even when
`Session Present = 1`. Incoming and outgoing alias maps, automatic allocation
state, and the broker-advertised maximum are reset for a new connection.

Before replay, an aliased publish is restored to a full topic name and its alias
is removed. An alias-only publish whose topic cannot be recovered is failed with
`TopicAliasReplayUnavailable` instead of being sent with an invalid empty topic.
Explicit and automatic aliases are checked against the maximum advertised by
the opposite endpoint.

## Clean Start, Session Expiry, and Session Present

These fields have separate roles:

- `Clean Start = 1` asks the broker to discard an earlier session and begin a
  new one for this connection.
- `Clean Start = 0` permits compatible broker and client state to resume.
- Session Expiry Interval controls retention after the network connection
  closes. A zero effective interval means no session remains after disconnect.
- `Session Present` reports whether a successful CONNACK resumed a prior
  session.

The event loop rejects `Session Present = 1` with `Clean Start = 1`. With
`Clean Start = 0`, strict mode also rejects a broker-only resume when no matching
local session state exists. `BrokerSessionResumePolicy::AllowBrokerOnly` is an
explicit compatibility mode. While active, requests that allocate client
packet identifiers are rejected because the client cannot reconstruct their
ownership safely from broker state alone.

When `Session Present = 0`, local session state and its persisted checkpoint are
discarded before the connection continues. When it is `1`, compatible
unfinished PUBLISH, PUBREL, SUBSCRIBE, and UNSUBSCRIBE exchanges retain their
packet identifiers and are replayed according to MQTT rules.

The separate live QoS 1/2 PUBLISH uses `DUP=0`, as required by the
first-transmission obligations MQTT-4.3.2-2 and MQTT-4.3.3-2. Persisted and
restored recovery PUBLISH packets use `DUP=1`, consistent with the
retransmission obligation MQTT-3.3.1-1. The strict first-transmission
qualification is limited to persistent recovery and is documented in
[Persistent recovery and the DUP flag](../docs/design.md#persistent-recovery-and-the-dup-flag).
Topic aliases remain connection-scoped, and QoS 2 PUBREL and terminal
completion keep their independent durability barriers.

The store is also cleared after a graceful DISCONNECT whose effective Session
Expiry Interval is zero. A Session Expiry Interval on the client DISCONNECT
overrides the CONNECT or CONNACK-derived value for that transition.

## Reason Codes and Completion

MQTT 5 acknowledgement reason codes are application-visible protocol results,
not merely transport errors. The shared completion milestones carry richer V5
results:

- Tracked QoS 1 completion returns the complete PUBACK and its reason code.
- A rejecting PUBREC is a distinct terminal QoS 2 result because PUBREL and
  PUBCOMP do not follow.
- Tracked subscribe and unsubscribe completion returns the complete SUBACK or
  UNSUBACK, including per-filter reason codes and properties.

A server DISCONNECT reason and reason string are surfaced as connection failure
context. Protocol-invalid inbound data is classified where possible, and the
network path attempts the corresponding MQTT 5 DISCONNECT reason before
closing.

## Enhanced Authentication

MQTT 5 permits AUTH packets during the initial connection and for
re-authentication. The state machine separates those exchange lifecycles,
prevents overlapping re-authentication, validates Authentication Method
consistency, and completes or fails tracked authentication notices when the
exchange succeeds, disconnects, resets, or encounters a protocol error.

The crate exposes a generic `Authenticator` interface and an optional SCRAM
implementation.
