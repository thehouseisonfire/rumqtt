# MQTT 5 Negotiated Limits and Connection State

## Goal

Make every MQTT 5 negotiated limit apply only to its network connection and
enforce Receive Maximum in both directions.

## Required Work

- Separate configured baselines from values negotiated in CONNACK for Server
  Keep Alive, Receive Maximum, Maximum Packet Size, Retain Available, and Topic
  Alias Maximum.
- Restore each configured baseline before processing every CONNACK; an omitted
  property must not retain a value from the previous connection.
- Reject a broker that exceeds the Receive Maximum advertised by the client.
- Keep broker Receive Maximum enforcement limited to unacknowledged outgoing
  QoS 1 and QoS 2 PUBLISH packets.
- Allow non-PUBLISH control packets to proceed when the outgoing publish quota
  is exhausted.
- Keep connection-scoped values and topic-alias mappings out of persistent
  session checkpoints.

## Tests

- Reconnect successively to brokers that advertise smaller, larger, and omitted
  values for every connection-scoped property.
- Exercise incoming and outgoing Receive Maximum boundaries, including release
  on PUBACK, PUBCOMP, and rejecting PUBREC.
- Verify control-packet progress while the outgoing publish quota is exhausted.
- Verify persistent-session restore does not restore connection-scoped values.

## Completion Criteria

- MQTT-3.3.4-7 through MQTT-3.3.4-10 are covered for all client-applicable
  behavior.
- Targeted tests and `cargo test -p rumqttc-v5-next` pass.
- User-visible behavior changes are recorded in `CHANGELOG.md`, and
  `rumqttc-v5/design.md` no longer describes this audit as unfinished.
