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

The built-in crate provides the `SessionStore` trait, scoped `SessionStoreKey`,
persisted data model, and canonical `PersistedSession::encode`/`decode` helpers.
Applications own file/database layout, encryption, and crash-consistent writes.
Use `MqttOptions::set_session_store_scope(...)` when a store is shared by
multiple brokers, tenants, environments, or connection profiles that may reuse
the same MQTT Client Identifier.

Exactly one active `EventLoop` may own and modify a session-store key at a time.
The core `SessionStore` API does not provide leases, fencing, compare-and-swap,
or active/passive failover coordination. `save` and `clear` must be atomic:
after success, and even after cancellation with indeterminate completion status,
a later `load` must see either the complete previous state or the complete new
state, never a torn checkpoint.

`SessionStore` persists MQTT protocol recovery state that has already been
admitted into the client state machine. This includes in-flight QoS flows,
packet-ID ownership and progress, SUBSCRIBE/UNSUBSCRIBE state, and incoming QoS
2 state. Requests marked for protocol replay keep their packet IDs and replay
semantics after restoration.

It is not a durable application outbox. Requests accepted by the client but not
yet admitted into MQTT protocol state remain recoverable across ordinary live
reconnects while the same `EventLoop` remains alive, but they are not persisted.
They may be lost if the process exits, crashes, or the `EventLoop` is dropped.
Applications that require every submitted request to survive process restart
must maintain their own durable outbound queue.

## Broker-Only Session Resume

MQTT 5 strict mode rejects a broker response that reports `Session Present = 1`
when the local client did not restore matching session state. Applications that
intentionally accept broker-only subscription resume can opt into the documented
compatibility policy, but cannot recover lost local in-flight QoS state.
