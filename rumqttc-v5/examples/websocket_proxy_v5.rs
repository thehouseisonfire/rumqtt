use rumqttc::mqttbytes::QoS;
use rumqttc::{AsyncClient, Broker, MqttOptions, Proxy, ProxyAuth, ProxyType, PublishOptions};
use std::{error::Error, time::Duration};
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new(
        "rumqtt-websocket-proxy-v5",
        Broker::websocket("ws://broker.mqttdashboard.com:8000/mqtt").expect("valid websocket URL"),
    );
    mqttoptions.set_keep_alive(60);
    mqttoptions.set_proxy(Proxy {
        ty: ProxyType::Http,
        auth: ProxyAuth::None,
        addr: "127.0.0.1".into(),
        port: 8100,
    });

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    task::spawn(async move {
        requests(client).await;
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

async fn requests(client: AsyncClient) {
    client
        .subscribe("hello/websocket-proxy", QoS::AtMostOnce)
        .await
        .unwrap();

    for i in 1..=10 {
        client
            .publish(
                "hello/websocket-proxy",
                vec![1; i],
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .unwrap();

        time::sleep(Duration::from_secs(1)).await;
    }
}
