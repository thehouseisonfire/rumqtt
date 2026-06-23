use rumqttc::PublishOptions;
use rumqttc::mqttbytes::{QoS, v5::LastWill};
use rumqttc::{Client, ConnectionError, MqttOptions};
use std::thread;
use std::time::Duration;

fn main() {
    pretty_env_logger::init();

    let mut mqttoptions = MqttOptions::new("test-1", ("localhost", 1884));
    let will = LastWill::new("hello/world", "good bye", QoS::AtMostOnce, false, None);
    mqttoptions.set_keep_alive(5).set_last_will(will);

    let (client, mut connection) = Client::builder(mqttoptions).capacity(10).build();
    thread::spawn(move || publish(&client));

    for (i, notification) in connection.iter().enumerate() {
        match notification {
            Err(ConnectionError::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                println!(
                    "Failed to connect to the server. Make sure correct client is configured properly!\nError: {error:?}"
                );
                return;
            }
            _ => {}
        }
        println!("{i}. Notification = {notification:?}");
    }

    println!("Done with the stream!!");
}

fn publish(client: &Client) {
    client.subscribe("hello/+/world", QoS::AtMostOnce).unwrap();
    for i in 0..10_usize {
        let payload = vec![1; i];
        let topic = format!("hello/{i}/world");
        let qos = QoS::AtLeastOnce;

        client
            .publish(topic, payload, PublishOptions::new(qos).retained())
            .expect("example publish should succeed");
    }

    thread::sleep(Duration::from_secs(1));
}
