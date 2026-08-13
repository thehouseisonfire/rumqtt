use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rumqttc_wrapper_core::{
    ClientConfig, Command, Completion, DeliveryStatus, NativeClient, ProtocolVersion,
    PublishCommand, PublishCompletion, PublishProtocolOptions, QoS, WrapperEvent,
};

fn read_frame(stream: &mut TcpStream) -> Option<u8> {
    read_frame_with_body(stream).map(|(header, _)| header)
}

fn wait_connected(events: &mut rumqttc_wrapper_core::EventConsumer) {
    loop {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            Some(WrapperEvent::Connected { .. }) => return,
            Some(_) => {}
            None => panic!("driver stopped before connection"),
        }
    }
}

fn config(protocol: ProtocolVersion, port: u16) -> ClientConfig {
    match protocol {
        ProtocolVersion::V311 => ClientConfig::v311("graceful", "127.0.0.1", port),
        ProtocolVersion::V5 => ClientConfig::v5("graceful", "127.0.0.1", port),
    }
}

fn assert_graceful_shutdown_drains_ready_publish(protocol: ProtocolVersion) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (publish_tx, publish_rx) = mpsc::sync_channel(0);
    let (ack_tx, ack_rx) = mpsc::sync_channel(0);
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
        match protocol {
            ProtocolVersion::V311 => stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(),
            ProtocolVersion::V5 => stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap(),
        }

        let (packet_id_high, packet_id_low) = loop {
            let (header, body) = read_frame_with_body(&mut stream).unwrap();
            if header >> 4 == 3 {
                let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                let offset = 2 + topic_len;
                break (body[offset], body[offset + 1]);
            }
        };
        publish_tx.send(()).unwrap();
        ack_rx.recv().unwrap();
        stream
            .write_all(&[0x40, 0x02, packet_id_high, packet_id_low])
            .unwrap();
        while read_frame(&mut stream).is_some() {}
    });

    let mut native = NativeClient::start(config(protocol, port)).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    wait_connected(&mut events);
    let publish = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "graceful/pending".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    publish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let close = handle
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(2)),
        })
        .unwrap();
    ack_tx.send(()).unwrap();

    assert_eq!(
        close
            .completion
            .wait_timeout(Duration::from_secs(3))
            .unwrap(),
        Completion::GracefulShutdown
    );
    assert_eq!(
        publish
            .completion
            .wait_timeout(Duration::from_secs(1))
            .unwrap(),
        Completion::Publish(PublishCompletion::Qos1Acknowledged)
    );
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

fn read_frame_with_body(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
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

#[test]
fn graceful_shutdown_resolves_protocol_admitted_work_for_both_protocols() {
    assert_graceful_shutdown_drains_ready_publish(ProtocolVersion::V311);
    assert_graceful_shutdown_drains_ready_publish(ProtocolVersion::V5);
}

fn assert_immediate_shutdown_keeps_unfinished_publish_ambiguous(protocol: ProtocolVersion) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
        match protocol {
            ProtocolVersion::V311 => stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(),
            ProtocolVersion::V5 => stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap(),
        }
        while read_frame(&mut stream).is_some() {}
    });

    let mut native = NativeClient::start(config(protocol, port)).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    wait_connected(&mut events);
    let publish = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "unfinished".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    let close = handle.try_admit(Command::ImmediateDisconnect).unwrap();
    assert_eq!(
        close
            .completion
            .wait_timeout(Duration::from_secs(2))
            .unwrap(),
        Completion::ImmediateShutdown
    );
    let error = publish
        .completion
        .wait_timeout(Duration::from_secs(1))
        .unwrap_err();
    assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
    native.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn immediate_shutdown_keeps_unfinished_publish_ambiguous_for_both_protocols() {
    assert_immediate_shutdown_keeps_unfinished_publish_ambiguous(ProtocolVersion::V311);
    assert_immediate_shutdown_keeps_unfinished_publish_ambiguous(ProtocolVersion::V5);
}

fn assert_immediate_shutdown_interrupts_connection_establishment(protocol: ProtocolVersion) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (connect_tx, connect_rx) = mpsc::sync_channel(0);
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
        connect_tx.send(()).unwrap();

        let mut byte = [0];
        assert_eq!(
            stream.read(&mut byte).unwrap(),
            0,
            "driver did not close the in-progress connection attempt"
        );
    });

    let mut config = config(protocol, port);
    config.common.connection_timeout = Duration::from_secs(5);
    let native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    connect_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let started = Instant::now();
    let close = handle.try_admit(Command::ImmediateDisconnect).unwrap();
    assert_eq!(
        close
            .completion
            .wait_timeout(Duration::from_secs(1))
            .unwrap(),
        Completion::ImmediateShutdown
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    native.join(Duration::from_secs(1)).unwrap();
    broker.join().unwrap();
}

