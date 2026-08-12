# Run Clients Using Docker

The checked-in Docker fixture builds the MQTT 3.1.1 and MQTT 5 finite
publish/subscribe examples from this workspace and runs them against a local
Mosquitto broker. From the repository root:

```bash
docker compose -f docs/recipes/fixtures/docker-clients/compose.yaml up -d --wait broker
docker compose -f docs/recipes/fixtures/docker-clients/compose.yaml run --rm client-v4
docker compose -f docs/recipes/fixtures/docker-clients/compose.yaml run --rm client-v5
docker compose -f docs/recipes/fixtures/docker-clients/compose.yaml down -v
```

The client images use separate multi-stage Dockerfile targets and contain only
the compiled example binary and its runtime dependencies. A successful run
prints `MQTT 3.1.1 broker recipe smoke test passed` for v4 and
`MQTT 5 broker recipe smoke test passed` for v5, then exits with status zero.

Inside the Compose network, `localhost` means the current container rather than
the broker. The fixture therefore sets `RUMQTTC_RECIPE_HOST=broker`, using the
broker service name for container DNS, and `RUMQTTC_RECIPE_PORT=1883`. The
clients wait for the broker health check before connecting.

The Mosquitto configuration deliberately permits anonymous connections and is
for local development and CI only. For a deployed client image, configure the
broker endpoint and credentials for the target environment, keep secrets out of
the image, and follow the [TLS](./tls.md), [WebSocket](./websockets.md), and
[broker-specific](./brokers.md) recipes as applicable.

The containerized programs are the existing
[`broker_recipe_smoke.rs`](../../rumqttc-v4/examples/broker_recipe_smoke.rs) and
[`broker_recipe_smoke_v5.rs`](../../rumqttc-v5/examples/broker_recipe_smoke_v5.rs)
examples. They accept the same host and port environment variables when used in
another container runtime.
