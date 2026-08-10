use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rumqttc_wrapper_core::{
    ClientConfig, Command, Completion, DiagnosticsSnapshot, ErrorKind, NativeClient,
    ProtocolVersion, PublishCommand, PublishCompletion, QoS, SubscribeCommand, Subscription,
    WrapperEvent,
};

fn spawn_broker(protocol: ProtocolVersion) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        while let Some((header, body)) = read_frame(&mut stream) {
            match header >> 4 {
                1 => match protocol {
                    ProtocolVersion::V311 => stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(),
                    ProtocolVersion::V5 => {
                        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
                    }
                },
                3 => {
                    let qos = (header >> 1) & 0x03;
                    if qos != 0 {
                        let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                        let offset = 2 + topic_len;
                        let packet_id = [body[offset], body[offset + 1]];
                        let response = if qos == 1 { 0x40 } else { 0x50 };
                        stream
                            .write_all(&[response, 0x02, packet_id[0], packet_id[1]])
                            .unwrap();
                    }
                }
                6 => {
                    stream.write_all(&[0x70, 0x02, body[0], body[1]]).unwrap();
                }
                8 => {
                    let packet_id = [body[0], body[1]];
                    match protocol {
                        ProtocolVersion::V311 => stream
                            .write_all(&[0x90, 0x03, packet_id[0], packet_id[1], 0x00])
                            .unwrap(),
                        ProtocolVersion::V5 => stream
                            .write_all(&[0x90, 0x04, packet_id[0], packet_id[1], 0x00, 0x00])
                            .unwrap(),
                    }
                }
                10 => {
                    let packet_id = [body[0], body[1]];
                    match protocol {
                        ProtocolVersion::V311 => stream
                            .write_all(&[0xb0, 0x02, packet_id[0], packet_id[1]])
                            .unwrap(),
                        ProtocolVersion::V5 => stream
                            .write_all(&[0xb0, 0x04, packet_id[0], packet_id[1], 0x00, 0x00])
                            .unwrap(),
                    }
                }
                14 => break,
                _ => {}
            }
        }
    });
    (port, join)
}

fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0_u8; 1];
    if stream.read_exact(&mut header).is_err() {
        return None;
    }
    let mut multiplier = 1_usize;
    let mut remaining = 0_usize;
    loop {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).unwrap();
        remaining += usize::from(byte[0] & 0x7f) * multiplier;
        if byte[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining];
    stream.read_exact(&mut body).unwrap();
    Some((header[0], body))
}

fn wait_connected(events: &mut rumqttc_wrapper_core::EventConsumer) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if matches!(
            events.recv_timeout(Duration::from_millis(100)).unwrap(),
            Some(WrapperEvent::Connected { .. })
        ) {
            return;
        }
    }
    panic!("client did not connect");
}

fn config(protocol: ProtocolVersion, port: u16) -> ClientConfig {
    match protocol {
        ProtocolVersion::V311 => ClientConfig::v311("wrapper-test", "127.0.0.1", port),
        ProtocolVersion::V5 => ClientConfig::v5("wrapper-test", "127.0.0.1", port),
    }
}

#[test]
fn sustained_diagnostics_do_not_starve_mqtt_progress() {
    const PRODUCERS: usize = 4;
    let (port, broker) = spawn_broker(ProtocolVersion::V311);
    let mut config = config(ProtocolVersion::V311, port);
    config.common.request_channel_capacity = 64;
    let mut native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    wait_connected(&mut events);

    let stop = Arc::new(AtomicBool::new(false));
    let producers = (0..PRODUCERS)
        .map(|_| {
            let handle = handle.clone();
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match handle.try_admit(Command::Diagnostics) {
                        Ok(admission) => drop(admission.completion),
                        Err(error) if error.kind() == ErrorKind::Backpressure => {
                            thread::yield_now();
                        }
                        Err(error) if error.kind() == ErrorKind::Shutdown => break,
                        Err(error) => panic!("unexpected diagnostics error: {error}"),
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    thread::sleep(Duration::from_millis(20));

    let publish = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "fairness/progress".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            v5_properties: None,
        }))
        .unwrap();
    let result = publish.completion.wait_timeout(Duration::from_secs(2));

    stop.store(true, Ordering::Release);
    for producer in producers {
        producer.join().unwrap();
    }
    assert_eq!(
        result.unwrap(),
        Completion::Publish(PublishCompletion::Qos1Acknowledged)
    );
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[allow(clippy::too_many_lines)]
fn run_lifecycle(protocol: ProtocolVersion) {
    let (port, broker) = spawn_broker(protocol);
    let mut native = NativeClient::start(config(protocol, port)).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    wait_connected(&mut events);

    let publish = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "wrapper/test".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            v5_properties: None,
        }))
        .unwrap();
    assert!(matches!(
        publish
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::Publish(PublishCompletion::Qos0Flushed)
    ));

    for (qos, expected) in [
        (QoS::AtLeastOnce, PublishCompletion::Qos1Acknowledged),
        (QoS::ExactlyOnce, PublishCompletion::Qos2Completed),
    ] {
        let publish = handle
            .try_admit(Command::Publish(PublishCommand {
                topic: "wrapper/tracked".into(),
                payload: Bytes::from_static(b"payload"),
                qos,
                retain: false,
                v5_properties: None,
            }))
            .unwrap();
        assert_eq!(
            publish
                .completion
                .wait_timeout(Duration::from_secs(2))
                .unwrap(),
            Completion::Publish(expected)
        );
    }

    let dropped_waiter = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "wrapper/dropped-waiter".into(),
            payload: Bytes::from_static(b"still-delivered"),
            qos: QoS::AtLeastOnce,
            retain: false,
            v5_properties: None,
        }))
        .unwrap();
    drop(dropped_waiter.completion);

    let subscribe = handle
        .try_admit(Command::Subscribe(SubscribeCommand {
            filters: vec![Subscription {
                filter: "wrapper/#".into(),
                qos: QoS::AtMostOnce,
            }],
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
        .try_admit(Command::Unsubscribe(vec!["wrapper/#".into()]))
        .unwrap();
    assert!(matches!(
        unsubscribe
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::Unsubscribe(_)
    ));

    let diagnostics = handle.try_admit(Command::Diagnostics).unwrap();
    match diagnostics
        .completion
        .wait_timeout(Duration::from_secs(2))
        .unwrap()
    {
        Completion::Diagnostics(DiagnosticsSnapshot {
            connected: true, ..
        }) => {}
        other => panic!("unexpected diagnostics completion: {other:?}"),
    }

    let close = handle
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(2)),
        })
        .unwrap();
    assert!(matches!(
        close
            .completion
            .wait_timeout(Duration::from_secs(3))
            .unwrap(),
        Completion::GracefulShutdown
    ));
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn v311_lifecycle_uses_owned_boundary() {
    run_lifecycle(ProtocolVersion::V311);
}

#[test]
fn v5_lifecycle_uses_owned_boundary() {
    run_lifecycle(ProtocolVersion::V5);
}

#[test]
fn bounded_request_channel_reports_backpressure() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut config = ClientConfig::v311("backpressure", "127.0.0.1", port);
    config.common.request_channel_capacity = 1;
    config.common.event_buffer_capacity = 32;
    let native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    let command = || {
        Command::Publish(PublishCommand {
            topic: "wrapper/backpressure".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            v5_properties: None,
        })
    };
    handle.try_admit(command()).unwrap();
    let error = handle.try_admit(command()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Backpressure);
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    native.join(Duration::from_secs(2)).unwrap();
}
