use rumqttc::PublishOptions;
use rumqttc::{Client, MqttOptions, QoS};
use std::thread;
use std::time::Duration;

fn main() {
    let mqttoptions = MqttOptions::new("test-1", "localhost");

    let (client, mut connection) = Client::builder(mqttoptions).unbounded().build();

    client.subscribe("hello/rumqtt", QoS::AtMostOnce).unwrap();
    thread::spawn(move || {
        for i in 0..10 {
            client
                .publish(
                    "hello/rumqtt",
                    vec![1; i],
                    PublishOptions::new(QoS::AtLeastOnce),
                )
                .unwrap();
            thread::sleep(Duration::from_millis(100));
        }
    });

    for notification in connection.iter() {
        println!("Notification = {notification:?}");
    }
}
