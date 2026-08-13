use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    AckMode, AckToken, Command, Completion, DeliveryStatus, ErrorKind, NativeClient,
    PublishCommand, PublishProtocolOptions, QoS, SubscribeCommand, SubscribeProtocolOptions,
    Subscription, SubscriptionProtocolOptions, UnsubscribeCommand, UnsubscribeProtocolOptions,
    V5OutgoingPublishProperties, V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions,
    V5UnsubscribeProperties, WrapperEvent,
};

fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0];
    stream.read_exact(&mut header).ok()?;
    let mut remaining = 0_usize;
    let mut multiplier = 1_usize;
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).ok()?;
        remaining += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining];
    stream.read_exact(&mut body).ok()?;
    Some((header[0], body))
}

fn recv_ack_token(events: &mut rumqttc_wrapper_core::EventConsumer, duplicate: bool) -> AckToken {
    loop {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            Some(WrapperEvent::IncomingPublish(publish)) if publish.duplicate == duplicate => {
                return publish.ack_token.unwrap();
            }
            Some(_) => {}
            None => panic!("driver stopped before incoming publish"),
        }
    }
}

fn assert_commands_not_admitted(
    client_id: &str,
    mqtt5: bool,
    commands: impl IntoIterator<Item = Command>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        if mqtt5 {
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        } else {
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        }
        while let Some((header, _)) = read_frame(&mut stream) {
            assert!(
                !matches!(header >> 4, 3 | 8 | 10),
                "wrapper emitted an application packet after rejecting the command"
            );
            if header >> 4 == 14 {
                break;
            }
        }
    });
    let config = if mqtt5 {
        rumqttc_wrapper_core::ClientConfig::v5(client_id, "127.0.0.1", port)
    } else {
        rumqttc_wrapper_core::ClientConfig::v311(client_id, "127.0.0.1", port)
    };
    let mut native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));

    for command in commands {
        let error = handle.try_admit(command).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn mqtt5_publish_rejection_preserves_reason_code() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        while let Some((header, body)) = read_frame(&mut stream) {
            match header >> 4 {
                1 => stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap(),
                3 => {
                    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let offset = 2 + topic_len;
                    stream
                        .write_all(&[0x40, 0x04, body[offset], body[offset + 1], 0x87, 0x00])
                        .unwrap();
                }
                14 => break,
                _ => {}
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "negative-ack",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let admission = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "rejected".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    let error = admission
        .completion
        .wait_timeout(Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Protocol);
    assert_eq!(error.delivery_status(), DeliveryStatus::Rejected);
    assert_eq!(error.broker_reason(), Some(0x87));
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn incoming_mqtt5_subscription_identifiers_are_preserved() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap().0 >> 4, 1);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let topic = b"incoming/identifiers";
        let mut body = Vec::new();
        body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
        body.extend_from_slice(topic);
        body.extend_from_slice(&[4, 0x0b, 7, 0x0b, 9]);
        body.extend_from_slice(b"payload");
        stream
            .write_all(&[0x30, u8::try_from(body.len()).unwrap()])
            .unwrap();
        stream.write_all(&body).unwrap();

        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "incoming-subscription-identifiers",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let publish = loop {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            Some(WrapperEvent::IncomingPublish(publish)) => break publish,
            Some(_) => {}
            None => panic!("driver stopped before incoming publish"),
        }
    };
    assert_eq!(
        publish
            .v5_properties
            .as_ref()
            .unwrap()
            .subscription_identifiers,
        [7, 9]
    );

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn rejects_invalid_client_originated_mqtt5_publish_properties() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        while let Some((header, _)) = read_frame(&mut stream) {
            assert_ne!(header >> 4, 3, "wrapper emitted an invalid MQTT 5 PUBLISH");
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "invalid-subscription-id",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let assert_rejected = |properties, payload| {
        let error = handle
            .try_admit(Command::Publish(PublishCommand {
                topic: "invalid/property".into(),
                payload,
                qos: QoS::AtMostOnce,
                retain: false,
                protocol: PublishProtocolOptions::V5(properties),
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    };
    let payload = || Bytes::from_static(b"payload");

    for properties in [None, Some(V5OutgoingPublishProperties::default())] {
        let error = handle
            .try_admit(Command::Publish(PublishCommand {
                topic: String::new(),
                payload: payload(),
                qos: QoS::AtMostOnce,
                retain: false,
                protocol: properties.map_or(
                    PublishProtocolOptions::VersionNeutral,
                    PublishProtocolOptions::V5,
                ),
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }

    assert_rejected(
        V5OutgoingPublishProperties {
            payload_format_indicator: Some(2),
            ..V5OutgoingPublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5OutgoingPublishProperties {
            payload_format_indicator: Some(1),
            ..V5OutgoingPublishProperties::default()
        },
        Bytes::from_static(&[0xff]),
    );
    for alias in [0, 1] {
        assert_rejected(
            V5OutgoingPublishProperties {
                topic_alias: Some(alias),
                ..V5OutgoingPublishProperties::default()
            },
            payload(),
        );
    }
    for response_topic in ["", "response/+", "response/#", "response\0topic"] {
        assert_rejected(
            V5OutgoingPublishProperties {
                response_topic: Some(response_topic.into()),
                ..V5OutgoingPublishProperties::default()
            },
            payload(),
        );
    }
    assert_rejected(
        V5OutgoingPublishProperties {
            response_topic: Some("x".repeat(usize::from(u16::MAX) + 1)),
            ..V5OutgoingPublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5OutgoingPublishProperties {
            correlation_data: Some(Bytes::from(vec![0; usize::from(u16::MAX) + 1])),
            ..V5OutgoingPublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5OutgoingPublishProperties {
            user_properties: vec![("invalid\0key".into(), "value".into())],
            ..V5OutgoingPublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5OutgoingPublishProperties {
            content_type: Some("invalid\0type".into()),
            ..V5OutgoingPublishProperties::default()
        },
        payload(),
    );

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn rejects_unmapped_topic_alias_with_empty_topic() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        stream
            .write_all(&[0x20, 0x06, 0x00, 0x00, 0x03, 0x22, 0x00, 0x01])
            .unwrap();
        while let Some((header, _)) = read_frame(&mut stream) {
            assert_ne!(header >> 4, 3, "wrapper emitted an unmapped Topic Alias");
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "unmapped-topic-alias",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let error = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: String::new(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                topic_alias: Some(1),
                ..V5OutgoingPublishProperties::default()
            }),
        }))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Admission);
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn accepts_topic_alias_within_broker_advertised_maximum() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        // Successful CONNACK with Topic Alias Maximum = 1.
        stream
            .write_all(&[0x20, 0x06, 0x00, 0x00, 0x03, 0x22, 0x00, 0x01])
            .unwrap();
        for _ in 0..2 {
            let (header, _) = read_frame(&mut stream).unwrap();
            assert_eq!(header >> 4, 3);
        }
        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "valid-topic-alias",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let admission = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "valid/alias".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                topic_alias: Some(1),
                ..V5OutgoingPublishProperties::default()
            }),
        }))
        .unwrap();
    assert!(
        admission
            .completion
            .wait_timeout(Duration::from_secs(2))
            .is_ok()
    );
    let reused_alias = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: String::new(),
            payload: Bytes::from_static(b"second payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                topic_alias: Some(1),
                ..V5OutgoingPublishProperties::default()
            }),
        }))
        .unwrap();
    assert!(
        reused_alias
            .completion
            .wait_timeout(Duration::from_secs(2))
            .is_ok()
    );

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn mqtt5_subscribe_and_unsubscribe_extensions_reach_the_wire() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let (header, default_subscribe) = read_frame(&mut stream).unwrap();
        assert_eq!(header >> 4, 8);
        assert_eq!(
            &default_subscribe[2..],
            &[0, 0, 7, b'd', b'e', b'f', b'a', b'u', b'l', b't', 0]
        );
        stream
            .write_all(&[0x90, 0x04, default_subscribe[0], default_subscribe[1], 0, 0])
            .unwrap();

        let (header, subscribe) = read_frame(&mut stream).unwrap();
        assert_eq!(header >> 4, 8);
        assert_eq!(
            &subscribe[2..12],
            &[9, 0x0b, 7, 0x26, 0, 1, b'k', 0, 1, b'v']
        );
        assert_eq!(&subscribe[12..], &[0, 3, b'a', b'/', b'#', 0x1d]);
        stream
            .write_all(&[0x90, 0x04, subscribe[0], subscribe[1], 0, 1])
            .unwrap();

        let (header, unsubscribe) = read_frame(&mut stream).unwrap();
        assert_eq!(header >> 4, 10);
        assert_eq!(
            &unsubscribe[2..],
            &[7, 0x26, 0, 1, b'u', 0, 1, b'p', 0, 3, b'a', b'/', b'#']
        );
        stream
            .write_all(&[0xb0, 0x04, unsubscribe[0], unsubscribe[1], 0, 0])
            .unwrap();
        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut native = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v5(
        "subscription-extensions",
        "127.0.0.1",
        port,
    ))
    .unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));

    let default_subscribe = handle
        .try_admit(Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "default".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::V5(V5SubscriptionOptions::default()),
            }],
            protocol: SubscribeProtocolOptions::V5(V5SubscribeProperties::default()),
        }))
        .unwrap();
    assert!(matches!(
        default_subscribe
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::Subscribe(_)
    ));

    let subscribe = handle
        .try_admit(Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "a/#".into(),
                qos: QoS::AtLeastOnce,
                protocol: SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                    no_local: true,
                    retain_as_published: true,
                    retain_forward_rule: V5RetainForwardRule::OnNewSubscribe,
                }),
            }],
            protocol: SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: Some(7),
                user_properties: vec![("k".into(), "v".into())],
            }),
        }))
        .unwrap();
    assert!(matches!(
        subscribe
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::Subscribe(_)
    ));

    let unsubscribe = handle
        .try_admit(Command::Unsubscribe(UnsubscribeCommand {
            filters: vec!["a/#".into()],
            protocol: UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties {
                user_properties: vec![("u".into(), "p".into())],
            }),
        }))
        .unwrap();
    assert!(matches!(
        unsubscribe
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::Unsubscribe(_)
    ));

    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn mqtt5_publish_options_are_not_admitted_to_v311_clients() {
    assert_commands_not_admitted(
        "reject-v5-publish-options",
        false,
        [Command::Publish(PublishCommand {
            topic: "a".into(),
            payload: Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::V5(V5OutgoingPublishProperties::default()),
        })],
    );
}

