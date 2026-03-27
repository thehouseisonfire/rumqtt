use rumqttc::{Client, Event, Incoming, MqttOptions, QoS};
use std::thread;
use std::time::{Duration, SystemTime};
use wincode::{SchemaRead, SchemaWrite};

#[derive(SchemaWrite, SchemaRead, Debug)]
struct Message {
    i: usize,
    time: SystemTime,
}

fn encode_message(value: &Message) -> Result<Vec<u8>, wincode::WriteError> {
    wincode::serialize(value)
}

impl TryFrom<&[u8]> for Message {
    type Error = wincode::ReadError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        wincode::deserialize(value)
    }
}

fn main() {
    let mqtt_opts = MqttOptions::new("test-1", "localhost");

    let (client, mut connection) = Client::new(mqtt_opts, 10);
    client.subscribe("hello/rumqtt", QoS::AtMostOnce).unwrap();
    thread::spawn(move || {
        for i in 0..10 {
            let message = Message {
                i,
                time: SystemTime::now(),
            };

            client
                .publish(
                    "hello/rumqtt",
                    QoS::AtLeastOnce,
                    false,
                    encode_message(&message).unwrap(),
                )
                .unwrap();
            thread::sleep(Duration::from_millis(100));
        }
    });

    // Iterate to poll the eventloop for connection progress
    for notification in connection.iter().flatten() {
        if let Event::Incoming(Incoming::Publish(packet)) = notification {
            match Message::try_from(packet.payload.as_ref()) {
                Ok(message) => println!("Payload = {message:?}"),
                Err(error) => println!("Error = {error}"),
            }
        }
    }
}
