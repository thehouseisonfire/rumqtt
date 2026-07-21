# Multipath TCP

On Linux, opt an MQTT connection into Multipath TCP (MPTCP) through
`NetworkOptions`. The same configuration works with the MQTT 3.1.1 and MQTT 5
packages and requires no Cargo feature.

```rust,no_run
use rumqttc::{MqttOptions, NetworkOptions};

let mut options = MqttOptions::new("mobile-client", ("broker.example.com", 1883));

#[cfg(target_os = "linux")]
{
    let mut network_options = NetworkOptions::new();
    network_options.set_mptcp(true);
    options.set_network_options(network_options);
}
```

MPTCP can preserve a connection while paths change, such as during a Wi-Fi to
cellular handover. Actual multipath use requires an MPTCP-capable peer and Linux
path-manager configuration on the endpoints. A peer that does not negotiate
MPTCP uses the connection as ordinary TCP. If the local kernel reports MPTCP as
unsupported or disabled, rumqttc creates an ordinary TCP socket instead.

When an HTTP proxy is configured, MPTCP applies to the client-to-proxy
connection; the proxy owns its connection to the broker.
