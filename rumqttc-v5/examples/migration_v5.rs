use bytes::Bytes;
use rumqttc::mqttbytes::v5::{ConnectProperties, PublishProperties};
use rumqttc::{
    AckMode, AsyncClient, BrokerSessionResumePolicy, Client, ConnectAuth, MqttOptions,
    PublishOptions, QoS, SessionMode, TopicAliasPolicy,
};

fn configured_options() -> MqttOptions {
    let mut connect_properties = ConnectProperties::new();
    connect_properties.session_expiry_interval = Some(3600);
    connect_properties.receive_maximum = Some(32);
    connect_properties.topic_alias_max = Some(16);
    connect_properties.user_properties = vec![("app".into(), "migration-guide".into())];

    let mut options = MqttOptions::new("migration-v5", "localhost");
    options
        .set_keep_alive(30)
        .set_connect_properties(connect_properties)
        .set_clean_start(false)
        .set_password(Bytes::from_static(b"token"))
        .set_ack_mode(AckMode::Manual)
        .set_topic_alias_policy(TopicAliasPolicy::Lru)
        .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly)
        .set_session_mode(SessionMode::Persistent);

    assert_eq!(options.ack_mode(), AckMode::Manual);
    assert!(matches!(options.auth(), ConnectAuth::Password { .. }));
    assert_eq!(options.topic_alias_policy(), TopicAliasPolicy::Lru);
    assert_eq!(
        options.broker_session_resume_policy(),
        BrokerSessionResumePolicy::AllowBrokerOnly
    );
    assert!(!options.clean_start());
    assert_eq!(options.topic_alias_max(), Some(16));

    options
}

fn sync_client_uses_builder_and_publish_options() -> Result<(), rumqttc::ClientError> {
    let options = configured_options();
    let (client, _connection) = Client::builder(options).capacity(10).build();

    let properties = PublishProperties {
        payload_format_indicator: Some(1),
        content_type: Some("application/json".into()),
        correlation_data: Some(Bytes::from_static(b"request-1")),
        user_properties: vec![("source".into(), "migration-guide".into())],
        ..Default::default()
    };

    client.publish(
        "migration/v5/status",
        br#"{"online":true}"#,
        PublishOptions::at_least_once().properties(properties),
    )?;

    Ok(())
}

#[expect(dead_code)]
async fn async_client_uses_builder_and_publish_options() -> Result<(), rumqttc::ClientError> {
    let options = configured_options();
    let (client, _eventloop) = AsyncClient::builder(options).capacity(10).build();

    let properties = PublishProperties {
        topic_alias: Some(1),
        ..Default::default()
    };

    client
        .publish(
            "migration/v5/async",
            Bytes::from_static(b"payload"),
            PublishOptions::new(QoS::AtMostOnce).properties(properties),
        )
        .await?;

    Ok(())
}

fn main() -> Result<(), rumqttc::ClientError> {
    sync_client_uses_builder_and_publish_options()
}
