use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, Broker, MqttOptions, PublishOptions, TlsConfiguration, Transport};
use std::{error::Error, time::Duration};
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new(
        "rumqtt-wss-v5",
        Broker::websocket("ws://broker.example.com:443/mqtt").expect("valid websocket URL"),
    );
    mqttoptions.set_keep_alive(60);
    mqttoptions.set_transport(Transport::wss_with_config(
        TlsConfiguration::default_rustls(),
    ));

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
