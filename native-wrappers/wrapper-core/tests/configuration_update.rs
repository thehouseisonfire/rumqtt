#[cfg(feature = "use-rustls-no-provider")]
#[path = "support/custom_transport.rs"]
mod custom;
mod support;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
#[path = "support/tls.rs"]
mod tls;

use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use rumqttc_wrapper_core::*;
use support::*;

fn credentials(password: &[u8]) -> FieldUpdate<BrokerCredentials> {
    FieldUpdate::Replace(BrokerCredentials {
        username: Some("user".into()),
        password: Some(SecretBytes::new(password.to_vec())),
    })
}

fn stage(handle: &ClientHandle, update: RuntimeConfigUpdate) -> ConfigurationUpdateReceipt {
    let admission = handle.try_configuration_update(update).unwrap();
    let Completion::ConfigurationStaged(receipt) =
        admission.completion.wait_timeout(DEADLINE).unwrap()
    else {
        panic!("expected configuration staging");
    };
    receipt
}

fn wait_snapshot(
    handle: &ClientHandle,
    predicate: impl Fn(&ConfigurationSnapshot) -> bool,
) -> ConfigurationSnapshot {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = handle.configuration_snapshot().unwrap();
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "configuration did not advance: {snapshot:?}"
        );
        thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn assert_password(packet: &mut bytes::BytesMut, mqtt5: bool, expected: &[u8]) {
    let password = if mqtt5 {
        let rumqttc_v5::Packet::Connect(
            _,
            _,
            rumqttc_v5::ConnectAuth::UsernamePassword { username, password },
        ) = rumqttc_v5::Packet::read(packet, None).unwrap()
        else {
            panic!("expected username/password CONNECT");
        };
        assert_eq!(username, "user");
        password
    } else {
        let rumqttc_v4::Packet::Connect(connect) =
            rumqttc_v4::Packet::read(packet, 1024 * 1024).unwrap()
        else {
            panic!("expected CONNECT");
        };
        let rumqttc_v4::ConnectAuth::UsernamePassword { username, password } = connect.auth else {
            panic!("expected credentials");
        };
        assert_eq!(username, "user");
        password
    };
    assert_eq!(&password[..], expected);
}

fn connack(socket: &mut impl Write, mqtt5: bool) {
    socket
        .write_all(if mqtt5 {
            &[0x20, 3, 0, 0, 0]
        } else {
            &[0x20, 2, 0, 0]
        })
        .unwrap();
    socket.flush().unwrap();
}

#[test]
fn idle_poll_stages_and_supersedes_tuning_without_cancelling_protocol_work() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (wake, wake_rx) = mpsc::channel();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.keep_alive = std::time::Duration::ZERO;
        let broker = thread::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            wake_rx.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    &[0x30, 5, 0, 1, b't', 0, b'a']
                } else {
                    &[0x30, 4, 0, 1, b't', b'a']
                })
                .unwrap();
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let first = stage(
            &handle,
            RuntimeConfigUpdate {
                max_request_batch: FieldUpdate::Replace(17),
                ..Default::default()
            },
        );
        let second = stage(
            &handle,
            RuntimeConfigUpdate {
                read_batch_size: FieldUpdate::Replace(500),
                pending_throttle: FieldUpdate::Replace(std::time::Duration::from_nanos(313)),
                ..Default::default()
            },
        );
        assert_eq!(first.activation().0, ActivationState::Superseded);
        assert_eq!(second.activation().0, ActivationState::Staged);
        let pending = handle.configuration_snapshot().unwrap();
        assert_eq!(pending.revision, 2);
        assert_eq!(pending.effective_tuning_revision, 0);
        assert_eq!(pending.desired_tuning.max_request_batch, 17);
        let invalid = handle
            .try_configuration_update(RuntimeConfigUpdate {
                max_request_batch: FieldUpdate::Replace(99),
                broker_tls: FieldUpdate::Clear,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            invalid
                .completion
                .wait_timeout(DEADLINE)
                .unwrap_err()
                .code(),
            ErrorCode::ConfigurationUpdateUnsupported
        );
        assert_eq!(handle.configuration_snapshot().unwrap().revision, 2);
        wake.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::IncomingPublish(_))
        });
        let applied = wait_snapshot(&handle, |snapshot| snapshot.effective_tuning_revision == 2);
        assert_eq!(applied.effective_tuning.max_request_batch, 17);
        assert_eq!(applied.effective_read_batch_size, 128);
        assert_eq!(second.activation().0, ActivationState::Activated);
        client.closer().close_now(DEADLINE).unwrap();
        assert!(handle.configuration_snapshot().unwrap().closed);
        assert_eq!(second.activation().0, ActivationState::Activated);
        broker.join().unwrap();
    }
}

