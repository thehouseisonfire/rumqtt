//! Typed topic wrapper example for `rumqttc-v4-next`.
//!
//! A sibling fork at `~/MQTT/mqtt-typed-client-next` is patched for the current
//! `rumqttc-v4-next` API. In an application, use dependency overrides like this
//! while the fork points at the rumqtt checkout:
//!
//! ```toml
//! [dependencies]
//! mqtt-typed-client = { package = "mqtt-typed-client-next", path = "path/to/MQTT/mqtt-typed-client-next", default-features = false, features = ["rumqttc-v4", "macros", "json"] }
//! mqtt-typed-client-core = { package = "mqtt-typed-client-core-next", path = "path/to/MQTT/mqtt-typed-client-next/core", default-features = false, features = ["rumqttc-v4", "json"] }
//! mqtt-typed-client-macros = { package = "mqtt-typed-client-macros-next", path = "path/to/MQTT/mqtt-typed-client-next/macros", features = ["rumqttc-v4"] }
//! rumqttc = { package = "rumqttc-v4-next", path = "path/to/rumqtt/rumqttc-v4" }
//! ```
//!
//! When the published crate catches up with the current API, replace the
//! `rumqttc` path override with:
//!
//! ```toml
//! rumqttc = { package = "rumqttc-v4-next", version = "0.33.2" }
//! ```
//!
//! Run with a local MQTT broker on port 1883:
//!
//! ```sh
//! cargo run -p rumqttc-v4-next --example typed_client_v4
//! ```

use mqtt_typed_client::{JsonSerializer, MqttClient};
use mqtt_typed_client_macros::mqtt_topic;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Reading {
    temperature_c: f32,
    battery_percent: u8,
}

#[mqtt_topic("rumqtt/v4/{room}/{sensor_id}/reading")]
struct SensorReading {
    room: String,
    sensor_id: u32,
    payload: Reading,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let broker_url = std::env::var("MQTT_BROKER_URL")
        .unwrap_or_else(|_| "mqtt://localhost:1883?client_id=rumqtt-v4-typed-example".to_owned());

    println!("connecting to {broker_url}");
    let (client, connection) = MqttClient::<JsonSerializer>::connect(&broker_url).await?;

    let topic_client = client.sensor_reading();
    let mut subscriber = topic_client.subscribe().await?;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let reading = Reading {
        temperature_c: 22.5,
        battery_percent: 91,
    };

    topic_client.publish("lab", 7, &reading).await?;

    let received = tokio::time::timeout(Duration::from_secs(3), subscriber.receive()).await?;
    match received {
        Some(Ok(message)) => {
            println!(
                "received room={} sensor_id={} reading={:?}",
                message.room, message.sensor_id, message.payload
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
