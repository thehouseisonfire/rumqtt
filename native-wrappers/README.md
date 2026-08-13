# rumqttc native wrappers

This independent workspace contains the host-neutral wrapper infrastructure and
the C API for the MQTT 3.1.1 and MQTT 5 clients:

- [`wrapper-core`](wrapper-core/README.md) owns the protocol-neutral native
  client driver and wrapper-facing Rust API.
- [`c`](c/README.md) exposes that API as a versioned C ABI and packages shared
  and static native libraries.

The two crates share one workspace because the C API directly adapts wrapper
core and changes to their command, event, completion, and lifecycle contracts
must remain coordinated. The workspace stays in this repository because
wrapper core tracks both client implementations closely.

From the repository root, run:

```bash
cargo test --manifest-path native-wrappers/Cargo.toml --workspace
```

The C ABI checks and native consumers are documented in the
[`c` crate README](c/README.md).
