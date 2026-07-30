# MQTT 3.1.1 Local In-flight Window and Connection State

## Goal

Keep the MQTT 3.1.1 configured in-flight limit as a local outgoing QoS 1/2
PUBLISH window, preserve control-packet progress when that window is full, and
reconstruct its connection-local occupancy correctly across reconnects and
persistent-session restore.

MQTT 3.1.1 has no negotiated Receive Maximum, Server Keep Alive, Maximum Packet
Size, Retain Available, or Topic Alias Maximum. It also has no client-advertised
incoming publish limit. Do not add MQTT 5 CONNACK properties, inbound
Receive-Maximum enforcement, or corresponding protocol errors to the v4 client.

## Required Work

- Audit both the event-loop scheduler and the public low-level `MqttState`
  transition path so the configured in-flight limit gates only outgoing QoS 1
  and QoS 2 PUBLISH packets.
- Allow PUBACK, PUBREC, PUBREL, PUBCOMP, PINGREQ, SUBSCRIBE, UNSUBSCRIBE, and
  DISCONNECT packets to proceed while the outgoing publish window is full,
  subject to packet-identifier availability and their own protocol state.
- Keep QoS 1 publishes in the window until PUBACK and QoS 2 publishes in the
  window until PUBCOMP. Unlike MQTT 5, MQTT 3.1.1 PUBREC has no rejecting reason
  code that releases an outgoing publish slot.
- Reset derived connection-local window occupancy when a network connection is
  cleaned up, then reacquire slots as resumable QoS 1/2 PUBLISH or PUBREL work
  is replayed on the next connection.
- Preserve MQTT session state required for `Clean Session = 0`, including
  unfinished outgoing QoS 1/2 exchanges, PUBREL state, original packet
  identifiers, and incomplete incoming QoS 2 processing.
- Keep derived connection state such as the current in-flight count, scheduler
  blockage, collision bookkeeping, and acknowledgement-progress bookkeeping
  out of persistent checkpoints unless a field is independently required to
  reconstruct MQTT session state.
- Continue treating the configured maximum in-flight value as checkpoint
  compatibility metadata rather than a broker-negotiated or connection-scoped
  MQTT value.

## Tests

- Exercise the local outgoing window at, below, and above its boundary for QoS
  1 and QoS 2 PUBLISH packets through both the event loop and public low-level
  state transitions.
- Verify slot release on PUBACK and PUBCOMP, and verify that PUBREC advances QoS
  2 to PUBREL without releasing the slot.
- Verify every non-PUBLISH control-packet class can make progress past a
  window-blocked PUBLISH when its own packet identifier and protocol state
  permit it.
- Reconnect with `Clean Session = 0` and `Session Present = 1`; verify replay
  uses the original packet identifiers and correct DUP behavior while rebuilding
  the local window occupancy.
- Reconnect with `Session Present = 0` or `Clean Session = 1`; verify stale
  window occupancy and incompatible session work are discarded.
- Restore a persistent session and verify that derived connection-local
  occupancy is reconstructed from replayed work rather than restored as a
  checkpoint counter.

## Completion Criteria

- The local in-flight window cannot block MQTT control traffic needed for
  acknowledgement, keepalive, subscription management, or disconnect progress.
- MQTT-4.3.2-1, MQTT-4.3.3-1, MQTT-4.4.0-1, and the MQTT 3.1.1 Session State
  rules are preserved while reconnecting and restoring persistent sessions.
- No MQTT 5 negotiated-limit or inbound Receive-Maximum behavior is exposed by
  the MQTT 3.1.1 client.
- Targeted tests and `cargo test -p rumqttc-v4-next` pass.
- Any user-visible behavior changes are recorded in `CHANGELOG.md`, and
  `rumqttc-v4/design.md` documents the final local-window and checkpoint
  boundaries.
