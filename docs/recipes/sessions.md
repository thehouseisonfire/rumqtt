# Session and Reconnect Recipes

rumqttc reconnects automatically when the application keeps polling
`connection.iter()` or `eventloop.poll()`.

## Resubscribe After Reconnect

After every successful connection, the event loop yields an incoming CONNACK.
Reissue desired subscriptions when the effective session is fresh:
`!eventloop.diagnostics().session.connack.unwrap().session_resumed`.
The raw `connack.session_present` reports what the broker sent. For MQTT 3.1.1,
`eventloop.mqtt_options.clean_session() || !connack.session_present` is the
equivalent condition when evaluated with the options used for that connection.
A clean connection must resubscribe even if a broken broker reports Session
Present and the connection was accepted through `AcceptAsClean`.

Compile-checked examples:

- v4: `rumqttc-v4/examples/resubscribe_on_reconnect.rs`
- v5: `rumqttc-v5/examples/resubscribe_on_reconnect_v5.rs`

Keep the desired subscription list in application state. Do not rely on the
client object as the only source of subscription truth.

## Persistent Sessions

For restart-safe sessions, configure a stable client ID, broker-side persistent
session settings, and a local `SessionStore`.

Compile-checked examples:

- v4: `session-store-file/adapter/examples/persistent_session_file_store_v4.rs`
- v5: `session-store-file/adapter/examples/persistent_session_file_store_v5.rs`
- both: `session-store-file/adapter/examples/persistent_session_file_store_dual.rs`

```bash
cargo check --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v4 \
  --example persistent_session_file_store_v4
cargo check --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v5 \
  --example persistent_session_file_store_v5
cargo check --manifest-path session-store-file/Cargo.toml \
  -p rumqttc-session-store-file-next --no-default-features --features v4,v5 \
  --example persistent_session_file_store_dual
```

For a production-oriented Unix or Windows implementation, use
`rumqttc-session-store-file-next` with its `v4`, `v5`, or combined `v4,v5`
features. The stores require an existing, trusted, dedicated root and create
separate `v4`/`v5` namespaces beneath it. Protocol keys and session values are
not interchangeable even when both features are enabled.
Their versioned envelope and hashed filenames are intentionally incompatible
with former example files. An exact old filename is reported but never
migrated. See [session-store recovery](session-store-recovery.md) before
discarding local state or reusing an old root.

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

## MQTT 3.1.1 Session Present Interoperability

The default `SessionPresentMismatchPolicy::Error` rejects a successful CONNACK
with both `clean_session=true` and raw `session_present=true`. A broker returning
this combination violates its obligation under MQTT-3.2.2-1. MQTT 3.1.1 section
3.2.2.2 permits clients to continue or disconnect when Session Present is
unexpected.

To handle this specific broker defect, configure
`options.protocol_compatibility_mut().set_session_present_mismatch(SessionPresentMismatchPolicy::AcceptAsClean)`.
This applies only to a successful MQTT 3.1.1 CONNACK on a clean connection.
rumqttc resolves it as fresh, discards old local protocol/replay state, fails
old tracked notices with `SessionReset`, and clears the current scope/client-ID
checkpoint before reporting success. Clean sessions do not ordinarily load or
save checkpoints, but a checkpoint from an earlier persistent connection must
still be cleared. Failed or cancelled clearing leaves the clear obligation
pending; subsequent polling retries it before any checkpoint can load.

The broker remains non-conforming. The incoming CONNACK still exposes raw
Session Present=true; `diagnostics().session.connack` separately reports
`raw_session_present=true`, `session_resumed=false`, and
`SessionPresentMismatchAcceptedAsClean`. Resubscribe for this fresh session.
This option does not relax malformed packets, refused connections, or other
validation.

MQTT 5 has no `AcceptAsClean` option. Its MQTT-3.2.2-4 client requirement mandates
closing when local Session State is absent and Session Present=1. The existing
explicitly non-strict `BrokerSessionResumePolicy::AllowBrokerOnly` retains its
restrictions and is now configured canonically through
`options.protocol_compatibility_mut().set_broker_session_resume_policy(...)`.
Existing getter, setter, and builder APIs continue forwarding to the same value.
Clean Start with Session Present=1 is always rejected.
