# EMQX Deployment Recipe

The local fixture exposes MQTT TCP on port 1883 and MQTT over WebSockets on
port 8083. EMQX's dashboard remains bound to localhost on port 18083. The
fixture has no authentication and is intended only for development.

```bash
docker compose -f docs/recipes/fixtures/emqx/compose.yaml up -d --wait
cargo run -p rumqttc-v4-next --example broker_recipe_smoke
cargo run -p rumqttc-v5-next --example broker_recipe_smoke_v5
docker compose -f docs/recipes/fixtures/emqx/compose.yaml down -v
```

For a WebSocket client, use `ws://127.0.0.1:8083/mqtt` and enable the
`websocket` feature. Before production, configure an authenticator and
authorization rules, terminate TLS on the broker or a trusted gateway, disable
unused listeners, persist `/opt/emqx/data`, and set broker inflight and queued
message limits together with rumqttc's bounded channel capacity.

Use `rumqttc-v5-next` when the deployment depends on MQTT 5 properties,
enhanced authentication, session expiry, or shared-subscription behavior that
must be tested specifically under MQTT 5.

See EMQX's official
[listener configuration](https://docs.emqx.com/en/emqx/latest/configuration/listener.html)
for TCP, TLS, WebSocket, and secure WebSocket settings.
