//! Typed client example using MQTT 5 CONNECT properties.
//!
//! This uses the sibling `~/MQTT/mqtt-typed-client-next` fork with its
//! `rumqttc-v5` feature enabled. In an application, use dependencies like:
//!
//! ```toml
//! mqtt-typed-client = { package = "mqtt-typed-client-next", path = "path/to/MQTT/mqtt-typed-client-next", default-features = false, features = ["rumqttc-v5", "macros", "json"] }
//! mqtt-typed-client-core = { package = "mqtt-typed-client-core-next", path = "path/to/MQTT/mqtt-typed-client-next/core", default-features = false, features = ["rumqttc-v5", "json"] }
//! mqtt-typed-client-macros = { package = "mqtt-typed-client-macros-next", path = "path/to/MQTT/mqtt-typed-client-next/macros", features = ["rumqttc-v5"] }
//! rumqttc = { package = "rumqttc-v5-next", path = "path/to/rumqtt/rumqttc-v5" }
//! ```

use mqtt_typed_client::{JsonSerializer, MqttClient, MqttClientConfig};
use mqtt_typed_client_macros::mqtt_topic;
use rumqttc::mqttbytes::v5::ConnectProperties;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Reading {
    humidity_percent: u8,
}

#[mqtt_topic("rumqtt/v5/connect/{site}/{sensor_id}/reading")]
struct ConnectPropertyReading {
    site: String,
    sensor_id: u32,
    payload: Reading,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut connect_properties = ConnectProperties::new();
    connect_properties.session_expiry_interval = Some(60);
    connect_properties.receive_maximum = Some(16);
    connect_properties.topic_alias_max = Some(8);
    connect_properties.user_properties = vec![("client-kind".into(), "typed-v5-example".into())];

    let mut config = MqttClientConfig::<JsonSerializer>::new(
        "rumqtt-v5-typed-connect-properties",
        "localhost",
        1883,
    );
    config.connection.set_keep_alive(5);
    config.connection.set_connect_properties(connect_properties);

    let (client, connection) = MqttClient::<JsonSerializer>::connect_with_config(config).await?;

    let topic_client = client.connect_property_reading();
    let mut subscriber = topic_client.subscribe().await?;

    tokio::time::sleep(Duration::from_millis(300)).await;

    topic_client
        .publish(
            "lab",
            42,
            &Reading {
                humidity_percent: 64,
            },
        )
        .await?;

    let received = tokio::time::timeout(Duration::from_secs(3), subscriber.receive()).await?;
    match received {
        Some(Ok(message)) => {
            println!(
                "received site={} sensor_id={} reading={:?}",
                message.site, message.sensor_id, message.payload
            );
        }
        Some(Err(error)) => return Err(Box::<dyn Error>::from(error)),
        None => {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "subscriber closed before receiving message",
            )));
        }
    }

    connection.shutdown().await?;
    Ok(())
}