#[test]
fn mqtt5_subscribe_properties_are_not_admitted_to_v311_clients() {
    assert_commands_not_admitted(
        "reject-v5-subscribe-properties",
        false,
        [Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "a/#".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::VersionNeutral,
            }],
            protocol: SubscribeProtocolOptions::V5(V5SubscribeProperties::default()),
        })],
    );
}

#[test]
fn mqtt5_subscription_options_are_not_admitted_to_v311_clients() {
    assert_commands_not_admitted(
        "reject-v5-subscription-options",
        false,
        [Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "a/#".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::V5(V5SubscriptionOptions::default()),
            }],
            protocol: SubscribeProtocolOptions::VersionNeutral,
        })],
    );
}

#[test]
fn mqtt5_unsubscribe_properties_are_not_admitted_to_v311_clients() {
    assert_commands_not_admitted(
        "reject-v5-unsubscribe-properties",
        false,
        [Command::Unsubscribe(UnsubscribeCommand {
            filters: vec!["a/#".into()],
            protocol: UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties::default()),
        })],
    );
}

#[test]
fn mqtt5_invalid_subscription_identifiers_are_not_admitted() {
    let commands = [0, 268_435_456].map(|subscription_identifier| {
        Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "a/#".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::VersionNeutral,
            }],
            protocol: SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: Some(subscription_identifier),
                user_properties: Vec::new(),
            }),
        })
    });
    assert_commands_not_admitted("reject-invalid-subscription-identifiers", true, commands);
}

