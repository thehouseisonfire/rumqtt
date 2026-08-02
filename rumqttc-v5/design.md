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
| Receive Maximum | The broker's CONNACK value caps outgoing QoS 1/2 PUBLISH packets; the client's CONNECT value independently caps incoming QoS 1/2 PUBLISH packets. |
| Maximum Packet Size | Makes the encoder reject packets larger than the server accepts. |
| Topic Alias Maximum | Limits client-to-server aliases for this network connection. |
| Session Expiry Interval | Becomes the effective interval used for current-session persistence decisions. |
| Retain Available | Causes unsupported retained publishes to be rejected locally. |
| Assigned Client Identifier | Replaces an empty identifier when the server supplies a valid assignment. |

The client can advertise its Receive Maximum, Maximum Packet Size, and Topic
Alias Maximum in CONNECT. Configured incoming packet-size limits are enforced
by the decoder. The implementation enforces both Receive Maximum directions,
including quota release at the protocol-defined acknowledgement milestones,
while allowing non-PUBLISH control packets to progress when the outgoing
publish quota is exhausted.

## Packet Identifiers and Send Quota

Outgoing PUBLISH, PUBREL, SUBSCRIBE, and UNSUBSCRIBE flows share the MQTT
packet-identifier namespace `1..=65,535`. The configured outgoing inflight
upper limit and the broker's Receive Maximum do not restrict identifier values;
they limit only the number of QoS 1/2 PUBLISH packets consuming send quota. The
effective limit for a connection is the smaller of those two values.

A QoS 1 PUBLISH releases quota and its identifier on PUBACK. A successful QoS 2
exchange retains both through PUBREC and PUBREL, releasing them on PUBCOMP. A
PUBREC reason code of `0x80` or greater terminates the exchange and releases
both immediately. Sending or retransmitting PUBREL never consumes another
quota slot. In particular, a restored PUBREL reserves its durable session
identifier but consumes no quota on the new connection.

Send quota, the negotiated Receive Maximum, and the allocator cursor are
connection-local runtime state. They reset for every network connection and are
not stored in session checkpoints. A restored PUBLISH backlog is therefore
replayed gradually under the newly negotiated effective limit, which may be
smaller or larger than on the previous connection.

The configured CONNECT Session Expiry Interval remains the application's
baseline request for every connection. A broker value returned in CONNACK is
tracked separately as the effective interval for the current connection and is
used for checkpoint and connection-closure decisions; it does not replace the
value requested on a later reconnect.

The configured baselines for Server Keep Alive and the client-advertised
limits remain unchanged across reconnects. Before each CONNACK is applied,
connection-scoped negotiated values return to their MQTT defaults. Server Keep
Alive, broker Receive Maximum, Maximum Packet Size, Retain Available, Topic
Alias Maximum, quotas, and alias mappings do not become durable session
checkpoint state. The effective Session Expiry Interval is different: it is
session-lifetime metadata and is deliberately stored in the V5 checkpoint,
including a CONNACK override.

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

Connection cleanup resets topic-alias and authentication exchange state,
negotiated limits, and both connection-local Receive Maximum quotas.

The v5 checkpoint model and canonical codec are version 2. Checkpoints contain
ordered replayable PUBLISH, PUBREL, SUBSCRIBE, and UNSUBSCRIBE work, incomplete
incoming QoS 2 state, session-expiry metadata, and configuration compatibility
fields. They do not contain allocator cursors, acknowledgement frontiers,
quota, negotiated Receive Maximum, or other connection-local counters. Version
1 canonical checkpoints are intentionally incompatible and are rejected rather
than migrated or reinterpreted.

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

## Broker Redirect Boundaries

CONNACK and DISCONNECT reason codes `Use Another Server` and `Server Moved` are
correlated with Server Reference before they reach generic connection-failure
handling. Disabled, rejected, invalid, looping, exhausted, and failed redirects
retain a `RedirectOutcome` containing the reason, raw reference, and source
packet. Accepted policy decisions yield `Event::Redirect`; connection to the
target starts on the next event-loop poll.

The authority validator accepts the host, optional port, and bracketed IPv6
forms suggested by MQTT 5 section 4.11. Schemes and URI components are rejected,
and a policy can only select one of the validated advertised authorities. SRV
names remain visible to policy but fail as unsupported before applying a
transport default port, because the connector does not resolve DNS SRV targets.
Normalized host and port identities plus a non-zero policy limit bound each
redirect chain.

A target is transactional: it is not committed until its successful CONNACK.
Failure restores the previous connection options and reports both the redirect
and target connection error. `Use Another Server` remains temporary and
restores its predecessor after the target connection ends. `Server Moved`
commits its target for the remaining lifetime of that `EventLoop`.

An isolated target is a new security and session identity. Recognized CONNECT,
enhanced-authentication, proxy, and websocket-header credentials are cleared;
TLS configuration is explicitly supplied and verifies the target hostname; a
fresh Client Identifier and clean zero-expiry session are used; and the old
checkpoint remains stored under its old key. Pending tracked operations fail
with redirect-specific notice errors rather than crossing the boundary.
Applications can explicitly supply authentication, reuse a Client Identifier,
or select a session-store scope. Live MQTT state is preserved only when both
Client Identifier and store scope remain identical and session reuse was
explicitly selected.

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
