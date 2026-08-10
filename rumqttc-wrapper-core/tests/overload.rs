use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use rumqttc_wrapper_core::{ClientConfig, ErrorKind, NativeClient, WrapperEvent};

#[test]
fn full_event_buffer_terminates_through_reserved_status_path() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut scratch = [0_u8; 512];
        let _ = stream.read(&mut scratch).unwrap();
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
        thread::sleep(Duration::from_millis(20));
        // QoS 0 PUBLISH, topic "a", payload "x".
        stream
            .write_all(&[0x30, 0x04, 0x00, 0x01, b'a', b'x'])
            .unwrap();
        thread::sleep(Duration::from_millis(300));
    });

    let mut config = ClientConfig::v311("overflow", "127.0.0.1", port);
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = Duration::from_millis(50);
    let mut native = NativeClient::start(config).unwrap();
    let mut events = native.take_events().unwrap();
    thread::sleep(Duration::from_millis(200));

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
    native.join(Duration::from_secs(1)).unwrap();
    broker.join().unwrap();
}
