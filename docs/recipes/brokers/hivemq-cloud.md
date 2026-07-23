# HiveMQ Cloud Deployment Recipe

HiveMQ Cloud deployments normally use a public-CA TLS listener with
username/password authentication. Supply all values from deployment secrets;
do not embed them in the binary or log the resulting options.

```rust,no_run
use rumqttc::{AsyncClient, MqttOptions, TlsConfiguration, Transport};
use std::env;

# async fn connect() {
let host = env::var("HIVEMQ_HOST").expect("HIVEMQ_HOST is required");
let username = env::var("HIVEMQ_USERNAME").expect("HIVEMQ_USERNAME is required");
let password = env::var("HIVEMQ_PASSWORD").expect("HIVEMQ_PASSWORD is required");

let mut options = MqttOptions::new("stable-production-client-id", (host, 8883));
options.set_credentials(username, password);
options.set_transport(Transport::tls_with_config(TlsConfiguration::default_rustls()));
options.set_keep_alive(30);

let (_client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();
while let Ok(event) = eventloop.poll().await {
    println!("{event:?}");
}
# }
```

Use a unique, stable client ID when broker-side session continuity is required.
Confirm the cluster's MQTT version, session-expiry policy, maximum packet size,
inflight quota, and WebSocket path in its portal; service-plan limits can differ
from a self-hosted HiveMQ deployment.

See the official
[HiveMQ Cloud authentication and authorization guide](https://docs.hivemq.com/hivemq-cloud/authn-authz.html)
when provisioning credentials and permissions.
