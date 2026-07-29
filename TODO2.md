# MQTT 5 Broker Capability Enforcement

## Goal

Honor every capability restriction advertised by the broker in CONNACK before
sending affected application packets.

## Required Work

- Validate CONNACK capability-property values and reject malformed values with
  the appropriate MQTT 5 protocol error.
- Enforce Maximum QoS for outgoing PUBLISH packets, including queued work and
  reconnect replay (MQTT-3.2.2-11).
- Enforce Retain Available for all outgoing retained PUBLISH paths.
- Enforce Wildcard Subscription Available, Subscription Identifier Available,
  and Shared Subscription Available for outgoing SUBSCRIBE packets.
- Apply restrictions per connection and restore permissive defaults when the
  next CONNACK omits them.
- Return structured local failures to tracked operations without terminating a
  valid connection when the application request itself is unsupported.

## Tests

- Cover allowed, disallowed, queued, tracked, and replayed requests for every
  advertised restriction.
- Cover malformed property values and duplicate properties at the codec and
  connection boundaries.
- Reconnect from a restrictive broker to a broker that omits each property and
  verify that stale restrictions are cleared.

## Completion Criteria

- No decoded CONNACK capability property is merely informational when it
  constrains client output.
- Targeted tests and `cargo test -p rumqttc-v5-next` pass.
- Public failure types and behavior are documented and added to `CHANGELOG.md`.
