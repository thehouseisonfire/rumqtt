# AWS IoT Core Deployment Recipe

The standard X.509 listener uses mutual TLS on port 8883. Download the endpoint,
Amazon root CA, device certificate, and private key during provisioning and
make the key readable only by the service account.

```rust,no_run
use rumqttc::{AsyncClient, MqttOptions, TlsConfiguration, Transport};
use std::{env, fs};

# async fn connect() {
let endpoint = env::var("AWS_IOT_ENDPOINT").expect("AWS_IOT_ENDPOINT is required");
let ca = fs::read(env::var("AWS_IOT_CA").expect("AWS_IOT_CA is required"))
    .expect("read AWS IoT root CA");
let certificate = fs::read(env::var("AWS_IOT_CERT").expect("AWS_IOT_CERT is required"))
    .expect("read AWS IoT device certificate");
let private_key = fs::read(env::var("AWS_IOT_KEY").expect("AWS_IOT_KEY is required"))
    .expect("read AWS IoT private key");

let mut options = MqttOptions::new("provisioned-thing-client-id", (endpoint, 8883));
options.set_transport(Transport::tls_with_config(TlsConfiguration::Simple {
    ca,
    alpn: None,
    client_auth: Some((certificate, private_key)),
}));
options.set_keep_alive(30);

let (_client, mut eventloop) = AsyncClient::builder(options).capacity(100).build();
while let Ok(event) = eventloop.poll().await {
    println!("{event:?}");
}
# }
```

Attach an IoT policy that permits only the required connect client ID, topic
publishes, and topic-filter subscriptions. AWS IoT policy resources are
distinct for topics and topic filters. Use a stable client ID only when the
associated policy and session behavior intentionally permit it, and rotate the
certificate without copying private keys into source control or container
images.

See AWS's official
[device communication protocol matrix](https://docs.aws.amazon.com/iot/latest/developerguide/protocols.html)
for supported ports, authentication modes, and ALPN requirements.
