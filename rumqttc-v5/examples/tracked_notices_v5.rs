use std::error::Error;
use std::time::Duration;

use rumqttc::{AsyncClient, MqttOptions, PublishResult, QoS};
use tokio::{task, time};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut mqttoptions = MqttOptions::new("tracked-notices-v5", ("localhost", 1884));
    mqttoptions.set_keep_alive(5);

    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    task::spawn(async move {
        while let Ok(event) = eventloop.poll().await {
            println!("Event = {event:?}");
        }
    });

    let subscribe_notice = client
        .subscribe_tracked("hello/tracked", QoS::AtLeastOnce)
        .await?;
    let suback = subscribe_notice.wait_async().await?;
    println!(
        "SUBACK return codes = {:?}, properties = {:?}",
        suback.return_codes, suback.properties
    );

    let publish_notice = client
        .publish_tracked("hello/tracked", QoS::AtLeastOnce, false, "hello")
        .await?;
    match publish_notice.wait_async().await? {
        PublishResult::Qos0Flushed => println!("QoS0 publish flushed"),
        PublishResult::Qos1(puback) => {
            println!(
                "PUBACK pkid = {}, reason = {:?}",
                puback.pkid, puback.reason
            );
        }
        PublishResult::Qos2Completed(pubcomp) => {
            println!(
                "PUBCOMP pkid = {}, reason = {:?}",
                pubcomp.pkid, pubcomp.reason
            );
        }
        PublishResult::Qos2PubRecRejected(pubrec) => {
            println!(
                "PUBREC rejected pkid = {}, reason = {:?}",
                pubrec.pkid, pubrec.reason
            );
        }
    }

    client
        .unsubscribe_tracked("hello/tracked")
        .await?
        .wait_completion_async()
        .await?;

    time::sleep(Duration::from_millis(100)).await;
    Ok(())
}
