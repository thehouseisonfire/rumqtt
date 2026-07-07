use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, Broker, MqttOptions, PublishOptions};
use std::{error::Error, time::Duration};
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new(
        "rumqtt-websocket-headers-v5",
        Broker::websocket("ws://broker.example.com:8083/mqtt").expect("valid websocket URL"),
    );
    mqttoptions.set_keep_alive(60);
    mqttoptions.set_request_modifier(|mut request| async move {
        request
            .headers_mut()
            .insert("x-api-key", "replace-with-token".parse().unwrap());
        request.headers_mut().insert(
            "x-client-version",
            env!("CARGO_PKG_VERSION").parse().unwrap(),
        );
        request
    });

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    task::spawn(async move {
        client
            .subscribe("hello/websocket-headers", QoS::AtMostOnce)
            .await
            .unwrap();
        client
            .publish(
                "hello/websocket-headers",
                "hello through websocket headers",
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
