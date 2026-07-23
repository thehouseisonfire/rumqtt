//! Finite MQTT 5 publish/subscribe smoke test for deployment recipes.

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, PublishOptions, QoS};
use std::{env, error::Error, io, process, time::Duration};
use tokio::time::timeout;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let host = env::var("RUMQTTC_RECIPE_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = env::var("RUMQTTC_RECIPE_PORT")
        .map(|value| value.parse())
        .unwrap_or(Ok(1883))?;
    let client_id = format!("rumqttc-v5-recipe-smoke-{}", process::id());
    let topic = format!("rumqttc/recipes/smoke/{client_id}");
    let payload = b"mqtt-v5-recipe-ok";

    let mut options = MqttOptions::new(client_id, (host, port));
    options.set_keep_alive(5);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();

    client.subscribe(topic.clone(), QoS::AtLeastOnce).await?;
    timeout(Duration::from_secs(15), async {
        loop {
            let event = eventloop.poll().await?;
            println!("Event = {event:?}");
            if matches!(event, Event::Incoming(Packet::SubAck(_))) {
                return Ok::<_, rumqttc::ConnectionError>(());
            }
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for MQTT 5 SUBACK",
        )
    })??;

    client
        .publish(
            topic.clone(),
            payload.as_slice(),
            PublishOptions::new(QoS::AtLeastOnce),
        )
        .await?;

    timeout(Duration::from_secs(15), async {
        loop {
            let event = eventloop.poll().await?;
            println!("Event = {event:?}");
            if let Event::Incoming(Packet::Publish(publish)) = event
                && publish.topic == topic
                && publish.payload.as_ref() == payload
            {
                return Ok::<_, rumqttc::ConnectionError>(());
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for MQTT 5 echo"))??;

    println!("MQTT 5 broker recipe smoke test passed");
    Ok(())
}
