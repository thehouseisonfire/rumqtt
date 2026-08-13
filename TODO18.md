# `mqtt-typed-client` File-Session-Store Recipe

## Goal

Add and continuously verify one explicit, copyable recipe showing how an
application can use `rumqttc-session-store-file-next` through
`mqtt-typed-client`'s `rumqttc-next` backend API.

The recipe must be added only after a released `mqtt-typed-client` version
supports `rumqttc-v4-next` and `rumqttc-v5-next` `0.34.0` (or a compatible
later stable release). At the time this TODO was written,
`mqtt-typed-client 0.4.1` exposes the required `backend-rumqttc-next` and
`unstable-backend-api` features, but its backend depends on rumqttc-next
`0.33.3`. That release does not expose the `SessionStore` API used by the file
adapter. Do not publish a recipe that relies on incompatible duplicate backend
types or a private dependency patch.

Use the actual package and crate names throughout:

- Cargo package: `rumqttc-session-store-file-next`
- Rust crate: `rumqttc_session_store_file`
- Backend packages: `rumqttc-v4-next` and `rumqttc-v5-next`

“session-file-store” may be used as a descriptive phrase, but it is not the
Cargo package name.

## Upstream compatibility gate

Before implementing the recipe:

1. Identify the first published `mqtt-typed-client` release whose
   `backend-rumqttc-next` feature resolves to rumqttc-next `0.34.0` or later.
2. Confirm that it still exposes a supported way to mutate the raw backend
   options before the event loop is constructed. In `0.4.1` this is
   `ConnectionOptions::backend_tweak` behind `unstable-backend-api`; use the
   replacement documented by upstream if that API has changed.
3. Build a scratch dependency graph and prove that the backend and the adapter
   use one Cargo instance of each selected rumqttc-next protocol crate. The
   `SessionStore` implementation and `MqttOptions::set_session_store` must refer
   to identical Rust types.
4. Record the selected `mqtt-typed-client` version and its minimum supported
   Rust version in this file when implementation begins. If its MSRV is newer
   than the session-store workspace MSRV, keep the interoperability target in a
   stable-toolchain CI step and do not raise this workspace's MSRV solely for
   the recipe.

The gate is satisfied only if these commands show exactly one compatible v4
and v5 backend version, not parallel `0.33.x` and `0.34.x` copies:

```bash
cargo tree --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-consumer-tests -i rumqttc-v4-next
cargo tree --manifest-path session-store-file/Cargo.toml \
  -p session-store-file-consumer-tests -i rumqttc-v5-next
```

If `cargo tree` reports multiple versions, stop and update the upstream or
workspace dependency constraints. Do not work around the mismatch with casts,
newtype adapters, or unsafe code.

## Deliverables

### 1. Add a runnable MQTT 5 example

Add:

```text
session-store-file/adapter/examples/mqtt_typed_client_session_file_store_v5.rs
```

Register it in `session-store-file/adapter/Cargo.toml` with
`required-features = ["v5"]`. Add a version-pinned `mqtt-typed-client`
development dependency with default features disabled and only the features
the example needs. At minimum, select:

```toml
features = [
    "backend-rumqttc-next",
    "unstable-backend-api",
    "bincode",
]
```

Do not enable `backend-rumqttc`, TLS, WebSocket, proxy, or unrelated serializer
features in this TCP example. If upstream stabilizes a session-store setting
and no longer requires `unstable-backend-api`, use the stable API and omit that
feature.

The example must:

1. create an existing, trusted store root supplied by
   `RUMQTTC_SESSION_STORE`, with a temporary-directory fallback suitable for a
   local demonstration;
2. call `rumqttc_session_store_file::v5::SessionFileStore::open`;
3. construct `MqttClientConfig` explicitly rather than hiding persistence
   settings in a URL;
4. select MQTT 5 and a persistent session policy (`SessionPolicy::Resume` or
   its current upstream equivalent);
