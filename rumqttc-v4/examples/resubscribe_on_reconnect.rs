use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::error::Error;
use std::time::Duration;
use tokio::time;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new("rumqtt-resubscribe-v4", "localhost");
    mqttoptions.set_keep_alive(5);

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();
    let subscriptions = [("hello/rumqtt", QoS::AtMostOnce)];
    let mut connected_once = false;

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(connack))) => {
                println!("Connected. session_present = {}", connack.session_present);

                // Raw Session Present is broker evidence. Effective session semantics
                // also account for an opted-in AcceptAsClean compatibility recovery.
                let fresh_session = !eventloop
                    .diagnostics()
                    .session
                    .connack
                    .expect("accepted CONNACK has committed session diagnostics")
                    .session_resumed;
                if fresh_session {
                    for (topic, qos) in subscriptions {
                        client.try_subscribe(topic, qos)?;
                    }
                }

                if connected_once && fresh_session {
                    println!(
                        "Reconnected with a fresh broker session; subscriptions were reissued"
                    );
                }
                connected_once = true;
            }
            Ok(event) => {
                println!("Event = {event:?}");
            }
            Err(error) => {
                println!("Connection error = {error:?}. Polling again will reconnect.");
                time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
