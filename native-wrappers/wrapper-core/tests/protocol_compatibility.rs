mod support;

use std::io::Write;
use std::net::TcpListener;

use rumqttc_wrapper_core::*;
use support::{Broker, DEADLINE, accept, frame, terminal, until};

#[test]
fn accepted_v4_mismatch_preserves_raw_connection_flags_and_reports_effective_fresh_session() {
    assert_eq!(
        V4Config::default().session_present_mismatch_policy,
        SessionPresentMismatchPolicy::Error
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        assert_eq!(frame(&mut socket)[0], 0x10);
        socket.write_all(&[0x20, 2, 1, 0]).unwrap();
        let packet = frame(&mut socket);
        assert_eq!(packet[0], 0x32);
        let topic_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
        let id = u16::from_be_bytes([packet[4 + topic_len], packet[5 + topic_len]]);
        assert_eq!(id, 1);
        support::puback(&mut socket, id);
        assert_eq!(frame(&mut socket)[0], 0xe0);
    });
    let mut config = ClientConfig::v4("compatibility", "127.0.0.1", port);
    let ProtocolConfig::V4(v4) = &mut config.protocol else {
        unreachable!()
    };
    v4.session_present_mismatch_policy = SessionPresentMismatchPolicy::AcceptAsClean;
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    assert!(matches!(
        until(&mut events, |event| matches!(
            event,
            WrapperEvent::Connected { .. }
        )),
        WrapperEvent::Connected {
            session_present: true,
            ..
        }
    ));
    assert!(
        client
            .connection()
            .try_wait()
            .unwrap()
            .unwrap()
            .session_present
    );
    let admission = client.handle().try_admit(Command::Diagnostics).unwrap();
    let Completion::Diagnostics(diagnostics) = terminal(&admission).unwrap() else {
        panic!("expected diagnostics")
    };
    assert!(diagnostics.connected);
    let connack = diagnostics.connack.unwrap();
    assert!(connack.raw_session_present);
    assert!(!connack.session_resumed);
    assert_eq!(
        connack.diagnostic,
        Some(ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean)
    );
    let published = support::publish(&client, b"fresh");
    assert!(matches!(
        terminal(&published).unwrap(),
        Completion::Publish(_)
    ));
    client.closer().close(DEADLINE).unwrap();
    broker.join();
}