#[test]
fn current_attempt_keeps_its_credentials_and_next_attempt_gets_the_coherent_profile() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local_reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let next_local_address = local_reservation.local_addr().unwrap();
        let (attempt, attempt_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.username = Some("user".into());
        config.common.password = Some(b"old".as_slice().into());
        let broker = thread::spawn(move || {
            let mut first = accept(&listener);
            assert_password(&mut frame(&mut first), mqtt5, b"old");
            attempt.send(()).unwrap();
            release_rx.recv_timeout(DEADLINE).unwrap();
            connack(&mut first, mqtt5);
            drop(local_reservation);
            drop(first);
            let mut second = accept(&listener);
            assert_password(&mut frame(&mut second), mqtt5, b"\x00\xffnew");
            assert_eq!(second.peer_addr().unwrap(), next_local_address);
            connack(&mut second, mqtt5);
            assert_eq!(frame(&mut second)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let handle = client.handle();
        attempt_rx.recv_timeout(DEADLINE).unwrap();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"\x00\xffnew"),
                network: FieldUpdate::Replace(NetworkConfig {
                    local_address: Some(next_local_address),
                    tcp_nodelay: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let current = handle.configuration_snapshot().unwrap();
        assert_eq!(current.connection.attempt_revision, Some(0));
        assert_eq!(current.connection.outcome, AttemptOutcome::Pending);
        assert_eq!(receipt.activation().1, ActivationState::Staged);
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        let snapshot = wait_snapshot(&handle, |snapshot| {
            snapshot.connection.successful_revision == Some(1)
        });
        assert_eq!(snapshot.connection.attempt_revision, Some(1));
        assert_eq!(snapshot.effective_connection_revision, 1);
        assert!(snapshot.effective_connection.network.tcp_nodelay);
        assert_eq!(receipt.activation().1, ActivationState::Activated);
        client.closer().close_now(DEADLINE).unwrap();
        broker.join().unwrap();
    }
}

#[test]
fn closing_an_idle_client_finalizes_staged_receipts_and_rejects_new_updates() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.keep_alive = std::time::Duration::ZERO;
        let broker = thread::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let _events = connected(&mut client);
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                max_request_batch: FieldUpdate::Replace(4),
                ..Default::default()
            },
        );
        client.closer().close_now(DEADLINE).unwrap();
        assert_eq!(
            receipt.activation().1,
            ActivationState::ClosedBeforeActivation
        );
        assert!(
            handle
                .try_configuration_update(RuntimeConfigUpdate {
                    read_batch_size: FieldUpdate::Clear,
                    ..Default::default()
                })
                .is_err()
        );
        drop(client);
        assert_eq!(
            receipt.activation().1,
            ActivationState::ClosedBeforeActivation
        );
        broker.join().unwrap();
    }
}

#[test]
fn configuration_stages_during_classified_backoff_and_close_finalizes_the_receipt() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: std::time::Duration::from_secs(30),
            maximum_delay: std::time::Duration::from_secs(30),
            jitter: ReconnectJitter::None,
            budget: RetryBudget::Limited(1),
            ..Default::default()
        });
        let broker = thread::spawn(move || {
            let mut socket = accept(&listener);
            frame(&mut socket);
            // Dropping the stream produces a retryable failure on the initial attempt.
        });
        let mut client = start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let WrapperEvent::Disconnected { error, .. } = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        }) else {
            unreachable!()
        };
        assert!(error.retryable());
        assert_eq!(error.configuration_revision(), Some(0));
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                ..Default::default()
            },
        );
        assert_eq!(receipt.activation().1, ActivationState::Staged);
        assert_eq!(
            handle.reconnect_diagnostics().phase,
            ReconnectPhase::Waiting
        );
        assert_eq!(handle.reconnect_diagnostics().cycles_started, 1);
        client.closer().close_now(DEADLINE).unwrap();
        assert_eq!(
            receipt.activation().1,
            ActivationState::ClosedBeforeActivation
        );
        broker.join().unwrap();
    }
}

#[test]
fn failed_rotated_attempts_retain_the_profile_and_failure_revision_across_retries() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.username = Some("user".into());
        config.common.password = Some(b"old".as_slice().into());
        config.common.keep_alive = std::time::Duration::ZERO;
        let (release, release_rx) = mpsc::channel();
        let (retry, retry_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut initial = accept(&listener);
            assert_password(&mut frame(&mut initial), mqtt5, b"old");
            connack(&mut initial, mqtt5);
            release_rx.recv_timeout(DEADLINE).unwrap();
            drop(initial);
            let mut rejected = accept(&listener);
            assert_password(&mut frame(&mut rejected), mqtt5, b"new");
            rejected
                .write_all(if mqtt5 {
                    b"\x20\x03\x00\x86\x00"
                } else {
                    b"\x20\x02\x00\x04"
                })
                .unwrap();
            drop(rejected);
            let mut subsequent = accept(&listener);
            assert_password(&mut frame(&mut subsequent), mqtt5, b"new");
            retry_rx.recv_timeout(DEADLINE).unwrap();
            connack(&mut subsequent, mqtt5);
            assert_eq!(frame(&mut subsequent)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                ..Default::default()
            },
        );
        release.send(()).unwrap();
        let WrapperEvent::Disconnected { error, .. } = until(
            &mut events,
            |event| matches!(event, WrapperEvent::Disconnected { error, .. } if error.kind() == ErrorKind::Authentication),
        ) else {
            unreachable!()
        };
        assert_eq!(error.configuration_revision(), Some(1));
        assert_eq!(
            handle
                .configuration_snapshot()
                .unwrap()
                .connection
                .successful_revision,
            Some(0)
        );
        assert_eq!(receipt.activation().1, ActivationState::Activated);
        retry.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        wait_snapshot(&handle, |snapshot| {
            snapshot.connection.successful_revision == Some(1)
        });
        // An older event's error retains its owning revision after the retry completes.
        assert_eq!(error.configuration_revision(), Some(1));
        client.closer().close_now(DEADLINE).unwrap();
        broker.join().unwrap();
    }
}

