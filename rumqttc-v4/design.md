# MQTT 3.1.1 Client Design

This document contains the MQTT 3.1.1-specific design of the
`rumqttc-v4-next` package. Shared client, event-loop, scheduling, keepalive,
completion, persistence, and shutdown behavior is documented in the
[library design](../docs/design.md).

The version-specific implementation is concentrated in
[`src/mqttbytes/v4/`](src/mqttbytes/v4/), [`src/state.rs`](src/state.rs), and the
session reconciliation in [`src/eventloop.rs`](src/eventloop.rs).

## MQTT 3.1.1 Protocol Surface

MQTT 3.1.1 packets do not carry MQTT 5 properties or reason codes. CONNACK uses
a return code, SUBACK has per-filter return codes, and the remaining
acknowledgement packets identify the exchange without a reason-code field. The
state machine therefore treats a syntactically valid PUBACK, PUBREC, PUBREL, or
PUBCOMP primarily as progress for its packet identifier.

The outgoing in-flight publish limit is a local QoS 1/2 PUBLISH window, not a
broker-negotiated Receive Maximum and not a packet-identifier ceiling. Outgoing
publishes may use any currently unused non-zero 16-bit Packet Identifier.
QoS 1 occupies a window slot until PUBACK. QoS 2 keeps the same slot through
PUBLISH, PUBREC, and PUBREL, releasing it only at PUBCOMP; MQTT 3.1.1 PUBREC has
no rejecting reason code that can end the exchange early.

When the window is full, the event-loop scheduler holds later QoS 1/2 PUBLISH
work while allowing protocol-valid acknowledgements, keepalive, subscription
management, PUBREL retransmission, and disconnect processing to progress.
The public low-level `MqttState` transition enforces the same publish count.

## Clean Session and Session Present

MQTT 3.1.1 uses `Clean Session` to select the lifetime of session state:

- `clean_session = true` requests a new session and prevents local checkpoint
  restoration. A successful CONNACK with `Session Present = 1` is rejected as
  inconsistent.
- `clean_session = false` permits the broker and client to resume a session for
  a stable Client Identifier. `Session Present = 1` retains compatible local
  protocol state and replays unfinished work with its packet identifiers.
- `Session Present = 0` means the broker has no previous session. The event loop
  discards local session state and clears its stored checkpoint before
  continuing with the new session.

An empty Client Identifier cannot be combined with `clean_session = false`.
The CONNECT encoder and option validation reject that configuration because a
persistent MQTT 3.1.1 session requires a stable identity.

Incomplete incoming QoS 2 processing is part of the session and is retained
only when the session resumes. Incoming QoS 1 acknowledgement state is not
session state and is cleared at a connection boundary.

When the broker reports `Session Present = 0`, old packet identifiers cannot be
replayed as if the earlier session survived. The current implementation fails
tracked state discarded by this reset. A possible policy for reclassifying
eligible publications as new application work is tracked in
[`TODO6.md`](../TODO6.md), but it must preserve ambiguous-delivery semantics.

## Persistent Checkpoint Contents

V4 checkpoints record `clean_session`, the configured maximum in-flight value,
acknowledgement mode, replayable outbound exchanges with their original packet
identifiers and protocol order, and incomplete incoming QoS 2 state.
Restoration is enabled only for
`clean_session = false` and validates that the checkpoint belongs to the
configured Client Identifier and compatible local options.

The maximum in-flight value is checkpoint compatibility metadata. Current
window occupancy, packet-identifier allocation cursors, acknowledgement
frontiers, collision state, ping state, and scheduler blockage are
connection-local and are not serialized. Cleanup resets occupancy to zero;
replayed PUBLISH and PUBREL work reacquires slots on the next connection.
Checkpoint format version 2 removes the former allocator and acknowledgement
frontier fields, so version 1 canonical checkpoints are rejected.

The separately returned live QoS 1/2 PUBLISH uses `DUP=0`, as required by the
first-transmission obligations MQTT-4.3.2-1 and MQTT-4.3.3-1. Persisted and
restored recovery PUBLISH packets use `DUP=1`, consistent with the
retransmission obligation MQTT-3.3.1-1. The strict first-transmission
qualification is limited to persistent recovery and is documented in
[Persistent recovery and the DUP flag](../docs/design.md#persistent-recovery-and-the-dup-flag).
QoS 2 PUBREL and terminal completion retain their independent durability
barriers.
