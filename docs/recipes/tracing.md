# Structured Tracing Recipes

The opt-in `tracing` feature emits low-frequency lifecycle events with the
`rumqttc::lifecycle` target. It covers connection attempts and failures,
established and lost connections, session restoration and replay, and protocol
violations. Payloads, topics, credentials, broker URLs, client IDs, and MQTT user
properties are not recorded.

## Install a Subscriber

rumqttc emits events but deliberately does not install a subscriber. Enable the
crate feature and configure a subscriber in the application before polling an
event loop:

```toml
[dependencies]
rumqttc-v5-next = { version = "0.34.0-alpha", features = ["tracing"] }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

```rust
use tracing_subscriber::EnvFilter;

let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info,rumqttc::lifecycle=debug"));
tracing_subscriber::fmt().with_env_filter(filter).init();
```

The same setup works with `rumqttc-v4-next` and the MQTT 5 `rumqttc-next`
facade. Keep the `rumqttc::lifecycle=debug` directive if connection-attempt
events are needed; those events use `DEBUG`, while established connections and
session/replay events use `INFO`, failures use `WARN`, and protocol violations
use `ERROR`.

Run the complete examples with:

```bash
cargo run -p rumqttc-v4-next --features tracing --example lifecycle_tracing
cargo run -p rumqttc-v5-next --features tracing --example lifecycle_tracing_v5
```

## Consume Structured Fields

Prefer event names and typed primitive fields over parsing the rendered error
message. Useful correlation fields include `protocol`, `attempt_id`,
`connection_generation`, `attempt_in_generation`, and `reconnect`. Failure
events also provide stable `error_kind` or `violation_kind` classifications.

A cancelled `EventLoop::poll()` can leave an `mqtt.connection_attempt` without
a matching terminal event. Correlate attempts by `attempt_id` and do not treat
an unmatched attempt alone as proof that a connection is still in progress.

## Compatibility with `log`

Use `tracing-log-compat` instead of `tracing` only when events must fall back to
`log::Record` values in applications that have no tracing subscriber. Cargo
feature unification enables `tracing/log`, so this compatibility mode is best
chosen by the final application rather than a library. Its fallback fields are
textual; use a tracing subscriber when structured fields matter.
