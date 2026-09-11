use rumqttc::{AsyncClient, ConnectionError, MqttOptions, PublishOptions, QoS};
use std::{error::Error, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let (client, mut eventloop) =
        AsyncClient::builder(MqttOptions::new("ordered-shutdown", "localhost"))
            .capacity(16)
            .build();
    let driver = tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => break,
                // Continue polling for reconnect or terminal persistence cleanup.
                Err(error) => eprintln!("MQTT: {error}"),
            }
        }
    });

    for index in 0..32u8 {
        client
            .publish(
                "shutdown/burst",
                vec![index],
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await?;
    }
    let completion = client
        .disconnect_after_queued_with_timeout(Duration::from_secs(5))
        .await?;
    let result = completion.wait_async().await;
    driver.await?;
    result?;
    Ok(())
}
