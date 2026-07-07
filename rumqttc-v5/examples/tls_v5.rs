//! TLS with rustls and platform root certificates.

use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, PublishOptions, Transport};
use std::error::Error;
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new("rumqtt-tls-v5", ("mqtt.example.com", 8883));
    mqttoptions.set_keep_alive(30);
    mqttoptions.set_credentials("username", "password");
    mqttoptions.set_transport(Transport::tls_with_default_config());

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    task::spawn(async move {
        client
            .subscribe("hello/tls", QoS::AtLeastOnce)
            .await
            .unwrap();
        client
            .publish(
                "hello/tls",
                "hello over tls",
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .unwrap();
    });

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Incoming::Publish(publish))) => {
                println!(
                    "Topic: {}, Payload: {:?}",
                    String::from_utf8_lossy(&publish.topic),
                    publish.payload
                );
            }
            Ok(event) => println!("Event = {event:?}"),
            Err(error) => {
                println!("Error = {error:?}");
                return Ok(());
            }
        }

        time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
