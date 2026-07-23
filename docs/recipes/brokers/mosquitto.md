# Mosquitto Deployment Recipe

The checked-in fixture enables MQTT TCP on port 1883, MQTT over WebSockets on
port 8083, persistence, and bounded broker queues. It deliberately allows
anonymous access and is suitable only for local development and CI.

From the repository root:

```bash
docker compose -f docs/recipes/fixtures/mosquitto/compose.yaml up -d --wait
cargo run -p rumqttc-v4-next --example broker_recipe_smoke
cargo run -p rumqttc-v5-next --example broker_recipe_smoke_v5
docker compose -f docs/recipes/fixtures/mosquitto/compose.yaml down -v
```

The finite smoke examples accept `RUMQTTC_RECIPE_HOST` and
`RUMQTTC_RECIPE_PORT` when the published listener is not `127.0.0.1:1883`.
Their source is
[`broker_recipe_smoke.rs`](../../../rumqttc-v4/examples/broker_recipe_smoke.rs)
and
[`broker_recipe_smoke_v5.rs`](../../../rumqttc-v5/examples/broker_recipe_smoke_v5.rs).

For production, replace anonymous access with a password file, plugin, or
client-certificate listener; mount credentials read-only; protect the
WebSocket listener with TLS; and use a durable volume with an explicit backup
policy. Keep `max_inflight_messages`, `max_queued_messages`, and the rumqttc
request-channel capacity bounded according to the deployment's loss and latency
budget.

See the official
[Mosquitto configuration reference](https://mosquitto.org/man/mosquitto-conf-5.html)
for listener, persistence, authentication, TLS, and queue settings.
