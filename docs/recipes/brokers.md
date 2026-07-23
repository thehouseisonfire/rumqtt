# Broker Deployment Recipes

Broker-specific behavior changes with broker version, managed-service settings,
and account policy. Treat these notes as rumqttc configuration starting points
and confirm the exact requirements in your broker deployment.

Use the broker-specific guides for deployable configuration, exact client
settings, verification commands, and operational caveats:

- [Mosquitto](./brokers/mosquitto.md): Docker Compose TCP/WebSocket fixture,
  persistence, bounded broker queues, and v4/v5 smoke tests.
- [EMQX](./brokers/emqx.md): Docker Compose TCP/WebSocket fixture and client
  verification.
- [HiveMQ Cloud](./brokers/hivemq-cloud.md): public-root TLS with
  username/password authentication.
- [AWS IoT Core](./brokers/aws-iot-core.md): private key and client-certificate
  authentication with a custom CA.

## Corporate Proxies

- Use `Proxy::http(...)` for ordinary HTTP CONNECT proxies and
  `Proxy::socks5(...)` for SOCKS5 proxies.
- Add `.with_credentials(...)` only when required; do not log proxy credentials.
- For TLS-intercepting proxies, install the corporate CA in the OS trust store or
  pass it explicitly in the TLS configuration selected by the recipe.
