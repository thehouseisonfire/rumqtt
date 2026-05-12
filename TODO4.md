# TODO4: Persistent MQTT 5 Client State and Restart-Safe Resume

## Goal

Add true restart-safe MQTT 5 session resume for `rumqttc-v5`.

Today rumqttc can resume a persistent session only while the same `EventLoop`
keeps local session state in memory. If the process restarts or a new
`EventLoop` is constructed, local state is lost. The opt-in compatibility mode
`allow_broker_session_resume_without_local_state` can accept broker-only
session reuse, but that is explicitly non-strict MQTT 5 behavior because the
client cannot reconcile lost in-flight QoS/session state.

The target feature is strict MQTT 5 restart-safe resume: persist enough client
session state before shutdown/crash so a later process can restore it and safely
accept `Session Present = 1`.

## Required State

Persist at least:

- Client identity and session settings: `client_id`, `clean_start`,
  `session_expiry_interval`, protocol version, and any settings that affect
  session interpretation.
- Outbound QoS 1 publishes awaiting `PUBACK`, including packet identifier,
  topic, payload, properties, retain flag, DUP state, and notice/recovery
  metadata that can be represented after restart.
- Outbound QoS 2 state, including whether the client is waiting for `PUBREC`,
  waiting to send or resend `PUBREL`, or waiting for `PUBCOMP`.
- Packet identifiers currently owned by publish, pubrel, subscribe, and
  unsubscribe flows.
- Tracked subscribe/unsubscribe requests that were sent but not acknowledged,
  if the public API chooses to recover them after restart.
- Incoming QoS 2 state where the client has received `PUBLISH` and owes the
  next handshake packet.
- Replay queue ordering and throttling state needed to preserve MQTT retry
  semantics after reconnect.

Do not persist connection-scoped state:

- Server-to-client and client-to-server topic alias mappings.
- Broker `Receive Maximum`, `Maximum Packet Size`, `Topic Alias Maximum`, and
  other CONNACK-scoped limits from an old network connection.
- Authentication exchange state unless the design explicitly supports
  restartable enhanced authentication.

## Design Requirements

- Default behavior must remain strict MQTT 5 and must not silently accept
  broker-only session reuse after local state loss.
- Persistence must be explicit and opt-in; existing users should not get disk
  writes by default.
- The storage interface should be backend-neutral. Start with a trait or small
  adapter boundary so file, embedded database, and application-provided storage
  can be added without wiring one storage choice into the event loop.
- Writes must be crash-consistent. A restored session must be either the last
  complete state checkpoint or no state, never a partially written state.
- Persist before acknowledging success to the application when losing the state
  would make the protocol recovery ambiguous.
- Restored packet identifiers must not collide with broker-side in-flight
  state.
- Restored outbound QoS 1/2 packets and `PUBREL` packets must be resent with
  original packet identifiers and correct DUP handling when reconnecting with
  `clean_start=false` and `Session Present = 1`.
- If CONNACK returns `Session Present = 0`, discard persisted state for that
  client/session before continuing.
- If the configured `client_id` or session-affecting options no longer match
  the persisted state, reject the restore or discard it using a documented
  policy.

## API Shape To Consider

Possible direction:

```rust
pub trait SessionStore {
    type Error;

    fn load(&self, client_id: &str) -> Result<Option<PersistedSession>, Self::Error>;
    fn save(&self, session: &PersistedSession) -> Result<(), Self::Error>;
    fn clear(&self, client_id: &str) -> Result<(), Self::Error>;
}
```

Then expose configuration through `MqttOptions` or the client builder, for
example:

```rust
MqttOptions::builder("client", "localhost")
    .clean_start(false)
    .session_expiry_interval(Some(3600))
    .session_store(store)
    .build();
```

The final API should account for async storage if needed. Avoid committing to a
blocking trait if it would force file or database I/O onto the event loop.

## Testing Plan

Add tests that cover:

- Same-process reconnect still works without a persistent store.
- First connection with restored local state accepts broker `Session Present = 1`.
- First connection without restored local state rejects broker
  `Session Present = 1` in strict mode.
- QoS 1 outbound publish is restored and resent with the original packet ID
  after restart.
- QoS 2 outbound handshake is restored in each phase: waiting for `PUBREC`,
  waiting to send/resend `PUBREL`, and waiting for `PUBCOMP`.
- Incoming QoS 2 state is restored and completes correctly after reconnect.
- Persisted state is discarded when CONNACK returns `Session Present = 0`.
- Persisted state is rejected or discarded when `client_id` or incompatible
  session options change.
- Corrupt, partial, or version-mismatched persisted state does not lead to
  accepting an unsafe resume.
- Topic aliases are not persisted across network connections.
- Broker limits from the old CONNACK are not reused after reconnect.

## Open Questions

- Should storage be synchronous, asynchronous, or support both through separate
  adapters?
- How should notices/promises be represented after restart, given that original
  application tasks no longer exist?
- Should subscribe/unsubscribe requests be restored automatically, failed, or
  surfaced to the application for recovery?
- What is the minimum stable serialized format, and how should migrations be
  handled across crate versions?
- Should the first implementation include a built-in file store, or only the
  storage trait plus tests with an in-memory durable test double?