#[test]
fn mqtt5_invalid_subscribe_user_properties_are_not_admitted() {
    let oversized = "x".repeat(usize::from(u16::MAX) + 1);
    let commands = [
        ("invalid\0key".into(), "value".into()),
        ("key".into(), "invalid\0value".into()),
        (oversized.clone(), "value".into()),
        ("key".into(), oversized.clone()),
    ]
    .map(|user_property| {
        Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "a/#".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::VersionNeutral,
            }],
            protocol: SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: None,
                user_properties: vec![user_property],
            }),
        })
    });
    assert_commands_not_admitted("reject-invalid-subscribe-properties", true, commands);
}

#[test]
fn mqtt5_invalid_unsubscribe_user_properties_are_not_admitted() {
    let oversized = "x".repeat(usize::from(u16::MAX) + 1);
    let commands = [
        ("invalid\0key".into(), "value".into()),
        ("key".into(), "invalid\0value".into()),
        (oversized.clone(), "value".into()),
        ("key".into(), oversized.clone()),
    ]
    .map(|user_property| {
        Command::Unsubscribe(UnsubscribeCommand {
            filters: vec!["a/#".into()],
            protocol: UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties {
                user_properties: vec![user_property],
            }),
        })
    });
    assert_commands_not_admitted("reject-invalid-unsubscribe-properties", true, commands);
}

