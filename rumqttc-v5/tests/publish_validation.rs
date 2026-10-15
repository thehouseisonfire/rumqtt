use bytes::Bytes;
use rumqttc::{
    AsyncClient, Client, ClientError, IntoPublishPayload, MqttOptions, PublishAdmissionPolicy,
    PublishOptions, PublishProperties, Request,
};

fn text_options(validate: bool) -> PublishOptions {
    let options = PublishOptions::at_most_once().properties(PublishProperties {
        payload_format_indicator: Some(1),
        ..Default::default()
    });
    if validate {
        options.validate_payload(true)
    } else {
        options
    }
}

const fn invalid_payload() -> Bytes {
    Bytes::from_static(&[0xff, 0x00])
}

fn assert_original_request(error: ClientError) {
    let ClientError::InvalidRequest(request) = error else {
        panic!("expected an invalid request, got {error:?}");
    };
    let Request::Publish(publish) = *request else {
        panic!("expected the rejected publish");
    };
    assert_eq!(publish.topic, "validation/topic");
    assert_eq!(publish.payload, invalid_payload());
    assert_eq!(
        publish.properties.unwrap().payload_format_indicator,
        Some(1)
    );
}

#[tokio::test]
async fn payload_validation_is_explicit_for_all_untracked_publish_apis() {
    let (tx, rx) = flume::bounded(3);
    let client = AsyncClient::from_senders(tx.clone());
    let blocking = Client::from_sender(tx);

    // PFI = 1 remains the caller's declaration when content validation is disabled.
    client
        .publish("validation/topic", invalid_payload(), text_options(false))
        .await
        .unwrap();
    client
        .try_publish("validation/topic", invalid_payload(), text_options(false))
        .unwrap();
    blocking
        .publish(
            "validation/topic",
            invalid_payload(),
            text_options(true).validate_payload(false),
        )
        .unwrap();
    for _ in 0..3 {
        let Request::Publish(publish) = rx.recv().unwrap() else {
            panic!("expected a publish");
        };
        assert_eq!(publish.payload, invalid_payload());
    }

    assert_original_request(
        client
            .publish("validation/topic", invalid_payload(), text_options(true))
            .await
            .unwrap_err(),
    );
    assert_original_request(
        client
            .try_publish("validation/topic", invalid_payload(), text_options(true))
            .unwrap_err(),
    );
    assert_original_request(
        blocking
            .publish("validation/topic", invalid_payload(), text_options(true))
            .unwrap_err(),
    );
    assert_original_request(
        blocking
            .try_publish("validation/topic", invalid_payload(), text_options(true))
            .unwrap_err(),
    );
    assert!(
        rx.is_empty(),
        "rejected content must not consume channel capacity"
    );
}

#[tokio::test]
async fn tracked_validation_is_independent_of_negotiated_capability_admission() {
    for policy in [
        PublishAdmissionPolicy::EventLoopValidated,
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
    ] {
        let (client, _eventloop) =
            AsyncClient::builder(MqttOptions::new("validation", "localhost"))
                .capacity(2)
                .publish_admission_policy(policy)
                .build();
        assert_original_request(
            client
                .publish_tracked("validation/topic", invalid_payload(), text_options(true))
                .await
                .unwrap_err(),
        );
        assert_original_request(
            client
                .try_publish_tracked("validation/topic", invalid_payload(), text_options(true))
                .unwrap_err(),
        );
        // Both rejected requests leave room for these two offline QoS 0 publishes.
        let _async_notice = client
            .publish_tracked("validation/topic", invalid_payload(), text_options(false))
            .await
            .unwrap();
        let _try_notice = client
            .try_publish_tracked("validation/topic", invalid_payload(), text_options(false))
            .unwrap();
    }
}

// An existing downstream implementation still needs only its original conversion method.
struct LegacyPayload(Vec<u8>);

impl IntoPublishPayload for LegacyPayload {
    fn into_publish_payload(self) -> Bytes {
        self.0.into()
    }
}

