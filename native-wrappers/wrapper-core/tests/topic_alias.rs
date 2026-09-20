mod support;

use std::io::Write;
use std::net::TcpListener;

use rumqttc_wrapper_core::*;
use support::*;

fn read_publish(socket: &mut std::net::TcpStream) -> rumqttc_v5::Publish {
    let rumqttc_v5::Packet::Publish(packet) =
        rumqttc_v5::Packet::read(&mut frame(socket), None).unwrap()
    else {
        panic!("PUBLISH")
    };
    packet
}

fn command(explicit: bool, alias_only: bool) -> Command {
    Command::Publish(PublishCommand {
        topic: if alias_only { "" } else { "topic" }.into(),
        payload: b"payload".as_slice().into(),
        qos: QoS::AtLeastOnce,
        retain: false,
        protocol: if explicit {
            PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                topic_alias: Some(1),
                ..Default::default()
            })
        } else {
            PublishProtocolOptions::VersionNeutral
        },
    })
}

#[test]
fn automatic_and_explicit_aliases_replay_concrete_topics_under_new_connack_limits() {
    for policy in [
        TopicAliasPolicy::Disabled,
        TopicAliasPolicy::Monotonic,
        TopicAliasPolicy::Lru,
    ] {
        let explicit = policy == TopicAliasPolicy::Disabled;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(true, listener.local_addr().unwrap().port());
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.topic_alias_policy = policy;
        v5.clean_start = false;
        v5.connect_properties.session_expiry_interval = Some(60);
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            frame(&mut socket);
            socket.write_all(&[0x20, 6, 0, 0, 3, 0x22, 0, 2]).unwrap();
            let first = read_publish(&mut socket);
            assert_eq!(first.topic.as_ref(), b"topic");
            assert_eq!(first.properties.unwrap().topic_alias, Some(1));
            puback(&mut socket, first.pkid);
            let second = read_publish(&mut socket);
            assert!(second.topic.is_empty());
            assert_eq!(second.properties.unwrap().topic_alias, Some(1));
            drop(socket);
            let mut socket = accept(&listener);
            frame(&mut socket);
            socket.write_all(&[0x20, 6, 1, 0, 3, 0x22, 0, 0]).unwrap();
            let replay = read_publish(&mut socket);
            assert_eq!(replay.topic.as_ref(), b"topic");
            assert_eq!(replay.pkid, second.pkid);
            assert!(replay.dup);
            assert_eq!(replay.properties.and_then(|p| p.topic_alias), None);
            puback(&mut socket, replay.pkid);
            let fresh = read_publish(&mut socket);
            assert_eq!(fresh.topic.as_ref(), b"topic");
            assert_eq!(fresh.properties.and_then(|p| p.topic_alias), None);
            puback(&mut socket, fresh.pkid);
            assert_eq!(frame(&mut socket)[0], 0xe0);
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = connected(&mut client);
        let first = client.handle().try_admit(command(explicit, false)).unwrap();
        assert_eq!(
            terminal(&first).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        );
        let second = client
            .handle()
            .try_admit(command(explicit, explicit))
            .unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert_eq!(
            terminal(&second).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        );
        let error = client.handle().try_admit(command(true, true)).unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
        let fresh = client.handle().try_admit(command(false, false)).unwrap();
        assert_eq!(
            terminal(&fresh).unwrap(),
            Completion::Publish(PublishCompletion::Qos1Acknowledged)
        );
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn alias_rejection_retains_broker_reason_and_connection_generation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(true, listener.local_addr().unwrap().port());
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        frame(&mut socket);
        socket.write_all(&[0x20, 6, 0, 0, 3, 0x22, 0, 1]).unwrap();
        read_publish(&mut socket);
        socket.write_all(&[0xe0, 2, 0x94, 0]).unwrap();
    });
    let mut client = NativeClient::start(config).unwrap();
    let mut events = connected(&mut client);
    let operation = client.handle().try_admit(command(true, false)).unwrap();
    let event = until(&mut events, |event| {
        matches!(event, WrapperEvent::Disconnected { .. })
    });
    let WrapperEvent::Disconnected { error, phase } = event else {
        unreachable!()
    };
    assert_eq!(error.broker_reason(), Some(0x94));
    assert_eq!(phase, ConnectionPhase::Established);
    assert_eq!(error.context().generation, Some(1));
    client.closer().close_now(DEADLINE).unwrap();
    assert_eq!(
        terminal(&operation).unwrap_err().delivery_status(),
        DeliveryStatus::Ambiguous
    );
    broker.join();
}

#[test]
fn rejected_connect_properties_preserve_reason_before_first_generation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = config(true, listener.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.topic_alias_maximum = Some(1);
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        let rumqttc_v5::Packet::Connect(connect, _, _) =
            rumqttc_v5::Packet::read(&mut frame(&mut socket), None).unwrap()
        else {
            panic!("CONNECT")
        };
        assert_eq!(connect.properties.unwrap().topic_alias_max, Some(1));
        release_rx.recv_timeout(DEADLINE).unwrap();
        socket.write_all(&[0x20, 3, 0, 0x82, 0]).unwrap();
    });
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let pending = client
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "queued".into(),
            payload: bytes::Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    release_tx.send(()).unwrap();
    let rejected = until(&mut events, |event| {
        matches!(event, WrapperEvent::ConnectionRejected(_))
    });
    let WrapperEvent::ConnectionRejected(details) = rejected else {
        unreachable!()
    };
    assert_eq!(details.reason_code, 0x82);
    let event = until(&mut events, |event| {
        matches!(event, WrapperEvent::Disconnected { .. })
    });
    let WrapperEvent::Disconnected { error, phase } = event else {
        unreachable!()
    };
    assert_eq!(error.broker_reason(), Some(0x82));
    assert_eq!(error.kind(), ErrorKind::Protocol);
    assert_eq!(phase, ConnectionPhase::Attempt);
    assert_eq!(error.context().generation, None);
    client.closer().close_now(DEADLINE).unwrap();
    assert_eq!(
        terminal(&pending).unwrap_err().delivery_status(),
        DeliveryStatus::Ambiguous
    );
    broker.join();
}
