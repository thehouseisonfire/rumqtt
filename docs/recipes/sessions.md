# Session and Reconnect Recipes

rumqttc reconnects automatically when the application keeps polling
`connection.iter()` or `eventloop.poll()`.

## Resubscribe After Reconnect

After every successful connection, the event loop yields an incoming CONNACK. If
`session_present` is false, the broker has no retained subscription state for
the client, so reissue the desired subscriptions.

Compile-checked examples:

- v4: `rumqttc-v4/examples/resubscribe_on_reconnect.rs`
- v5: `rumqttc-v5/examples/resubscribe_on_reconnect_v5.rs`

Keep the desired subscription list in application state. Do not rely on the
client object as the only source of subscription truth.

## Persistent Sessions

For restart-safe sessions, configure a stable client ID, broker-side persistent
session settings, and a local `SessionStore`.

Compile-checked examples:

- v4: `rumqttc-v4/examples/persistent_session_file_store.rs`
- v5: `rumqttc-v5/examples/persistent_session_file_store_v5.rs`

MQTT 3.1.1 uses `clean_session(false)`. MQTT 5 uses `clean_start(false)` plus a
non-zero session expiry interval or `SessionMode::Persistent`.

The built-in crate provides the `SessionStore` trait and persisted data model.
Applications own serialization, file/database layout, encryption, and
crash-consistent writes.

## Broker-Only Session Resume

MQTT 5 strict mode rejects a broker response that reports `Session Present = 1`
when the local client did not restore matching session state. Applications that
intentionally accept broker-only subscription resume can opt into the documented
compatibility policy, but cannot recover lost local in-flight QoS state.
