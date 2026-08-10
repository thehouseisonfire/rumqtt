use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    AckMode, AckToken, Command, DeliveryStatus, ErrorKind, NativeClient, PublishCommand, QoS,
    V5PublishProperties, WrapperEvent,
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
            v5_properties: None,
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
                v5_properties: Some(properties),
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    };
    let payload = || Bytes::from_static(b"payload");

    for properties in [None, Some(V5PublishProperties::default())] {
        let error = handle
            .try_admit(Command::Publish(PublishCommand {
                topic: String::new(),
                payload: payload(),
                qos: QoS::AtMostOnce,
                retain: false,
                v5_properties: properties,
            }))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }

    assert_rejected(
        V5PublishProperties {
            subscription_identifiers: vec![7],
            ..V5PublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5PublishProperties {
            payload_format_indicator: Some(2),
            ..V5PublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5PublishProperties {
            payload_format_indicator: Some(1),
            ..V5PublishProperties::default()
        },
        Bytes::from_static(&[0xff]),
    );
    for alias in [0, 1] {
        assert_rejected(
            V5PublishProperties {
                topic_alias: Some(alias),
                ..V5PublishProperties::default()
            },
            payload(),
        );
    }
    for response_topic in ["", "response/+", "response/#", "response\0topic"] {
        assert_rejected(
            V5PublishProperties {
                response_topic: Some(response_topic.into()),
                ..V5PublishProperties::default()
            },
            payload(),
        );
    }
    assert_rejected(
        V5PublishProperties {
            response_topic: Some("x".repeat(usize::from(u16::MAX) + 1)),
            ..V5PublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5PublishProperties {
            correlation_data: Some(Bytes::from(vec![0; usize::from(u16::MAX) + 1])),
            ..V5PublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5PublishProperties {
            user_properties: vec![("invalid\0key".into(), "value".into())],
            ..V5PublishProperties::default()
        },
        payload(),
    );
    assert_rejected(
        V5PublishProperties {
            content_type: Some("invalid\0type".into()),
            ..V5PublishProperties::default()
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
            v5_properties: Some(V5PublishProperties {
                topic_alias: Some(1),
                ..V5PublishProperties::default()
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
            v5_properties: Some(V5PublishProperties {
                topic_alias: Some(1),
                ..V5PublishProperties::default()
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
            v5_properties: Some(V5PublishProperties {
                topic_alias: Some(1),
                ..V5PublishProperties::default()
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
