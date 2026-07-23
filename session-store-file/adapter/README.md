# MQTT session file store

`rumqttc-session-store-file-next` provides file-backed session stores for the
explicit MQTT 3.1.1 and MQTT 5 rumqttc clients. Protocol support is opt-in and
additive:

| Cargo feature | Client package | Rust module |
| --- | --- | --- |
| `v4` | `rumqttc-v4-next` | `rumqttc_session_store_file::v4` |
| `v5` | `rumqttc-v5-next` | `rumqttc_session_store_file::v5` |

There is no default protocol feature. Enable both features to use both stores
in one process:

```toml
[dependencies]
rumqttc-session-store-file-next = { version = "0.34.0-alpha", features = ["v4", "v5"] }
rumqttc-v4-next = "0.34.0-alpha"
rumqttc-v5-next = "0.34.0-alpha"
```

```rust
use rumqttc_session_store_file::v4::SessionFileStore as V4SessionFileStore;
use rumqttc_session_store_file::v5::SessionFileStore as V5SessionFileStore;
```

The stores preserve separate `v4` and `v5` namespaces and protocol-tagged key
formats. Enabling both does not make their session keys or persisted session
values interchangeable.

The adapter requires an existing, trusted local-filesystem root. It provides
process-local same-key coordination but no cross-process locking, encryption,
authentication, tamper resistance, or universal power-loss guarantee. Exact
filenames from the repository's former examples are detected but never scanned
for, decoded, or migrated.