#[test]
fn mqtt5_no_local_on_shared_subscription_is_not_admitted() {
    assert_commands_not_admitted(
        "reject-shared-no-local",
        true,
        [Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "$share/group/a/#".into(),
                qos: QoS::AtMostOnce,
                protocol: SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                    no_local: true,
                    ..V5SubscriptionOptions::default()
                }),
            }],
            protocol: SubscribeProtocolOptions::VersionNeutral,
        })],
    );
}

#[test]
fn manual_ack_token_is_single_use() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        // QoS 1 PUBLISH on topic "a", packet identifier 7, payload "x".
        stream
            .write_all(&[0x32, 0x06, 0x00, 0x01, b'a', 0x00, 0x07, b'x'])
            .unwrap();
        // Retransmit the same unacknowledged packet before the application responds.
        stream
            .write_all(&[0x3a, 0x06, 0x00, 0x01, b'a', 0x00, 0x07, b'x'])
            .unwrap();
        // Two QoS 0 publishes fill the one-slot wrapper event channel and block the driver from
        // polling the queued manual ACK until the consumer makes room.
        stream
            .write_all(&[0x30, 0x04, 0x00, 0x01, b'b', b'y'])
            .unwrap();
        stream
            .write_all(&[0x30, 0x04, 0x00, 0x01, b'c', b'z'])
            .unwrap();
        while let Some((header, _)) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
    });

    let mut config = rumqttc_wrapper_core::ClientConfig::v311("manual-ack", "127.0.0.1", port);
    config.common.ack_mode = AckMode::Manual;
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = Duration::from_secs(5);
    let mut native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    let token = recv_ack_token(&mut events, false);
    let duplicate_token = recv_ack_token(&mut events, true);
    assert_eq!(duplicate_token, token);

    let other_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let other_port = other_listener.local_addr().unwrap().port();
    let other_broker = thread::spawn(move || {
        let (mut stream, _) = other_listener.accept().unwrap();
        let _ = read_frame(&mut stream).unwrap();
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        while read_frame(&mut stream).is_some() {}
    });
    let mut other = NativeClient::start(rumqttc_wrapper_core::ClientConfig::v311(
        "other-client",
        "127.0.0.1",
        other_port,
    ))
    .unwrap();
    let other_handle = other.handle();
    let mut other_events = other.take_events().unwrap();
    assert!(matches!(
        other_events.recv_timeout(Duration::from_secs(2)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    let cross_client_error = other_handle
        .try_admit(Command::Acknowledge(token))
        .unwrap_err();
    assert_eq!(cross_client_error.kind(), ErrorKind::Admission);
    other_handle
        .try_admit(Command::ImmediateDisconnect)
        .unwrap();
    other.join(Duration::from_secs(2)).unwrap();
    other_broker.join().unwrap();

    thread::sleep(Duration::from_millis(50));
    let admission = handle.try_admit(Command::Acknowledge(token)).unwrap();
    let timeout = admission
        .completion
        .wait_timeout(Duration::from_millis(50))
        .unwrap_err();
    assert_eq!(timeout.kind(), ErrorKind::Timeout);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).unwrap(),
        Some(WrapperEvent::IncomingPublish(_))
    ));
    assert!(
        admission
            .completion
            .wait_timeout(Duration::from_secs(1))
            .is_ok()
    );
    let error = handle.try_admit(Command::Acknowledge(token)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Admission);
    let close = handle
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(1)),
        })
        .unwrap();
    assert!(
        close
            .completion
            .wait_timeout(Duration::from_secs(2))
            .is_ok()
    );
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}
