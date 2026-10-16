mod support;

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use rumqttc_wrapper_core::{
    AckMode, ClientHandle, Command, Completion, ErrorKind, EventConsumer, IncomingPublish, QoS,
    WrapperEvent,
};
use support::DEADLINE;

fn publish(stream: &mut TcpStream, mqtt5: bool, qos: u8, id: u16, duplicate: bool, payload: &[u8]) {
    let mut body = vec![0, 1, b'a'];
    if qos > 0 {
        body.extend_from_slice(&id.to_be_bytes());
    }
    if mqtt5 {
        body.push(0);
    }
    body.extend_from_slice(payload);
    stream
        .write_all(&[
            0x30 | (qos << 1) | (u8::from(duplicate) << 3),
            u8::try_from(body.len()).unwrap(),
        ])
        .unwrap();
    stream.write_all(&body).unwrap();
}

fn ack(stream: &mut TcpStream, header: u8, id: u16) {
    let frame = support::frame(stream);
    assert_eq!(frame[0], header);
    assert_eq!(&frame[2..4], &id.to_be_bytes());
}

fn receive(
    events: &mut EventConsumer,
    payload: &[u8],
    id: Option<u16>,
    generation: u64,
) -> IncomingPublish {
    let WrapperEvent::IncomingPublish(publish) = support::until(events, |event| {
        matches!(event, WrapperEvent::IncomingPublish(_))
    }) else {
        unreachable!()
    };
    assert_eq!(&publish.payload[..], payload);
    assert_eq!(publish.packet_id, id);
    assert_eq!(publish.connection_generation, Some(generation));
    *publish
}

fn acknowledge(handle: &ClientHandle, publish: &IncomingPublish, manual: bool) {
    assert_eq!(
        publish.ack_token.is_some(),
        manual && publish.qos != QoS::AtMostOnce
    );
    if let Some(token) = publish.ack_token {
        assert_eq!(
            handle
                .try_admit(Command::Acknowledge(token))
                .unwrap()
                .completion
                .wait_timeout(DEADLINE)
                .unwrap(),
            Completion::Acknowledged
        );
        assert!(handle.try_admit(Command::Acknowledge(token)).is_err());
    }
}

#[test]
fn incoming_identifiers_are_owned_observations_across_ack_modes_and_reconnects() {
    for mqtt5 in [false, true] {
        for manual in [false, true] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (disconnect_tx, disconnect_rx) = mpsc::channel();
            let broker = thread::spawn(move || {
                let mut stream = support::accept(&listener);
                support::connect(&mut stream, mqtt5);
                publish(&mut stream, mqtt5, 0, 0, false, b"zero");
                for (duplicate, payload) in [
                    (false, b"one".as_slice()),
                    (true, b"one"),
                    (false, b"reuse"),
                ] {
                    publish(&mut stream, mqtt5, 1, 0x1234, duplicate, payload);
                    ack(&mut stream, 0x40, 0x1234);
                }
                publish(&mut stream, mqtt5, 2, u16::MAX, false, b"two");
                ack(&mut stream, 0x50, u16::MAX);
                // Native QoS 2 suppression must still ACK the duplicate without
                // delivering another application event.
                publish(&mut stream, mqtt5, 2, u16::MAX, true, b"two");
                ack(&mut stream, 0x50, u16::MAX);
                stream.write_all(&[0x62, 2, 0xff, 0xff]).unwrap();
                ack(&mut stream, 0x70, u16::MAX);
                publish(&mut stream, mqtt5, 0, 0, false, b"done");
                publish(&mut stream, mqtt5, 1, 0x1234, false, b"stale");
                if !manual {
                    ack(&mut stream, 0x40, 0x1234);
                }
                disconnect_rx.recv_timeout(DEADLINE).unwrap();
                drop(stream);
                let mut stream = support::accept(&listener);
                support::connect(&mut stream, mqtt5);
                publish(&mut stream, mqtt5, 1, 0x1234, false, b"new");
                ack(&mut stream, 0x40, 0x1234);
                assert_eq!(support::frame(&mut stream)[0], 0xe0);
            });
            let mut config = support::config(mqtt5, port);
            config.common.ack_mode = if manual {
                AckMode::Manual
            } else {
                AckMode::Automatic
            };
            let mut client = support::start(config).unwrap();
            let handle = client.handle();
            let mut events = support::connected(&mut client);
            let zero = receive(&mut events, b"zero", None, 1);
            acknowledge(&handle, &zero, manual);
            let first = receive(&mut events, b"one", Some(0x1234), 1);
            assert!(!first.duplicate);
            acknowledge(&handle, &first, manual);
            let repeated = receive(&mut events, b"one", Some(0x1234), 1);
            assert!(repeated.duplicate);
            acknowledge(&handle, &repeated, manual);
            let reused = receive(&mut events, b"reuse", Some(0x1234), 1);
            assert!(!reused.duplicate);
            acknowledge(&handle, &reused, manual);
            let two = receive(&mut events, b"two", Some(u16::MAX), 1);
            acknowledge(&handle, &two, manual);
            receive(&mut events, b"done", None, 1);
            let stale = receive(&mut events, b"stale", Some(0x1234), 1);
            assert_eq!(stale.ack_token.is_some(), manual);
            disconnect_tx.send(()).unwrap();
            let current = receive(&mut events, b"new", Some(0x1234), 2);
            if let Some(token) = stale.ack_token {
                assert_eq!(
                    handle
                        .try_admit(Command::Acknowledge(token))
                        .unwrap_err()
                        .kind(),
                    ErrorKind::Admission
                );
                assert_ne!(Some(token), current.ack_token);
            }
            acknowledge(&handle, &current, manual);
            handle.try_admit(Command::ImmediateDisconnect).unwrap();
            client.join(DEADLINE).unwrap();
            drop(events);
            drop(handle);
            drop(client);
            broker.join().unwrap();
            // Owned metadata survives consumption of tokens and destruction of the client.
            assert_eq!(
                (first.packet_id, first.connection_generation),
                (Some(0x1234), Some(1))
            );
            assert_eq!(
                (two.packet_id, two.connection_generation),
                (Some(u16::MAX), Some(1))
            );
            assert_eq!(
                (stale.packet_id, stale.connection_generation),
                (Some(0x1234), Some(1))
            );
            assert_eq!(
                (current.packet_id, current.connection_generation),
                (Some(0x1234), Some(2))
            );
        }
    }
}

#[test]
fn zero_qos1_and_qos2_identifiers_are_rejected_before_publish_observation() {
    for mqtt5 in [false, true] {
        for qos in [1, 2] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = thread::spawn(move || {
                let mut stream = support::accept(&listener);
                support::connect(&mut stream, mqtt5);
                publish(&mut stream, mqtt5, qos, 0, false, b"invalid");
                // Retain the connection until native validation closes it.
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut stream, &mut bytes).unwrap();
            });
            let mut client = support::start(support::config(mqtt5, port)).unwrap();
            let mut events = support::connected(&mut client);
            let event = events.recv_timeout(DEADLINE).unwrap().unwrap();
            assert!(matches!(
                event,
                WrapperEvent::Disconnected { .. } | WrapperEvent::DriverTerminated(_)
            ));
            client.handle().close_now_idempotent();
            client.join(DEADLINE).unwrap();
            broker.join().unwrap();
        }
    }
}
