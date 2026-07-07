//! TLS with a custom CA and PEM client certificate/key.
//!
//! Replace the placeholder bytes with deployment certificates, for example:
//! `include_bytes!("AmazonRootCA1.pem")`,
//! `include_bytes!("device-certificate.pem.crt")`, and
//! `include_bytes!("private.pem.key")`.

use rumqttc::{AsyncClient, Event, MqttOptions, TlsConfiguration, Transport};
use std::error::Error;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let ca = vec![1, 2, 3];
    let client_cert = vec![1, 2, 3];
    let client_key = vec![1, 2, 3];

    let mut mqttoptions = MqttOptions::new("rumqtt-client-auth-v5", ("mqtt.example.com", 8883));
    mqttoptions.set_keep_alive(30);
    mqttoptions.set_transport(Transport::tls_with_config(TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth: Some((client_cert, client_key)),
    }));

    let (_client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(packet)) => println!("Incoming = {packet:?}"),
            Ok(Event::Outgoing(packet)) => println!("Outgoing = {packet:?}"),
            Ok(Event::Auth(event)) => println!("Auth = {event:?}"),
            Err(error) => {
                println!("Error = {error:?}");
                return Ok(());
            }
        }
    }
}
