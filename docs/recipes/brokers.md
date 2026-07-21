# Broker Notes

Broker-specific behavior changes with broker version, managed-service settings,
and account policy. Treat these notes as rumqttc configuration starting points
and confirm the exact requirements in your broker deployment.

## AWS IoT Core

- Use TLS with a custom CA and client certificate on port 8883 for X.509
  authentication.
- Use a stable client ID for persistent sessions.
- Keep certificate/key material outside source control and load it from the
  deployment environment.
- See [TLS](./tls.md) for the rustls client-auth pattern.

## EMQX

- TCP, TLS, WS, and WSS deployments are common; choose features to match the
  listener enabled in EMQX.
- Confirm whether the listener expects username/password, client certificates,
  MQTT 5 enhanced auth, or WebSocket headers.
- For shared subscriptions and MQTT 5 properties, prefer `rumqttc-v5-next`.

## HiveMQ

- Managed HiveMQ Cloud commonly uses TLS with username/password credentials.
- Enterprise deployments may use WebSockets behind load balancers; use request
  modifiers for required gateway headers.
- Verify session expiry and maximum in-flight limits in the broker configuration.

## Mosquitto

- Mosquitto is useful for local smoke tests and examples.
- Use plain TCP on `localhost:1883` for local development.
- Enable TLS, WebSockets, persistence, and authentication explicitly in the
  Mosquitto configuration before testing those recipes.

## Corporate Proxies

- Use `Proxy::http(...)` for ordinary HTTP CONNECT proxies and
  `Proxy::socks5(...)` for SOCKS5 proxies.
- Add `.with_credentials(...)` only when required; do not log proxy credentials.
- For TLS-intercepting proxies, install the corporate CA in the OS trust store or
  pass it explicitly in the TLS configuration selected by the recipe.