#[test]
fn concurrent_updates_are_serialized_and_do_not_starve_a_shared_execution_peer() {
    let execution = ExecutionContext::new(ExecutionOptions {
        client_capacity: 2,
        worker_threads: 1,
        max_blocking_threads: 1,
    })
    .unwrap();
    let mut clients = Vec::new();
    let mut brokers = Vec::new();
    let mut event_owners = Vec::new();
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.keep_alive = std::time::Duration::ZERO;
        brokers.push(thread::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            loop {
                match frame(&mut socket)[0] >> 4 {
                    3 => {}
                    14 => break,
                    kind => panic!("unexpected packet {kind}"),
                }
            }
        }));
        let mut client = NativeClient::start_in(config, &execution).unwrap();
        event_owners.push(connected(&mut client));
        clients.push(client);
    }
    let producers = (0..4)
        .map(|producer| {
            let handle = clients[0].handle();
            thread::spawn(move || {
                (0..32)
                    .filter_map(|index| {
                        match handle.try_configuration_update(RuntimeConfigUpdate {
                            read_batch_size: FieldUpdate::Replace(producer * 32 + index + 1),
                            ..Default::default()
                        }) {
                            Ok(admission) => Some(admission),
                            Err(error) => {
                                assert_eq!(error.kind(), ErrorKind::Backpressure);
                                None
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    clients[1]
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "peer/progress".into(),
            qos: QoS::AtMostOnce,
            payload: b"live".as_slice().into(),
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap()
        .completion
        .wait_timeout(DEADLINE)
        .unwrap();
    let mut revisions = Vec::new();
    for producer in producers {
        for admission in producer.join().unwrap() {
            let retained = admission.completion.clone();
            drop(admission.completion);
            let Completion::ConfigurationStaged(receipt) = retained.wait_timeout(DEADLINE).unwrap()
            else {
                unreachable!()
            };
            revisions.push(receipt.revision);
        }
    }
    revisions.sort_unstable();
    assert!(!revisions.is_empty());
    assert_eq!(
        revisions,
        (1..=u64::try_from(revisions.len()).unwrap()).collect::<Vec<_>>()
    );
    assert_eq!(
        clients[0]
            .handle()
            .configuration_snapshot()
            .unwrap()
            .revision,
        *revisions.last().unwrap()
    );
    execution.request_shutdown();
    execution.join(DEADLINE).unwrap();
    for broker in brokers {
        broker.join().unwrap();
    }
    drop(event_owners);
}

#[test]
fn shutdown_racing_updates_reconciles_every_admitted_completion_and_receipt() {
    use std::sync::{Arc, Barrier};

    for mqtt5 in [false, true] {
        for _ in 0..4 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = config(mqtt5, listener.local_addr().unwrap().port());
            config.common.keep_alive = std::time::Duration::ZERO;
            let broker = thread::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                assert_eq!(frame(&mut socket)[0] >> 4, 14);
            });
            let mut client = start(config).unwrap();
            let _events = connected(&mut client);
            let handle = client.handle();
            let barrier = Arc::new(Barrier::new(2));
            let producer = {
                let handle = handle.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    (0..32)
                        .filter_map(|_| {
                            match handle.try_configuration_update(RuntimeConfigUpdate {
                                credentials: credentials(b"new"),
                                max_request_batch: FieldUpdate::Replace(3),
                                ..Default::default()
                            }) {
                                Ok(admission) => Some(admission),
                                Err(error) => {
                                    assert!(matches!(
                                        error.kind(),
                                        ErrorKind::Shutdown | ErrorKind::Backpressure
                                    ));
                                    None
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                })
            };
            barrier.wait();
            client.closer().close_now(DEADLINE).unwrap();
            for admission in producer.join().unwrap() {
                match admission.completion.wait_timeout(DEADLINE) {
                    Ok(Completion::ConfigurationStaged(receipt)) => {
                        assert_ne!(receipt.activation().0, ActivationState::Staged);
                        assert_ne!(receipt.activation().1, ActivationState::Staged);
                    }
                    Err(error) => assert_eq!(error.kind(), ErrorKind::Shutdown),
                    Ok(completion) => panic!("unexpected completion {completion:?}"),
                }
            }
            let closed = handle.configuration_snapshot().unwrap();
            assert!(closed.closed);
            assert_eq!(
                handle.configuration_snapshot().unwrap().revision,
                closed.revision
            );
            broker.join().unwrap();
        }
    }
}

#[test]
fn event_backpressure_closes_unactivated_receipts_without_losing_staged_completions() {
    use std::io::Read;
    use std::time::Duration;

    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.keep_alive = Duration::ZERO;
        config.common.event_buffer_capacity = 1;
        config.common.event_delivery_timeout = Duration::from_millis(50);
        let (release, release_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            release_rx.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    b"\x30\x05\x00\x01t\x00a"
                } else {
                    b"\x30\x04\x00\x01ta"
                })
                .unwrap();
            let mut scratch = [0; 32];
            while socket.read(&mut scratch).is_ok_and(|length| length != 0) {}
        });
        let mut client = start(config).unwrap();
        let handle = client.handle();
        wait_snapshot(&handle, |snapshot| {
            snapshot.connection.outcome == AttemptOutcome::Succeeded
        });
        // Connected occupies the only event slot. Stage while the native poll waits for data.
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                max_request_batch: FieldUpdate::Replace(5),
                credentials: credentials(b"new"),
                ..Default::default()
            },
        );
        assert_eq!(
            receipt.activation(),
            (ActivationState::Staged, ActivationState::Staged)
        );
        release.send(()).unwrap();
        client.join(DEADLINE).unwrap();
        assert_eq!(
            receipt.activation(),
            (
                ActivationState::ClosedBeforeActivation,
                ActivationState::ClosedBeforeActivation
            )
        );
        let mut events = client.take_events().unwrap();
        assert!(matches!(
            events.try_recv().unwrap(),
            Some(WrapperEvent::Connected { .. })
        ));
        assert!(
            matches!(events.try_recv().unwrap(), Some(WrapperEvent::DriverTerminated(error)) if error.kind() == ErrorKind::Backpressure)
        );
        broker.join().unwrap();
    }
}

