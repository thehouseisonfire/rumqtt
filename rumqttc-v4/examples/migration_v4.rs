use bytes::Bytes;
use rumqttc::{
    AckMode, AsyncClient, Client, ConnectAuth, MqttOptions, PublishOptions, QoS, SessionMode,
};

fn configured_options() -> MqttOptions {
    let mut options = MqttOptions::new("migration-v4", "localhost");
    options
        .set_keep_alive(30)
        .set_credentials("user", Bytes::from_static(b"password"))
        .set_ack_mode(AckMode::Manual)
        .set_session_mode(SessionMode::Persistent);

    options.set_socket_connector(|host, network_options| async move {
        rumqttc::default_socket_connect(host, network_options).await
    });

    assert_eq!(options.ack_mode(), AckMode::Manual);
    assert!(matches!(
        options.auth(),
        ConnectAuth::UsernamePassword { username, .. } if username == "user"
    ));
    assert!(!options.clean_session());
    assert!(options.has_socket_connector());

    options
}

fn sync_client_uses_builder_and_publish_options() -> Result<(), rumqttc::ClientError> {
    let options = configured_options();
    let (client, _connection) = Client::builder(options).capacity(10).build();

    client.publish(
        "migration/v4/status",
        "online",
        PublishOptions::at_least_once(),
    )?;
    client.publish(
        "migration/v4/bytes",
        [1, 2, 3, 4],
        PublishOptions::new(QoS::ExactlyOnce).retained(),
    )?;

    Ok(())
}

#[allow(dead_code)]
async fn async_client_uses_builder_and_publish_options() -> Result<(), rumqttc::ClientError> {
    let options = configured_options();
    let (client, _eventloop) = AsyncClient::builder(options).capacity(10).build();

    client
        .publish(
            "migration/v4/async",
            Bytes::from_static(b"payload"),
            PublishOptions::new(QoS::AtMostOnce),
        )
        .await?;

    Ok(())
}

fn main() -> Result<(), rumqttc::ClientError> {
    sync_client_uses_builder_and_publish_options()
}
