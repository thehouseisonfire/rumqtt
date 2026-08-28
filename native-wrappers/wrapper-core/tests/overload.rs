use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rumqttc_wrapper_core::{
    ClientConfig, ErrorCode, ErrorKind, NativeClient, ProtocolVersion, WrapperEvent,
};

fn assert_full_event_buffer_terminates(mqtt5: bool) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut scratch = [0_u8; 512];
        let _ = stream.read(&mut scratch).unwrap();
        if mqtt5 {
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        } else {
            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        }
        thread::sleep(Duration::from_millis(20));
        // QoS 0 PUBLISH, topic "a", payload "x".
        if mqtt5 {
            stream
                .write_all(&[0x30, 0x05, 0x00, 0x01, b'a', 0x00, b'x'])
                .unwrap();
        } else {
            stream
                .write_all(&[0x30, 0x04, 0x00, 0x01, b'a', b'x'])
                .unwrap();
        }
        thread::sleep(Duration::from_millis(300));
    });

    let mut config = if mqtt5 {
        ClientConfig::v5("overflow-v5", "127.0.0.1", port)
    } else {
        ClientConfig::v4("overflow-v4", "127.0.0.1", port)
    };
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = Duration::from_millis(50);
    let mut native = NativeClient::start(config).unwrap();
    let mut events = native.take_events().unwrap();

    // Do not drain the ordinary event buffer until the driver has terminated. A fixed sleep is
    // racy under load: if the driver has not yet reached the delivery timeout, receiving
    // `Connected` frees the slot and allows the pending publish to be delivered successfully.
    native.join(Duration::from_secs(1)).unwrap();

    assert!(matches!(
        events.recv_timeout(Duration::from_secs(1)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    match events.recv_timeout(Duration::from_secs(1)).unwrap() {
        Some(WrapperEvent::DriverTerminated(error)) => {
            assert_eq!(error.kind(), ErrorKind::Backpressure);
        }
        other => panic!("unexpected terminal event: {other:?}"),
    }
    broker.join().unwrap();
}

#[test]
fn full_event_buffer_terminates_through_reserved_status_path_for_both_protocols() {
    assert_full_event_buffer_terminates(false);
    assert_full_event_buffer_terminates(true);
}

fn assert_boundary_panic_interrupts_full_event_buffer(protocol: ProtocolVersion) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (publish_tx, publish_rx) = mpsc::sync_channel(0);
    let (published_tx, published_rx) = mpsc::sync_channel(0);
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut scratch = [0_u8; 512];
        let _ = stream.read(&mut scratch).unwrap();
        match protocol {
            ProtocolVersion::V4 => stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(),
            ProtocolVersion::V5 => stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap(),
        }
        publish_rx.recv().unwrap();
        match protocol {
            ProtocolVersion::V4 => stream
                .write_all(&[0x30, 0x04, 0x00, 0x01, b'a', b'x'])
                .unwrap(),
            ProtocolVersion::V5 => stream
                .write_all(&[0x30, 0x05, 0x00, 0x01, b'a', 0x00, b'x'])
                .unwrap(),
        }
        published_tx.send(()).unwrap();
        while stream.read(&mut scratch).is_ok_and(|read| read != 0) {}
    });

    let mut config = match protocol {
        ProtocolVersion::V4 => ClientConfig::v4("panic-delivery-v4", "127.0.0.1", port),
        ProtocolVersion::V5 => ClientConfig::v5("panic-delivery-v5", "127.0.0.1", port),
    };
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = Duration::from_secs(30);
    config.common.emit_outgoing_events = true;
    let mut native = NativeClient::start(config).unwrap();
    let handle = native.handle();
    let connection = native.connection();

    // Connection observation is updated immediately before Connected is delivered. Keep that event
    // (or an earlier outgoing event) queued, then send another broker event so the driver must wait
    // in event delivery while the consumer remains deliberately undrained.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while connection.try_wait().is_none() {
        assert!(std::time::Instant::now() < deadline, "connection timed out");
        thread::sleep(Duration::from_millis(1));
    }
    publish_tx.send(()).unwrap();
    published_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    thread::sleep(Duration::from_millis(20));

    handle.terminate_for_internal_panic();
    native.join(Duration::from_secs(1)).unwrap();

    let mut events = native.take_events().unwrap();
    let queued = events
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .expect("ordinary event remained queued");
    assert!(!matches!(queued, WrapperEvent::DriverTerminated(_)));
    match events.recv_timeout(Duration::from_secs(1)).unwrap() {
        Some(WrapperEvent::DriverTerminated(error)) => {
            assert_eq!(error.kind(), ErrorKind::Internal);
            assert_eq!(error.code(), ErrorCode::InternalPanic);
        }
        other => panic!("unexpected terminal event: {other:?}"),
    }
    broker.join().unwrap();
}

#[test]
fn boundary_panic_interrupts_full_event_buffer_for_both_protocols() {
    assert_boundary_panic_interrupts_full_event_buffer(ProtocolVersion::V4);
    assert_boundary_panic_interrupts_full_event_buffer(ProtocolVersion::V5);
}