#[test]
fn checked_content_accepts_strings_and_checks_custom_byte_payloads() {
    let (tx, rx) = flume::bounded(3);
    let client = AsyncClient::from_senders(tx);
    // Payload UTF-8 permits NUL; MQTT property strings do not.
    let text = "héllo\0世界";
    client
        .try_publish("validation/topic", text, text_options(true))
        .unwrap();
    client
        .try_publish("validation/topic", text.to_owned(), text_options(true))
        .unwrap();
    client
        .try_publish(
            "validation/topic",
            Bytes::copy_from_slice(text.as_bytes()),
            text_options(true),
        )
        .unwrap();
    assert_original_request(
        client
            .try_publish(
                "validation/topic",
                LegacyPayload(invalid_payload().to_vec()),
                text_options(true),
            )
            .unwrap_err(),
    );
    for _ in 0..3 {
        let Request::Publish(publish) = rx.recv().unwrap() else {
            panic!("expected a publish");
        };
        assert_eq!(publish.payload, text);
    }
}

struct UninspectedPayload;

impl IntoPublishPayload for UninspectedPayload {
    fn into_publish_payload(self) -> Bytes {
        invalid_payload()
    }

    fn is_utf8_validated(&self) -> bool {
        panic!("payload validity is unnecessary for this publish");
    }
}

#[test]
fn unchecked_and_binary_publishes_do_not_inspect_content_validity() {
    let (tx, rx) = flume::bounded(3);
    let client = AsyncClient::from_senders(tx);
    client
        .try_publish("validation/topic", UninspectedPayload, text_options(false))
        .unwrap();
    for indicator in [None, Some(0)] {
        client
            .try_publish(
                "validation/topic",
                UninspectedPayload,
                PublishOptions::at_most_once()
                    .properties(PublishProperties {
                        payload_format_indicator: indicator,
                        ..Default::default()
                    })
                    .validate_payload(true),
            )
            .unwrap();
    }
    assert_eq!(rx.len(), 3);
}

#[test]
fn metadata_validation_remains_required_in_both_content_modes() {
    let oversized = "x".repeat(usize::from(u16::MAX) + 1);
    let invalid = [
        PublishProperties {
            payload_format_indicator: Some(2),
            ..Default::default()
        },
        PublishProperties {
            topic_alias: Some(0),
            ..Default::default()
        },
        PublishProperties {
            subscription_identifiers: vec![1],
            ..Default::default()
        },
        PublishProperties {
            user_properties: vec![("key\0".into(), "value".into())],
            ..Default::default()
        },
        PublishProperties {
            user_properties: vec![("key".into(), "value\0".into())],
            ..Default::default()
        },
        PublishProperties {
            user_properties: vec![(oversized.clone(), "value".into())],
            ..Default::default()
        },
        PublishProperties {
            content_type: Some("text\0plain".into()),
            ..Default::default()
        },
        PublishProperties {
            content_type: Some(oversized.clone()),
            ..Default::default()
        },
        PublishProperties {
            response_topic: Some("response/+".into()),
            ..Default::default()
        },
        PublishProperties {
            response_topic: Some("response\0".into()),
            ..Default::default()
        },
        PublishProperties {
            response_topic: Some(String::new()),
            ..Default::default()
        },
        PublishProperties {
            response_topic: Some(oversized.clone()),
            ..Default::default()
        },
        PublishProperties {
            correlation_data: Some(Bytes::from(oversized)),
            ..Default::default()
        },
    ];
    let (tx, rx) = flume::bounded(1);
    let client = AsyncClient::from_senders(tx);
    for validate in [false, true] {
        for properties in &invalid {
            let error = client
                .try_publish(
                    "validation/topic",
                    "valid text",
                    PublishOptions::at_most_once()
                        .properties(properties.clone())
                        .validate_payload(validate),
                )
                .unwrap_err();
            assert!(matches!(error, ClientError::InvalidRequest(_)), "{error:?}");
        }
    }
    assert!(rx.is_empty());
}
