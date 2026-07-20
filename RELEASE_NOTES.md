# rumqttc-next 0.34.0-alpha

This alpha is a substantial update to bothclients, focusing on durable session
recovery, observability, safer configuration, and stricter protocol behavior.
It contains intentional breaking API changes, so make sure to read the [migration guide](https://github.com/thehouseisonfire/rumqtt/blob/rumqttc-next-0.34.0-alpha/MIGRATION.md)
before upgrading.

## Highlights

- **Unified publishing:** all publish variants now use
  `publish(topic, payload, PublishOptions)`. Common string and byte containers
  work directly through `IntoPublishPayload`, which consumers may also
  implement for their own types. This allowed for API consolation and should
  be more ergonomic for informing options across functions and for files with
  large a amount of calls, if more verbose initially.

  ```rust
  client
      .publish(
          "devices/1/status",
          "online",
          PublishOptions::at_least_once().retained(),
      )
      .await?;
  ```

- **Persistent session recovery:** Clients can checkpoint and restore local
  QoS/session state across newly constructed clients and process restarts.
  Applications should provide the durable `SessionStore` backend, as rumqttc does
  not include a built-in file store. A working reference implementation with be
  provided as an optional crate in the future. This is protocol recovery state,
  not a durable outbox, and a session-store key must have only one active
  event-loop owner.

- **Production observability:** opt-in structured lifecycle tracing, synchronous
  runtime diagnostics snapshots, and an optional `Stream` to interface make
  event loops easier to monitor and integrate. Tracing does not currently
  include client IDs, broker URLs, topics, payloads, credentials, and user
  properties. Refer to recipes documentation for more information.

- **Safer configuration and errors:** new fallible builders and TLS defaults
  catch configuration failures before connecting. Client errors now distinguish
  channel backpressure, disconnected event loops, and invalid local requests.
  Reusable validated topic filters and multi-unsubscribe APIs reduce repeated
  validation work.

## Protocol and reliability

This release tightens MQTT validation throughout both codecs and state
machines. It prevents packet-identifier collisions, rejects unsolicited or
zero-identifier acknowledgements, validates UTF-8 conformance, and reports
oversized fields instead of panicking. Graceful disconnect, keepalive,
reconnect, and session-persistence edge cases received substantial fixes.

MQTT 5 additionally gains stricter topic-alias, assigned-client-ID, session
expiry, retained-publish, maximum-packet-size, CONNECT/CONNACK property, and
DISCONNECT encoding behavior.

## Breaking-change checklist

- Migrate publish calls to `PublishOptions` and the new payload argument order.
- Replace manual-ack booleans with `AckMode::{Automatic, Manual}`.
- On MQTT 5, use `BrokerSessionResumePolicy` and `TopicAliasPolicy` instead of
  booleans, and rename `Filter` to `SubscribeFilter`.
- Treat `PingReq` and `PingResp` packet/request variants as unit variants.
- Update matches for the new structured `ClientError` variants and
  `ProtocolViolation` state errors.
- MQTT 5 codec helpers now live directly under `mqttbytes`; packet-level types
  remain re-exported from `mqttbytes::v5`.
- Handle the new `Result` from MQTT string/binary write helpers.

Given the large amount of changes, this version is published as an alpha
release. Make sure to test this well before committing it to production and
report any issues you may run into.
