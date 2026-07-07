use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, MqttOptions, PublishOptions, TlsConfiguration};
use std::{error::Error, time::Duration};
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::websocket_with_tls_config(
        "rumqtt-wss-v5",
        "wss://broker.example.com/mqtt",
        TlsConfiguration::default_rustls(),
    )?;
    mqttoptions.set_keep_alive(60);

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    task::spawn(async move {
        client
            .subscribe("hello/wss", QoS::AtMostOnce)
            .await
            .unwrap();
        client
            .publish(
                "hello/wss",
                "hello over secure websockets",
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .unwrap();
        time::sleep(Duration::from_secs(3)).await;
    });

    loop {
        match eventloop.poll().await {
            Ok(event) => println!("Event = {event:?}"),
            Err(error) => {
                println!("Error = {error:?}");
                return Ok(());
            }
        }
    }
}