#[test]
fn immediate_shutdown_interrupts_connection_establishment_for_both_protocols() {
    assert_immediate_shutdown_interrupts_connection_establishment(ProtocolVersion::V311);
    assert_immediate_shutdown_interrupts_connection_establishment(ProtocolVersion::V5);
}

fn assert_immediate_shutdown_bypasses_repeated_event_backpressure(protocol: ProtocolVersion) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (published_tx, published_rx) = mpsc::sync_channel(0);
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
        match protocol {
            ProtocolVersion::V311 => {
                stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
                stream
                    .write_all(&[0x30, 0x04, 0x00, 0x01, b'a', b'x'])
                    .unwrap();
            }
            ProtocolVersion::V5 => {
                stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
                stream
                    .write_all(&[0x30, 0x05, 0x00, 0x01, b'a', 0x00, b'x'])
                    .unwrap();
            }
        }
        published_tx.send(()).unwrap();
        while read_frame(&mut stream).is_some() {}
    });

    let mut config = config(protocol, port);
    config.common.event_buffer_capacity = 1;
    // Keep the overflow deadline well beyond the shutdown assertion below. A short deadline
    // makes this test race driver-side overflow against the test thread being scheduled to
    // submit the immediate disconnect, particularly on slower CI hosts.
    config.common.event_delivery_timeout = Duration::from_secs(5);
    config.common.emit_outgoing_events = true;
    let native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    published_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    thread::sleep(Duration::from_millis(20));

    let close = handle.try_admit(Command::ImmediateDisconnect).unwrap();
    assert_eq!(
        close
            .completion
            .wait_timeout(Duration::from_secs(1))
            .unwrap(),
        Completion::ImmediateShutdown
    );
    native.join(Duration::from_secs(1)).unwrap();
    broker.join().unwrap();
}

#[test]
fn immediate_shutdown_bypasses_repeated_event_backpressure_for_both_protocols() {
    assert_immediate_shutdown_bypasses_repeated_event_backpressure(ProtocolVersion::V311);
    assert_immediate_shutdown_bypasses_repeated_event_backpressure(ProtocolVersion::V5);
}

#[test]
fn dropping_owner_escalates_an_unbounded_graceful_shutdown() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (publish_tx, publish_rx) = mpsc::sync_channel(0);
    let (terminated_tx, terminated_rx) = mpsc::sync_channel(0);
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        loop {
            let (header, _) = read_frame_with_body(&mut stream).unwrap();
            if header >> 4 == 3 {
                publish_tx.send(()).unwrap();
                break;
            }
        }
        while let Some(header) = read_frame(&mut stream) {
            if header >> 4 == 14 {
                break;
            }
        }
        terminated_tx.send(()).unwrap();
    });

    let mut native =
        NativeClient::start(ClientConfig::v311("drop-escalation", "127.0.0.1", port)).unwrap();
    let handle = native.handle();
    let mut events = native.take_events().unwrap();
    wait_connected(&mut events);
    handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "shutdown/pending".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    publish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let close = handle
        .try_admit(Command::GracefulDisconnect { timeout: None })
        .unwrap();

    drop(native);

    terminated_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("owner drop did not terminate the graceful close");
    let error = close
        .completion
        .wait_timeout(Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
    drop(handle);
    broker.join().unwrap();
}

#[test]
fn repeated_start_and_close_cycles_join_every_driver_thread() {
    const CYCLES: usize = 8;
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        for _ in 0..CYCLES {
            let (mut stream, _) = listener.accept().unwrap();
            assert_eq!(read_frame(&mut stream).unwrap() >> 4, 1);
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
            while read_frame(&mut stream).is_some() {}
        }
    });

    for cycle in 0..CYCLES {
        let mut native = NativeClient::start(ClientConfig::v311(
            format!("cycle-{cycle}"),
            "127.0.0.1",
            port,
        ))
        .unwrap();
        let handle = native.handle();
        let mut events = native.take_events().unwrap();
        wait_connected(&mut events);
        let close = handle.try_admit(Command::ImmediateDisconnect).unwrap();
        assert_eq!(
            close
                .completion
                .wait_timeout(Duration::from_secs(2))
                .unwrap(),
            Completion::ImmediateShutdown
        );
        native.join(Duration::from_secs(2)).unwrap();
    }
    broker.join().unwrap();
}