5. use a non-empty, stable client ID;
6. install the store through `ConnectionOptions::backend_tweak` and
   `BackendOptions::V5`, or the corresponding supported backend API;
7. set a stable, documented session-store scope and a non-zero MQTT 5 session
   expiry interval on the raw `rumqttc-v5-next::MqttOptions`;
8. reject or make unreachable the MQTT 3.1.1 backend variant instead of
   silently connecting without persistence;
9. connect, create at least one typed subscription, and perform a tracked QoS 1
   publish or another operation that creates meaningful persistent client
   session state;
10. keep polling through the `MqttConnection` handle and shut it down using the
    typed client's documented lifecycle API; and
11. explain in comments that the file store persists rumqttc's MQTT protocol
    session, not Rust handlers, typed subscriber channels, deserialized
    application data, or an application outbox. Typed subscriptions and
    handlers must be recreated after process restart.

The core backend attachment should remain recognizable as this pattern,
adjusted to the selected upstream release's exact API:

```rust,ignore
let store = SessionFileStore::open(&root).await?;

config.connection.session = SessionPolicy::Resume;
config.connection.protocol = ProtocolVersion::V5;
config.connection.backend_tweak(move |backend| match backend {
    BackendOptions::V5(options) => {
        options
            .set_session_store_scope("example-local-broker")
            .set_session_expiry_interval(Some(60 * 60))
            .set_session_store(store.clone());
    }
    BackendOptions::V4(_) => {
        unreachable!("the recipe explicitly selects MQTT 5")
    }
    _ => unreachable!("unsupported future backend variant"),
});
```

Do not copy this sketch without compiling it. In particular, import
`BackendOptions` and any raw backend types from `mqtt-typed-client`'s
version-matched backend re-export when upstream requires that; avoid adding a
second independently versioned rumqttc-next dependency merely to name those
types.

### 2. Add a concise README recipe

Add a “Using `mqtt-typed-client`” section to
`session-store-file/adapter/README.md`. It must contain:

- the complete dependency declaration, including `default-features = false`;
- the minimum supported `mqtt-typed-client` version;
- the complete backend-tweak snippet, not pseudocode;
- a link to the runnable example;
- the MQTT 3.1.1 equivalents (`v4`, `BackendOptions::V4`,
  `set_clean_session(false)`) even if the runnable example remains MQTT 5;
- the requirement that the typed client's persistent-session policy and the
  raw backend's session-store/expiry settings agree;
- the one-active-event-loop-per-store-key restriction;
- the requirement to recreate typed routing/subscription objects after a
  process restart; and
- a warning that `unstable-backend-api`, if still required, is explicitly
  semver-exempt and should be pinned and tested during dependency updates.

Keep the README snippet synchronized with the compiled example. Prefer
including the example source in documentation or testing a standalone consumer
target over maintaining two substantially different snippets.

### 3. Add a consumer-level compile target

Add the selected `mqtt-typed-client` dependency to
`session-store-file/consumer-tests/Cargo.toml` and add a small module or test
that constructs both of these configurations without contacting a broker:

- MQTT 3.1.1 + `rumqttc_session_store_file::v4::SessionFileStore` +
  `BackendOptions::V4`; and
- MQTT 5 + `rumqttc_session_store_file::v5::SessionFileStore` +
  `BackendOptions::V5`.

The test may open stores under `tempfile::TempDir`. It must exercise the public
APIs used by downstream applications and must not import private modules from
either project. Its purpose is to catch feature, type-identity, and API drift
for both protocols even though the user-facing runnable example focuses on
MQTT 5.

If upstream's backend tweak is applied only during connection construction and
cannot be exercised without opening a network connection, factor the public
configuration construction into a checked helper and compile it in the test;
do not add a flaky public-broker dependency to CI.

## CI verification

Update `.github/workflows/session-store-file-ci.yml` as follows.

### Required on every supported OS

In the existing stable `test` job, extend “Check documentation and examples”
or add a dedicated interoperability step that runs:

