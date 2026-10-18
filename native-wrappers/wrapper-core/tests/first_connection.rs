mod support;

use rumqttc_wrapper_core::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};
use support::*;

fn observed(handle: &ConnectionHandle) -> Result<ConnectionResult> {
    match handle.wait_timeout_outcome(DEADLINE) {
        ConnectionWaitOutcome::Completed(result) => result,
        other => panic!("unexpected observation: {other:?}"),
    }
}

#[test]
fn first_success_survives_retries_loss_shutdown_and_client_destruction_without_consuming_events() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, resume) = mpsc::channel();
        let broker = Broker::spawn(move || {
            for _ in 0..3 {
                let mut socket = accept(&listener);
                assert_eq!(frame(&mut socket)[0], 0x10);
            }
            let mut socket = accept(&listener);
            assert_eq!(frame(&mut socket)[0], 0x10);
            resume.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                })
                .unwrap();
            // Drop the accepted connection before any host event consumption.
        });
        let mut config = config(mqtt5, port);
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::ZERO,
            maximum_delay: Duration::ZERO,
            jitter: ReconnectJitter::None,
            budget: RetryBudget::Limited(4),
            ..ReconnectConfig::default()
        });
        let mut client = start(config).unwrap();
        let observer = client.connection();
        assert!(matches!(
            observer.wait_timeout_outcome(Duration::from_millis(1)),
            ConnectionWaitOutcome::DeadlineElapsed
        ));
        release.send(()).unwrap();
        let result = observed(&observer).unwrap();
        assert_eq!(
            result.protocol,
            if mqtt5 {
                ProtocolVersion::V5
            } else {
                ProtocolVersion::V4
            }
        );
        assert!(!result.session_present);
        broker.join();
        let mut events = client.take_events().unwrap();
        for _ in 0..3 {
            assert!(matches!(
                events.recv_timeout(DEADLINE).unwrap(),
                Some(WrapperEvent::Disconnected {
                    phase: ConnectionPhase::Attempt,
                    ..
                })
            ));
        }
        assert!(matches!(
            events.recv_timeout(DEADLINE).unwrap(),
            Some(WrapperEvent::Connected { .. })
        ));
        assert!(matches!(
            events.recv_timeout(DEADLINE).unwrap(),
            Some(WrapperEvent::Disconnected {
                phase: ConnectionPhase::Established,
                ..
            })
        ));
        client.closer().close_now(DEADLINE).unwrap();
        assert_eq!(observed(&client.connection()).unwrap(), result);
        drop(client);
        assert_eq!(observed(&observer).unwrap(), result);
    }
}

#[test]
fn event_overflow_before_success_fails_observer_and_after_acceptance_preserves_success() {
    for mqtt5 in [false, true] {
        for accept_connection in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = Broker::spawn(move || {
                // The first failed attempt fills the sole event slot.
                let mut first = accept(&listener);
                assert_eq!(frame(&mut first)[0], 0x10);
                drop(first);
                let mut socket = accept(&listener);
                assert_eq!(frame(&mut socket)[0], 0x10);
                if accept_connection {
                    socket
                        .write_all(if mqtt5 {
                            &[0x20, 3, 0, 0, 0]
                        } else {
                            &[0x20, 2, 0, 0]
                        })
                        .unwrap();
                    // Wait for overflow teardown, not socket closure by the fixture.
                    let mut byte = [0];
                    let _ = socket.read(&mut byte);
                }
            });
            let mut config = config(mqtt5, port);
            config.common.event_buffer_capacity = 1;
            config.common.event_delivery_timeout = Duration::from_millis(30);
            config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
                initial_delay: Duration::ZERO,
                maximum_delay: Duration::ZERO,
                jitter: ReconnectJitter::None,
                ..ReconnectConfig::default()
            });
            let mut client = start(config).unwrap();
            let observer = client.connection();
            let result = observed(&observer);
            client.join(DEADLINE).unwrap();
            if accept_connection {
                let result = result.unwrap();
                assert_eq!(observed(&observer).unwrap(), result);
            } else {
                assert_eq!(result.unwrap_err().code(), ErrorCode::EventBufferOverflow);
            }
            // No Connected event reached the full queue, but terminal status uses its own channel.
            let mut events = client.take_events().unwrap();
            assert!(matches!(
                events.recv_timeout(DEADLINE).unwrap(),
                Some(WrapperEvent::Disconnected { .. })
            ));
            assert!(
                matches!(events.recv_timeout(DEADLINE).unwrap(), Some(WrapperEvent::DriverTerminated(error)) if error.code() == ErrorCode::EventBufferOverflow)
            );
            broker.join();
        }
    }
}

#[test]
fn handshake_redirect_remains_pending_until_the_target_accepts() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 2,
        transport: TransportConfig::Tcp,
    };
    let (arrived, arrival) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        assert_eq!(frame(&mut socket)[0], 0x10);
        let mut properties = vec![0x1c];
        properties.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
        properties.extend_from_slice(reference.as_bytes());
        let mut packet = vec![
            0x20,
            u8::try_from(properties.len() + 3).unwrap(),
            0,
            0x9c,
            u8::try_from(properties.len()).unwrap(),
        ];
        packet.extend(properties);
        socket.write_all(&packet).unwrap();
        let mut socket = accept(&target);
        assert_eq!(frame(&mut socket)[0], 0x10);
        arrived.send(()).unwrap();
        resume.recv_timeout(DEADLINE).unwrap();
        socket
            .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
            .unwrap();
        let mut byte = [0];
        let _ = socket.read(&mut byte);
    });
    let client = start(config).unwrap();
    let observer = client.connection();
    arrival.recv_timeout(DEADLINE).unwrap();
    assert!(matches!(
        observer.wait_timeout_outcome(Duration::ZERO),
        ConnectionWaitOutcome::DeadlineElapsed
    ));
    release.send(()).unwrap();
    assert_eq!(observed(&observer).unwrap().protocol, ProtocolVersion::V5);
    client.closer().close_now(DEADLINE).unwrap();
    broker.join();
}

#[test]
fn terminal_connack_refusal_is_repeatable_after_client_destruction() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            assert_eq!(frame(&mut socket)[0], 0x10);
            socket
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0x86, 0]
                } else {
                    &[0x20, 2, 0, 4]
                })
                .unwrap();
        });
        let mut config = config(mqtt5, port);
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig::default());
        let client = start(config).unwrap();
        let observer = client.connection();
        let error = observed(&observer).unwrap_err();
        assert_eq!(error.broker_reason(), Some(if mqtt5 { 0x86 } else { 4 }));
        client.join(DEADLINE).unwrap();
        let late = client.connection();
        drop(client);
        assert_eq!(observed(&late).unwrap_err().code(), error.code());
        assert_eq!(
            observed(&observer).unwrap_err().broker_reason(),
            error.broker_reason()
        );
        broker.join();
    }
}
