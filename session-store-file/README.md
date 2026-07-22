# rumqttc file-backed session stores

This independent workspace contains the optional file-backed implementations of
the `SessionStore` APIs owned by `rumqttc-v4-next` and `rumqttc-v5-next`.

- [`core`](core/README.md) provides protocol-neutral, crash-consistent local
  checkpoint storage.
- [`v4`](v4/README.md) adapts the core to MQTT 3.1.1 sessions.
- [`v5`](v5/README.md) adapts the core to MQTT 5 sessions.
- [`benchmarks`](benchmarks/README.md) contains persistence-specific workloads.

From the repository root, run:

```bash
cargo test --manifest-path session-store-file/Cargo.toml --workspace
```