fn redirect(socket: &mut impl Write, reference: &str, permanent: bool) {
    let mut body = vec![if permanent { 0x9d } else { 0x9c }];
    let property_len = 3 + reference.len();
    body.push(u8::try_from(property_len).unwrap());
    body.push(0x1c);
    body.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
    body.extend_from_slice(reference.as_bytes());
    socket
        .write_all(&[0xe0, u8::try_from(body.len()).unwrap()])
        .unwrap();
    socket.write_all(&body).unwrap();
}

#[test]
fn origin_rotation_survives_temporary_redirect_and_never_reaches_the_isolated_target() {
    for permanent in [false, true] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let reference = target.local_addr().unwrap().to_string();
        let mut config = config(true, origin.local_addr().unwrap().port());
        config.common.keep_alive = std::time::Duration::ZERO;
        config.common.username = Some("user".into());
        config.common.password = Some(b"old".as_slice().into());
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 3,
            transport: TransportConfig::Tcp,
        };
        let (release, release_rx) = mpsc::channel();
        let (target_close, target_close_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut first = accept(&origin);
            assert_password(&mut frame(&mut first), true, b"old");
            connack(&mut first, true);
            release_rx.recv_timeout(DEADLINE).unwrap();
            redirect(&mut first, &reference, permanent);
            let mut redirected = accept(&target);
            let rumqttc_v5::Packet::Connect(_, _, auth) =
                rumqttc_v5::Packet::read(&mut frame(&mut redirected), None).unwrap()
            else {
                panic!("expected CONNECT");
            };
            assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
            redirected
                .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
                .unwrap();
            target_close_rx.recv_timeout(DEADLINE).unwrap();
            if permanent {
                assert_eq!(frame(&mut redirected)[0] >> 4, 14);
            } else {
                drop(redirected);
                let mut restored = accept(&origin);
                assert_password(&mut frame(&mut restored), true, b"new");
                connack(&mut restored, true);
                assert_eq!(frame(&mut restored)[0] >> 4, 14);
            }
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                ..Default::default()
            },
        );
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        let route = if permanent {
            ConnectionRoute::PermanentTarget
        } else {
            ConnectionRoute::TemporaryTarget
        };
        wait_snapshot(&handle, |snapshot| snapshot.connection.route == route);
        let rejected = handle
            .try_configuration_update(RuntimeConfigUpdate {
                credentials: credentials(b"wrong-target"),
                read_batch_size: FieldUpdate::Replace(99),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            rejected
                .completion
                .wait_timeout(DEADLINE)
                .unwrap_err()
                .code(),
            ErrorCode::ConfigurationUpdateUnsupported
        );
        assert_eq!(handle.configuration_snapshot().unwrap().revision, 1);
        target_close.send(()).unwrap();
        if permanent {
            wait_snapshot(&handle, |_| {
                receipt.activation().1 == ActivationState::UnavailableAfterRedirect
            });
        } else {
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            wait_snapshot(&handle, |snapshot| {
                snapshot.connection.successful_revision == Some(1)
            });
            assert_eq!(receipt.activation().1, ActivationState::Activated);
        }
        client.closer().close_now(DEADLINE).unwrap();
        broker.join().unwrap();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep handshake barriers and receipt assertions together for all three target outcomes"
)]
fn permanent_redirect_retires_origin_rotation_only_after_target_connack() {
    for outcome in [
        AttemptOutcome::Succeeded,
        AttemptOutcome::Failed,
        AttemptOutcome::Cancelled,
    ] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let reference = target.local_addr().unwrap().to_string();
        let mut config = config(true, origin.local_addr().unwrap().port());
        config.common.keep_alive = std::time::Duration::ZERO;
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 3,
            transport: TransportConfig::Tcp,
        };
        let (redirect_tx, redirect_rx) = mpsc::channel();
        let (target_ready_tx, target_ready_rx) = mpsc::channel();
        let (complete_tx, complete_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut first = accept(&origin);
            connect(&mut first, true);
            redirect_rx.recv_timeout(DEADLINE).unwrap();
            redirect(&mut first, &reference, true);
            let mut redirected = accept(&target);
            let rumqttc_v5::Packet::Connect(_, _, auth) =
                rumqttc_v5::Packet::read(&mut frame(&mut redirected), None).unwrap()
            else {
                panic!("expected CONNECT");
            };
            assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
            target_ready_tx.send(()).unwrap();
            complete_rx.recv_timeout(DEADLINE).unwrap();
            match outcome {
                AttemptOutcome::Succeeded => {
                    redirected
                        .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
                        .unwrap();
                    assert_eq!(frame(&mut redirected)[0] >> 4, 14);
                }
                AttemptOutcome::Failed => redirected.write_all(b"\x20\x03\x00\x87\x00").unwrap(),
                AttemptOutcome::Cancelled => {}
                _ => unreachable!(),
            }
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                ..Default::default()
            },
        );
        redirect_tx.send(()).unwrap();
        target_ready_rx.recv_timeout(DEADLINE).unwrap();
        let pending = handle.configuration_snapshot().unwrap();
        assert_eq!(pending.connection.route, ConnectionRoute::PermanentTarget);
        assert_eq!(
            pending.connection.attempt_route,
            ConnectionRoute::PermanentTarget
        );
        assert_eq!(pending.connection.outcome, AttemptOutcome::Pending);
        assert_eq!(pending.connection.successful_revision, Some(0));
        assert_eq!(receipt.activation().1, ActivationState::Staged);
        if outcome == AttemptOutcome::Cancelled {
            client.closer().close_now(DEADLINE).unwrap();
            complete_tx.send(()).unwrap();
        } else {
            complete_tx.send(()).unwrap();
            if outcome == AttemptOutcome::Succeeded {
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                wait_snapshot(&handle, |_| {
                    receipt.activation().1 == ActivationState::UnavailableAfterRedirect
                });
                client.closer().close_now(DEADLINE).unwrap();
            } else {
                let event = until(&mut events, |event| {
                    matches!(event, WrapperEvent::DriverTerminated(_))
                });
                assert!(
                    matches!(event, WrapperEvent::DriverTerminated(error) if error.redirect_failure().is_some())
                );
                client.join(DEADLINE).unwrap();
            }
        }
        let closed = handle.configuration_snapshot().unwrap();
        assert!(closed.closed);
        assert_eq!(
            closed.connection.attempt_route,
            ConnectionRoute::PermanentTarget
        );
        assert_eq!(closed.connection.outcome, outcome);
        assert_eq!(
            receipt.activation().1,
            if outcome == AttemptOutcome::Succeeded {
                ActivationState::UnavailableAfterRedirect
            } else {
                ActivationState::ClosedBeforeActivation
            }
        );
        broker.join().unwrap();
    }
}

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
#[test]
fn tls_preparation_failure_rolls_back_and_trust_rotation_selects_a_fresh_connection_profile() {
    let first_tls = tls::Fixture::new();
    let second_tls = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for backend in [TlsBackend::Rustls, TlsBackend::Native] {
            if backend.capabilities().version_policies == 0 {
                continue;
            }
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = config(mqtt5, listener.local_addr().unwrap().port());
            config.common.transport = TransportConfig::Tls(first_tls.client(backend));
            config.common.username = Some("user".into());
            config.common.password = Some(b"old".as_slice().into());
            config.common.keep_alive = std::time::Duration::ZERO;
            let first_server = first_tls.server.clone();
            let second_server = second_tls.server.clone();
            let (release, release_rx) = mpsc::channel();
            let broker = thread::spawn(move || {
                let mut first = tls::wrap(Box::new(accept(&listener)), first_server);
                assert_password(&mut frame(&mut first), mqtt5, b"old");
                connack(&mut first, mqtt5);
                release_rx.recv_timeout(DEADLINE).unwrap();
                drop(first);
                let mut second = tls::wrap(Box::new(accept(&listener)), second_server);
                assert_password(&mut frame(&mut second), mqtt5, b"new");
                connack(&mut second, mqtt5);
                assert_eq!(frame(&mut second)[0] >> 4, 14);
            });
            let mut client = start(config).unwrap();
            let mut events = connected(&mut client);
            let handle = client.handle();
            let invalid = handle
                .try_configuration_update(RuntimeConfigUpdate {
                    credentials: credentials(b"must-not-commit"),
                    max_request_batch: FieldUpdate::Replace(44),
                    broker_tls: FieldUpdate::Replace(TlsConfig {
                        backend,
                        roots: TlsRootPolicy::Pem(b"invalid PEM".as_slice().into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .unwrap();
            assert!(invalid.completion.wait_timeout(DEADLINE).is_err());
            assert_eq!(handle.configuration_snapshot().unwrap().revision, 0);
            let rotated = second_tls.client(backend);
            #[cfg(feature = "use-rustls-no-provider")]
            let rotated = {
                let mut rotated = rotated;
                if backend == TlsBackend::Rustls {
                    use rustls::pki_types::pem::PemObject;
                    use sha2::Digest;
                    let certificate = rustls::pki_types::CertificateDer::from_pem_slice(
                        second_tls.pem.as_bytes(),
                    )
                    .unwrap();
                    rotated.pins.push(TlsPin {
                        target: TlsPinTarget::LeafCertificate,
                        sha256: sha2::Sha256::digest(certificate).into(),
                    });
                }
                rotated
            };
            let receipt = stage(
                &handle,
                RuntimeConfigUpdate {
                    credentials: credentials(b"new"),
                    broker_tls: FieldUpdate::Replace(rotated),
                    ..Default::default()
                },
            );
            release.send(()).unwrap();
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            wait_snapshot(&handle, |snapshot| {
                snapshot.connection.successful_revision == Some(1)
            });
            assert_eq!(receipt.activation().1, ActivationState::Activated);
            client.closer().close_now(DEADLINE).unwrap();
            broker.join().unwrap();
        }
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep target handshake, blocked event delivery and owner lifetime assertions in one lifecycle"
)]
fn permanent_redirect_retires_origin_before_connected_event_backpressure() {
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    struct Policy;
    impl TlsVerifier for Policy {
        fn verify(
            &self,
            _: &TlsVerificationRequest<'_>,
        ) -> std::result::Result<(), TlsCallbackReason> {
            Ok(())
        }
    }
    let fixture = tls::Fixture::new();
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("mqtt://{}", target.local_addr().unwrap());
    let mut config = config(true, origin.local_addr().unwrap().port());
    config.common.transport = TransportConfig::Tls(fixture.client(TlsBackend::Rustls));
    config.common.keep_alive = Duration::ZERO;
    config.common.event_buffer_capacity = 1;
    config.common.event_delivery_timeout = Duration::from_secs(1);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 3,
        transport: TransportConfig::Tcp,
    };
    let server = fixture.server.clone();
    let (move_target, move_target_rx) = mpsc::channel();
    let (ready, ready_rx) = mpsc::channel();
    let (finish, finish_rx) = mpsc::channel();
    let broker = thread::spawn(move || {
        let mut first = tls::wrap(Box::new(accept(&origin)), server);
        assert_eq!(frame(&mut first)[0], 0x10);
        connack(&mut first, true);
        move_target_rx.recv_timeout(DEADLINE).unwrap();
        redirect(&mut first, &reference, true);
        let mut redirected = accept(&target);
        let rumqttc_v5::Packet::Connect(_, _, auth) =
            rumqttc_v5::Packet::read(&mut frame(&mut redirected), None).unwrap()
        else {
            panic!("expected target CONNECT");
        };
        assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
        ready.send(()).unwrap();
        finish_rx.recv_timeout(DEADLINE).unwrap();
        redirected
            .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
            .unwrap();
        let mut scratch = [0; 32];
        while redirected
            .read(&mut scratch)
            .is_ok_and(|length| length != 0)
        {}
    });
    let mut client = start(config).unwrap();
    let mut events = connected(&mut client);
    let handle = client.handle();
    let owner = Arc::new(Policy);
    let retired = Arc::downgrade(&owner);
    let receipt = stage(
        &handle,
        RuntimeConfigUpdate {
            credentials: credentials(b"origin-only"),
            broker_tls: FieldUpdate::Replace(TlsConfig {
                verifier: Some(TlsVerifierConfig(owner)),
                ..fixture.client(TlsBackend::Rustls)
            }),
            ..Default::default()
        },
    );
    move_target.send(()).unwrap();
    ready_rx.recv_timeout(DEADLINE).unwrap();
    let pending = handle.configuration_snapshot().unwrap();
    assert_eq!(
        pending.connection.attempt_route,
        ConnectionRoute::PermanentTarget
    );
    assert_eq!(pending.connection.outcome, AttemptOutcome::Pending);
    assert_eq!(receipt.activation().1, ActivationState::Staged);
    assert!(retired.upgrade().is_some());
    // Redirect occupies the only slot; do not consume it until delivery has timed out.
    finish.send(()).unwrap();
    let retired_snapshot = wait_snapshot(&handle, |_| {
        receipt.activation().1 != ActivationState::Staged
    });
    assert_eq!(
        receipt.activation().1,
        ActivationState::UnavailableAfterRedirect
    );
    assert!(!retired_snapshot.closed);
    assert!(retired.upgrade().is_none());
    client.join(DEADLINE).unwrap();
    let closed = handle.configuration_snapshot().unwrap();
    assert!(closed.closed);
    assert_eq!(closed.connection.route, ConnectionRoute::PermanentTarget);
    assert_eq!(
        closed.connection.attempt_route,
        ConnectionRoute::PermanentTarget
    );
    assert_eq!(closed.connection.outcome, AttemptOutcome::Succeeded);
    assert_eq!(
        receipt.activation().1,
        ActivationState::UnavailableAfterRedirect
    );
    assert!(matches!(
        events.recv_timeout(DEADLINE).unwrap(),
        Some(WrapperEvent::Redirect(_))
    ));
    assert!(matches!(
        events.recv_timeout(DEADLINE).unwrap(),
        Some(WrapperEvent::DriverTerminated(error)) if error.code() == ErrorCode::EventBufferOverflow
    ));
    broker.join().unwrap();
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep origin-owner retirement, snapshot and target tuning assertions in one connection lifecycle"
)]
fn permanent_redirect_releases_origin_owners_and_preserves_tuning_and_summaries() {
    use std::sync::Arc;

    struct Policy;
    impl TlsVerifier for Policy {
        fn verify(
            &self,
            _: &TlsVerificationRequest<'_>,
        ) -> std::result::Result<(), TlsCallbackReason> {
            Ok(())
        }
    }
    let fixture = tls::Fixture::new();
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("mqtt://{}", target.local_addr().unwrap());
    let mut config = config(true, origin.local_addr().unwrap().port());
    config.common.transport = TransportConfig::Tls(fixture.client(TlsBackend::Rustls));
    config.common.keep_alive = std::time::Duration::ZERO;
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 3,
        transport: TransportConfig::Tcp,
    };
    let server = fixture.server.clone();
    let (move_target, move_target_rx) = mpsc::channel();
    let (wake, wake_rx) = mpsc::channel();
    let broker = thread::spawn(move || {
        let mut first = tls::wrap(Box::new(accept(&origin)), server);
        assert_eq!(frame(&mut first)[0], 0x10);
        connack(&mut first, true);
        move_target_rx.recv_timeout(DEADLINE).unwrap();
        redirect(&mut first, &reference, true);
        let mut redirected = accept(&target);
        assert_eq!(frame(&mut redirected)[0], 0x10);
        redirected
            .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
            .unwrap();
        wake_rx.recv_timeout(DEADLINE).unwrap();
        redirected
            .write_all(&[0x30, 5, 0, 1, b't', 0, b'a'])
            .unwrap();
        assert_eq!(frame(&mut redirected)[0] >> 4, 14);
    });
    let mut client = start(config).unwrap();
    let mut events = connected(&mut client);
    let handle = client.handle();
    let owner = Arc::new(Policy);
    let retired = Arc::downgrade(&owner);
    let receipt = stage(
        &handle,
        RuntimeConfigUpdate {
            credentials: credentials(b"origin-only"),
            broker_tls: FieldUpdate::Replace(TlsConfig {
                verifier: Some(TlsVerifierConfig(owner)),
                ..fixture.client(TlsBackend::Rustls)
            }),
            ..Default::default()
        },
    );
    let staged = handle.configuration_snapshot().unwrap();
    assert!(retired.upgrade().is_some());
    move_target.send(()).unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    let redirected = wait_snapshot(&handle, |_| {
        receipt.activation().1 == ActivationState::UnavailableAfterRedirect
    });
    assert!(retired.upgrade().is_none());
    assert_eq!(redirected.desired_connection, staged.desired_connection);
    assert_eq!(redirected.effective_connection, staged.effective_connection);
    assert_eq!(redirected.desired_connection_revision, 1);
    assert_eq!(redirected.effective_connection_revision, 0);
    let tuning = stage(
        &handle,
        RuntimeConfigUpdate {
            read_batch_size: FieldUpdate::Replace(17),
            ..Default::default()
        },
    );
    assert_eq!(tuning.revision, 2);
    let invalid = handle
        .try_configuration_update(RuntimeConfigUpdate {
            pending_throttle: FieldUpdate::Replace(std::time::Duration::MAX),
            ..Default::default()
        })
        .unwrap();
    assert!(invalid.completion.wait_timeout(DEADLINE).is_err());
    let mixed = handle
        .try_configuration_update(RuntimeConfigUpdate {
            read_batch_size: FieldUpdate::Replace(99),
            credentials: credentials(b"rejected"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        mixed.completion.wait_timeout(DEADLINE).unwrap_err().code(),
        ErrorCode::ConfigurationUpdateUnsupported
    );
    wake.send(()).unwrap();
    wait_snapshot(&handle, |snapshot| snapshot.effective_tuning_revision == 2);
    assert_eq!(tuning.activation().0, ActivationState::Activated);
    assert!(retired.upgrade().is_none());
    client.closer().close_now(DEADLINE).unwrap();
    broker.join().unwrap();
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn custom_connectors_release_retired_startup_tls_owners_after_rotation() {
    use std::sync::Arc;

    struct Policy;
    impl TlsVerifier for Policy {
        fn verify(
            &self,
            _: &TlsVerificationRequest<'_>,
        ) -> std::result::Result<(), TlsCallbackReason> {
            Ok(())
        }
    }
    let fixture = tls::Fixture::new();
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = config(mqtt5, port);
        let policy = Arc::new(Policy);
        let retired = Arc::downgrade(&policy);
        config.common.transport = TransportConfig::Tls(TlsConfig {
            verifier: Some(TlsVerifierConfig(policy)),
            ..fixture.client(TlsBackend::Rustls)
        });
        config.common.username = Some("user".into());
        config.common.password = Some(b"old".as_slice().into());
        config.common.keep_alive = std::time::Duration::ZERO;
        let (connector, requests) = custom::configured();
        let connector_owner = Arc::downgrade(&connector.connector);
        config.common.connector = Some(connector);
        let server = fixture.server.clone();
        let (release, release_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            let mut first = tls::wrap(Box::new(accept(&listener)), server.clone());
            assert_password(&mut frame(&mut first), mqtt5, b"old");
            connack(&mut first, mqtt5);
            release_rx.recv_timeout(DEADLINE).unwrap();
            drop(first);
            let mut second = tls::wrap(Box::new(accept(&listener)), server);
            assert_password(&mut frame(&mut second), mqtt5, b"new");
            connack(&mut second, mqtt5);
            assert_eq!(frame(&mut second)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let handle = client.handle();
        let receipt = stage(
            &handle,
            RuntimeConfigUpdate {
                credentials: credentials(b"new"),
                broker_tls: FieldUpdate::Replace(fixture.client(TlsBackend::Rustls)),
                ..Default::default()
            },
        );
        // The active startup connection must still retain its verifier.
        assert!(retired.upgrade().is_some());
        assert_eq!(receipt.activation().1, ActivationState::Staged);
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        wait_snapshot(&handle, |snapshot| {
            snapshot.connection.successful_revision == Some(1) && retired.upgrade().is_none()
        });
        assert_eq!(receipt.activation().1, ActivationState::Activated);
        assert!(connector_owner.upgrade().is_some());
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for (request, generation) in requests.iter().zip([1, 2]) {
                assert_eq!(request.generation, generation);
                assert_eq!(request.client_id, "parity");
                assert_eq!(request.target, format!("127.0.0.1:{port}"));
                assert_eq!(request.mode, TransportMode::Base);
                assert_eq!(
                    request.protocol,
                    if mqtt5 {
                        ProtocolVersion::V5
                    } else {
                        ProtocolVersion::V4
                    }
                );
            }
        }
        client.closer().close_now(DEADLINE).unwrap();
        broker.join().unwrap();
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn superseding_staged_tls_profiles_releases_retired_callback_owners() {
    use std::sync::Arc;

    struct Policy;
    impl TlsVerifier for Policy {
        fn verify(
            &self,
            _: &TlsVerificationRequest<'_>,
        ) -> std::result::Result<(), TlsCallbackReason> {
            Ok(())
        }
    }
    let fixture = tls::Fixture::new();
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.transport = TransportConfig::Tls(fixture.client(TlsBackend::Rustls));
        config.common.keep_alive = std::time::Duration::ZERO;
        let server = fixture.server.clone();
        let broker = thread::spawn(move || {
            let mut socket = tls::wrap(Box::new(accept(&listener)), server);
            assert_eq!(frame(&mut socket)[0], 0x10);
            connack(&mut socket, mqtt5);
            assert_eq!(frame(&mut socket)[0] >> 4, 14);
        });
        let mut client = start(config).unwrap();
        let _events = connected(&mut client);
        let policy = Arc::new(Policy);
        let retired = Arc::downgrade(&policy);
        let profile = TlsConfig {
            verifier: Some(TlsVerifierConfig(policy)),
            ..fixture.client(TlsBackend::Rustls)
        };
        let first = stage(
            &client.handle(),
            RuntimeConfigUpdate {
                broker_tls: FieldUpdate::Replace(profile),
                ..Default::default()
            },
        );
        assert!(retired.upgrade().is_some());
        let second = stage(
            &client.handle(),
            RuntimeConfigUpdate {
                broker_tls: FieldUpdate::Replace(fixture.client(TlsBackend::Rustls)),
                ..Default::default()
            },
        );
        assert_eq!(first.activation().1, ActivationState::Superseded);
        assert!(retired.upgrade().is_none());
        client.closer().close_now(DEADLINE).unwrap();
        assert_eq!(
            second.activation().1,
            ActivationState::ClosedBeforeActivation
        );
        // Retained receipts cannot resurrect a configuration owner.
        assert!(retired.upgrade().is_none());
        broker.join().unwrap();
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn rotated_tls_identity_cannot_resume_the_previous_identity() {
    use rustls::pki_types::pem::PemObject;
    use std::sync::Arc;

    let server_tls = tls::Fixture::new();
    let identities = [
        rcgen::generate_simple_self_signed(vec!["first-client".into()]).unwrap(),
        rcgen::generate_simple_self_signed(vec!["second-client".into()]).unwrap(),
    ];
    let mut roots = rustls::RootCertStore::empty();
    for identity in &identities {
        roots.add(identity.cert.der().clone()).unwrap();
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .unwrap();
    let server = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![
                    rustls::pki_types::CertificateDer::from_pem_slice(server_tls.pem.as_bytes())
                        .unwrap(),
                ],
                rustls::pki_types::PrivateKeyDer::from_pem_slice(server_tls.key_pem.as_bytes())
                    .unwrap(),
            )
            .unwrap(),
    );
    let profiles = identities
        .iter()
        .map(|identity| TlsConfig {
            identity: Some(TlsClientIdentity::RustlsPem {
                certificate: identity.cert.pem().into(),
                private_key: SecretBytes::new(identity.signing_key.serialize_pem().into_bytes()),
            }),
            // Keep normal resumption enabled. Rotation must create a fresh client cache.
            resumption_policy: TlsResumptionPolicy::Default,
            ..server_tls.client(TlsBackend::Rustls)
        })
        .collect::<Vec<_>>();

    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(mqtt5, listener.local_addr().unwrap().port());
        config.common.transport = TransportConfig::Tls(profiles[0].clone());
        config.common.keep_alive = std::time::Duration::ZERO;
        let server = server.clone();
        let certificates = identities
            .iter()
            .map(|identity| identity.cert.der().clone())
            .collect::<Vec<_>>();
        let (release, release_rx) = mpsc::channel();
        let broker = thread::spawn(move || {
            for (index, certificate) in certificates.iter().enumerate() {
                let mut socket = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server.clone()).unwrap(),
                    accept(&listener),
                );
                assert_eq!(frame(&mut socket)[0], 0x10);
                assert_eq!(
                    socket.conn.peer_certificates().unwrap(),
                    std::slice::from_ref(certificate)
                );
                assert_eq!(
                    socket.conn.handshake_kind(),
                    Some(rustls::HandshakeKind::Full)
                );
                connack(&mut socket, mqtt5);
                socket.flush().unwrap();
                if index == 0 {
                    release_rx.recv_timeout(DEADLINE).unwrap();
                } else {
                    assert_eq!(frame(&mut socket)[0] >> 4, 14);
                }
            }
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let receipt = stage(
            &client.handle(),
            RuntimeConfigUpdate {
                broker_tls: FieldUpdate::Replace(profiles[1].clone()),
                ..Default::default()
            },
        );
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert_eq!(receipt.activation().1, ActivationState::Activated);
        client.closer().close_now(DEADLINE).unwrap();
        broker.join().unwrap();
    }
}
