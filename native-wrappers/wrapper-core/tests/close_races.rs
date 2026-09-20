mod support;

use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::*;
use support::*;

#[test]
fn mqtt5_disconnect_options_on_v4_do_not_admit_or_close() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(false, listener.local_addr().unwrap().port());
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        connect(&mut socket, false);
        let id = publish_id(&mut socket, false);
        puback(&mut socket, id);
        assert_eq!(frame(&mut socket).as_ref(), &[0xe0, 0]);
    });
    let mut client = NativeClient::start(config).unwrap();
    let _events = connected(&mut client);
    for command in [
        Command::ImmediateDisconnectWithOptions {
            protocol: DisconnectProtocolOptions::V5(V5DisconnectOptions::default()),
        },
        Command::GracefulDisconnectWithOptions {
            timeout: Some(DEADLINE),
            protocol: DisconnectProtocolOptions::V5(V5DisconnectOptions::default()),
        },
    ] {
        let error = client.handle().try_admit(command).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
        assert_eq!(client.handle().state(), LifecycleState::Running);
    }
    let operation = publish(&client, b"still running");
    assert_eq!(
        terminal(&operation).unwrap(),
        Completion::Publish(PublishCompletion::Qos1Acknowledged)
    );
    client.closer().close(DEADLINE).unwrap();
    broker.join();
}

#[test]
fn close_callers_keep_independent_deadlines_and_escalation_preserves_payload() {
    for escalate in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = config(true, listener.local_addr().unwrap().port());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, true);
            let id = publish_id(&mut socket, true);
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(DEADLINE).unwrap();
            if !escalate {
                puback(&mut socket, id);
            }
            let mut packet = frame(&mut socket);
            let rumqttc_v5::Packet::Disconnect(packet) =
                rumqttc_v5::Packet::read(&mut packet, None).unwrap()
            else {
                panic!("DISCONNECT")
            };
            assert_eq!(
                packet.properties.unwrap().reason_string.as_deref(),
                Some("selected")
            );
            // DISCONNECT is the final MQTT packet, even with several waiters.
            let _ = socket.flush();
        });
        let mut client = NativeClient::start(config).unwrap();
        let _events = connected(&mut client);
        let pending = publish(&client, b"pending");
        ready_rx.recv_timeout(DEADLINE).unwrap();
        let payload = DisconnectProtocolOptions::V5(V5DisconnectOptions {
            reason_string: Some("selected".into()),
            ..Default::default()
        });
        let closer = client.closer();
        let selected = payload.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let caller = std::thread::spawn(move || {
            done_tx
                .send(closer.close_with_options(DEADLINE, selected))
                .unwrap();
        });
        let deadline = Instant::now() + DEADLINE;
        while client.handle().state() == LifecycleState::Running {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let short = client
            .closer()
            .close_with_options(Duration::from_millis(20), payload.clone())
            .unwrap_err();
        assert_eq!(short.kind(), ErrorKind::Timeout);
        assert_eq!(client.handle().state(), LifecycleState::Closing);
        let conflict = client
            .closer()
            .close_now_with_options(
                DEADLINE,
                DisconnectProtocolOptions::V5(V5DisconnectOptions {
                    reason_string: Some("conflict".into()),
                    ..Default::default()
                }),
            )
            .unwrap_err();
        assert_eq!(conflict.delivery_status(), DeliveryStatus::NotAdmitted);
        release_tx.send(()).unwrap();
        if escalate {
            client
                .closer()
                .close_now_with_options(DEADLINE, payload)
                .unwrap();
        }
        let result = done_rx.recv_timeout(DEADLINE).unwrap();
        caller.join().unwrap();
        if escalate {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::Shutdown);
            assert_eq!(
                terminal(&pending).unwrap_err().delivery_status(),
                DeliveryStatus::Ambiguous
            );
        } else {
            assert_eq!(result.unwrap(), Completion::GracefulShutdown);
            assert_eq!(
                terminal(&pending).unwrap(),
                Completion::Publish(PublishCompletion::Qos1Acknowledged)
            );
        }
        client.join(DEADLINE).unwrap();
        broker.join();
    }
}
