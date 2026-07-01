# TODO: switch typed-client examples to crates.io dependencies

The `rumqttc-v4` and `rumqttc-v5` example manifests currently depend on the
local sibling checkout:

```toml
mqtt-typed-client = { package = "mqtt-typed-client-next", path = "../../mqtt-typed-client-next", ... }
mqtt-typed-client-core = { package = "mqtt-typed-client-core-next", path = "../../mqtt-typed-client-next/core", ... }
mqtt-typed-client-macros = { package = "mqtt-typed-client-macros-next", path = "../../mqtt-typed-client-next/macros", ... }
```

Once `mqtt-topic-engine-next`, `mqtt-typed-client-next`,
`mqtt-typed-client-core-next`, and `mqtt-typed-client-macros-next` are published
to crates.io, replace those path dependencies in `rumqttc-v4/Cargo.toml` and
`rumqttc-v5/Cargo.toml` with their published package IDs and versions, while
preserving the selected backend feature (`rumqttc-v4` or `rumqttc-v5`).