```bash
cargo check --locked \
  -p rumqttc-session-store-file-next \
  --example mqtt_typed_client_session_file_store_v5 \
  --features v5

cargo test --locked \
  -p session-store-file-consumer-tests \
  mqtt_typed_client_backend
```

Run the compile/test portion on Linux, macOS, and Windows. If the documentation
step remains excluded on Windows, place these commands in a separate step so
Windows still verifies the integration.

### Required dependency-identity assertion

On Ubuntu, add a shell assertion that fails if either protocol resolves more
than one rumqttc-next version. Use `cargo tree --duplicates` and/or parse
`cargo metadata --locked --format-version 1`; do not use a loose substring
check that can confuse package names with versions. The assertion must also
prove that the selected versions are at least `0.34.0` and are the versions
expected by the pinned `mqtt-typed-client` release.

Commit the updated `session-store-file/Cargo.lock`. All interoperability CI
commands must use `--locked` so an upstream dependency release cannot silently
change the verified recipe.

### MSRV handling

Do not add the interoperability target to the Rust 1.88 MSRV job unless the
selected `mqtt-typed-client` backend and all of its dependencies support Rust
1.88. The normal adapter MSRV check must remain intact. Document any newer
recipe-only compiler requirement in the README dependency section.

## Optional behavioral test

If the repository gains a hermetic MQTT broker fixture suitable for the
independent session-store workspace, add a restart test that:

1. connects with a stable client ID, MQTT 5 session expiry greater than zero,
   and the file store installed through the typed backend API;
2. creates an in-flight or tracked QoS operation and observes a checkpoint;
3. drops the first client/event loop to simulate process loss;
4. constructs a new typed client with the same store root, scope, and client
   ID;
5. receives `Session Present = 1` and resumes the local MQTT session without a
   session-state mismatch; and
6. recreates the typed subscription/handler and demonstrates delivery through
   the typed layer.

Do not use a public broker or timing-only sleeps in required CI. Until a
hermetic broker fixture exists, the cross-platform consumer test plus compiled
example is the required verification; the existing rumqttc and file-store
tests remain responsible for protocol-level persistence behavior.

## Documentation and release notes

- Add a user-facing entry to `session-store-file/CHANGELOG.md` when the recipe
  lands.
- If the integration requires a particular upstream pre-release, say so in the
  README and pin it exactly. Prefer waiting for a published compatible release
  rather than documenting a Git revision.
- Do not imply that `mqtt-typed-client` itself persists its routing tree or
  application payloads.
- Do not call the integration stable while it depends on an upstream API named
  `unstable-backend-api`; describe the file adapter as supported and the bridge
  as version-pinned/experimental.

## Acceptance criteria

This TODO is complete when all of the following are true:

- a released `mqtt-typed-client` dependency uses rumqttc-next `0.34.0` or a
  compatible later release;
- the MQTT 5 example builds from the independent session-store workspace;
- the README contains a copyable Cargo declaration and complete configuration
  snippet;
- public-API consumer tests compile both MQTT 3.1.1 and MQTT 5 backend/store
  combinations;
- CI runs those checks on Linux, macOS, and Windows with `--locked`;
- CI rejects duplicate or older rumqttc-next backend versions;
- the adapter's Rust 1.88 MSRV job still passes independently;
- the lockfile and changelog are updated; and
- the documentation clearly separates MQTT protocol-session persistence from
  typed-client routing and application-state persistence.

## Non-goals

- Persisting `mqtt-typed-client` handlers, subscriber channels, serializer
  state, or application messages.
- Treating the file store as a transactional application outbox.
- Supporting two active event loops for the same session-store key.
- Adding cross-process locking, fencing, encryption, or tamper resistance to
  the file adapter.
- Maintaining a permanent fork of `mqtt-typed-client` solely for this recipe.
- Claiming compatibility with the default upstream-rumqttc backend.
