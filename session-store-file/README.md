# rumqttc file-backed session stores

This independent workspace contains the optional file-backed implementations of
the `SessionStore` APIs owned by `rumqttc-v4-next` and `rumqttc-v5-next`.

- [`core`](core/README.md) provides protocol-neutral, crash-consistent local
  checkpoint storage.
- [`adapter`](adapter/README.md) provides independently selectable `v4` and
  `v5` modules in one package.
- [`consumer-tests`](consumer-tests/) verifies the dual-protocol public API as
  a downstream crate.
- [`benchmarks`](benchmarks/README.md) contains persistence-specific workloads.

From the repository root, run:

```bash
cargo test --manifest-path session-store-file/Cargo.toml --workspace
```
