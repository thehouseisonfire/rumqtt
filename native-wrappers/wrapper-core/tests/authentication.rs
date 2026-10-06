use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use rumqttc_wrapper_core::*;

mod support;

#[test]
fn explicit_callback_rejection_is_terminal_and_releases_owner() {
    struct Reject;
    impl Authenticator for Reject {
        fn respond(
            &self,
            _: AuthContext,
            _: AuthChallenge,
        ) -> std::result::Result<AuthAction, AuthFailure> {
            Err(AuthFailure::Rejected)
        }
    }
    let owner = Arc::new(Reject);
    let weak = Arc::downgrade(&owner);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let mut client = support::start(config(port, owner)).unwrap();
    let mut events = client.take_events().unwrap();
    let event = support::until(&mut events, |event| {
        matches!(event, WrapperEvent::DriverTerminated(_))
    });
    let WrapperEvent::DriverTerminated(error) = event else {
        unreachable!()
    };
    assert_eq!(error.kind(), ErrorKind::Authentication);
    assert_eq!(error.auth_failure(), Some(AuthFailure::Rejected));
    client.join(support::DEADLINE).unwrap();
    assert!(weak.upgrade().is_none());
    broker.join();
}

#[test]
fn broker_authentication_method_change_retains_typed_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        let mut packet = BytesMut::new();
        rumqttc_v5::Auth::new(
            rumqttc_v5::AuthReasonCode::Continue,
            Some(rumqttc_v5::AuthProperties {
                method: Some("changed".into()),
                data: Some(Bytes::from_static(b"secret-challenge")),
                ..Default::default()
            }),
        )
        .write(&mut packet)
        .unwrap();
        socket.write_all(&packet).unwrap();
    });
    let mut client = support::start(config(
        port,
        Arc::new(Mechanism {
            contexts: Mutex::new(vec![]),
        }),
    ))
    .unwrap();
    let mut events = client.take_events().unwrap();
    let event = support::until(&mut events, |event| {
        matches!(event, WrapperEvent::Disconnected { .. })
    });
    let WrapperEvent::Disconnected { error, phase } = event else {
        unreachable!()
    };
    assert_eq!(error.kind(), ErrorKind::Protocol);
    assert_eq!(phase, ConnectionPhase::Attempt);
    assert_eq!(error.context().generation, None);
    assert!(!format!("{error:?} {error}").contains("secret-challenge"));
    client.closer().close_now(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn overlapping_reauthentication_is_rejected_before_the_active_exchange_closes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Auth(_)
        ));
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(support::DEADLINE).unwrap();
        drop(socket);
    });
    let mut client = support::start(config(
        port,
        Arc::new(Mechanism {
            contexts: Mutex::new(vec![]),
        }),
    ))
    .unwrap();
    let _events = support::connected(&mut client);
    let first = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    ready_rx.recv_timeout(support::DEADLINE).unwrap();
    let second = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    let error = support::terminal(&second).unwrap_err();
    assert_eq!(error.auth_failure(), Some(AuthFailure::Overlapping));
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    assert!(first.completion.try_wait().unwrap().is_none());
    release_tx.send(()).unwrap();
    assert_eq!(
        support::terminal(&first).unwrap_err().auth_failure(),
        Some(AuthFailure::ConnectionClosed)
    );
    client.closer().close_now(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the traffic, broker and shutdown scenarios together"
)]
fn overlapping_reauthentication_traffic_preserves_mqtt_deadlines_and_shutdown() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct StopTraffic(Arc<AtomicBool>);
    impl Drop for StopTraffic {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    for timeout in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (traffic_tx, traffic_rx) = std::sync::mpsc::channel();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let broker = support::Broker::spawn(move || {
            let mut socket = support::accept(&listener);
            read_packet(&mut socket);
            socket
                .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                .unwrap();
            let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                panic!("reauthentication request expected")
            };
            assert_eq!(auth.code, rumqttc_v5::AuthReasonCode::ReAuthenticate);
            ready_tx.send(()).unwrap();
            traffic_rx.recv_timeout(support::DEADLINE).unwrap();
            // QoS 1 PUBLISH, topic "a", packet ID 1, payload "abc". Network
            // reads and the automatic PUBACK must progress during the AUTH exchange.
            socket
                .write_all(b"\x32\x09\x00\x01a\x00\x01\x00abc")
                .unwrap();
            let rumqttc_v5::Packet::PubAck(ack) = read_packet(&mut socket) else {
                panic!("PUBACK expected")
            };
            assert_eq!(ack.pkid, 1);
            progress_tx.send(()).unwrap();
            if timeout {
                assert_eq!(socket.read(&mut [0]).unwrap(), 0);
            } else {
                assert!(matches!(
                    read_packet(&mut socket),
                    rumqttc_v5::Packet::Disconnect(_)
                ));
            }
        });
        let mut configuration = config(
            port,
            Arc::new(Mechanism {
                contexts: Mutex::new(vec![]),
            }),
        );
        if timeout {
            let ProtocolConfig::V5(v5) = &mut configuration.protocol else {
                unreachable!()
            };
            v5.authenticator.as_mut().unwrap().exchange_timeout = Duration::from_secs(1);
        }
        let mut client = NativeClient::start(configuration).unwrap();
        let mut events = support::connected(&mut client);
        let handle = client.handle();
        let first = handle.try_admit(Command::Reauthenticate(None)).unwrap();
        ready_rx.recv_timeout(support::DEADLINE).unwrap();
        let overlap = handle.try_admit(Command::Reauthenticate(None)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        std::thread::scope(|scope| {
            // Overlap rejections register ready futures without consuming native
            // request-channel or publish-budget capacity. Dropping observers does
            // not cancel those registrations. Stop the producer even if an assertion fails.
            let _stop_on_exit = StopTraffic(stop.clone());
            let producer = scope.spawn(|| {
                let mut started = false;
                while !stop.load(Ordering::Acquire) {
                    match handle.try_admit(Command::Reauthenticate(None)) {
                        Ok(admission) => drop(admission),
                        Err(error) if error.kind() == ErrorKind::Shutdown => break,
                        Err(error) => panic!("unexpected registration error: {error}"),
                    }
                    if !started {
                        traffic_tx.send(()).unwrap();
                        started = true;
                    }
                    std::thread::yield_now();
                }
                assert!(started);
            });

            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::IncomingPublish(_))
            });
            progress_rx.recv_timeout(support::DEADLINE).unwrap();
            let diagnostics = handle.try_admit(Command::Diagnostics).unwrap();
            assert!(
                matches!(support::terminal(&diagnostics).unwrap(), Completion::Diagnostics(snapshot) if snapshot.connected)
            );
            let error = support::terminal(&overlap).unwrap_err();
            assert_eq!(error.auth_failure(), Some(AuthFailure::Overlapping));
            assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);

            if timeout {
                let event = support::until(&mut events, |event| {
                    matches!(event, WrapperEvent::DriverTerminated(_))
                });
                let WrapperEvent::DriverTerminated(error) = event else {
                    unreachable!()
                };
                assert_eq!(error.auth_failure(), Some(AuthFailure::Timeout));
            } else {
                // Keep submitting registrations until shutdown itself closes admission.
                client.closer().close_now(support::DEADLINE).unwrap();
            }
            stop.store(true, Ordering::Release);
            producer.join().unwrap();
        });
        let error = support::terminal(&first).unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        client.join(support::DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn rejected_overlap_preserves_success_and_releases_admission_for_the_next_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        for exchange in 0..2 {
            let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                panic!("AUTH start expected")
            };
            assert_eq!(auth.code, rumqttc_v5::AuthReasonCode::ReAuthenticate);
            if exchange == 0 {
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(support::DEADLINE).unwrap();
            }
            send_auth(&mut socket, rumqttc_v5::AuthReasonCode::Success, b"proof");
        }
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Disconnect(_)
        ));
    });
    let mut client = support::start(config(
        port,
        Arc::new(Mechanism {
            contexts: Mutex::new(vec![]),
        }),
    ))
    .unwrap();
    let _events = support::connected(&mut client);
    let first = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    ready_rx.recv_timeout(support::DEADLINE).unwrap();
    let second = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    let error = support::terminal(&second).unwrap_err();
    assert_eq!(error.auth_failure(), Some(AuthFailure::Overlapping));
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    assert!(first.completion.try_wait().unwrap().is_none());
    release_tx.send(()).unwrap();
    assert_eq!(
        support::terminal(&first).unwrap(),
        Completion::Authenticated
    );
    let third = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    assert_eq!(
        support::terminal(&third).unwrap(),
        Completion::Authenticated
    );
    client.closer().close(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn authentication_reconnect_and_pending_challenge_shutdown_release_exchange() {
    for reauth in [false, true] {
        for reconnect in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let broker = support::Broker::spawn(move || {
                let mut socket = support::accept(&listener);
                read_packet(&mut socket);
                if reauth {
                    socket
                        .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                        .unwrap();
                    assert!(matches!(
                        read_packet(&mut socket),
                        rumqttc_v5::Packet::Auth(_)
                    ));
                }
                send_auth(
                    &mut socket,
                    rumqttc_v5::AuthReasonCode::Continue,
                    b"challenge",
                );
                assert!(matches!(
                    read_packet(&mut socket),
                    rumqttc_v5::Packet::Auth(_)
                ));
                ready_tx.send(()).unwrap();
                release_rx.recv_timeout(support::DEADLINE).unwrap();
                if reconnect {
                    drop(socket);
                    let mut socket = support::accept(&listener);
                    assert!(matches!(
                        read_packet(&mut socket),
                        rumqttc_v5::Packet::Connect(..)
                    ));
                    socket
                        .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                        .unwrap();
                    assert!(matches!(
                        read_packet(&mut socket),
                        rumqttc_v5::Packet::Disconnect(_)
                    ));
                } else {
                    let mut bytes = [0; 32];
                    let _ = socket.read(&mut bytes);
                }
            });
            let owner = Arc::new(Mechanism {
                contexts: Mutex::new(vec![]),
            });
            let weak = Arc::downgrade(&owner);
            let mut client = support::start(config(port, owner.clone())).unwrap();
            let mut events = client.take_events().unwrap();
            let operation = if reauth {
                support::until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                Some(
                    client
                        .handle()
                        .try_admit(Command::Reauthenticate(None))
                        .unwrap(),
                )
            } else {
                None
            };
            ready_rx.recv_timeout(support::DEADLINE).unwrap();
            release_tx.send(()).unwrap();
            if reconnect {
                support::until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                let contexts = owner.contexts.lock().unwrap();
                assert!(
                    contexts.iter().any(|context| context.generation == 2
                        && context.exchange == AuthExchange::Initial)
                );
                drop(contexts);
            }
            client.closer().close_now(support::DEADLINE).unwrap();
            if let Some(operation) = operation {
                let error = support::terminal(&operation).unwrap_err();
                if reconnect {
                    assert_eq!(error.auth_failure(), Some(AuthFailure::ConnectionClosed));
                } else {
                    assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
                }
            }
            drop(owner);
            assert!(weak.upgrade().is_none());
            broker.join();
        }
    }
}

struct Mechanism {
    contexts: Mutex<Vec<AuthContext>>,
}
impl Authenticator for Mechanism {
    fn respond(
        &self,
        context: AuthContext,
        challenge: AuthChallenge,
    ) -> std::result::Result<AuthAction, AuthFailure> {
        self.contexts.lock().unwrap().push(context);
        match challenge {
            AuthChallenge::Start => Ok(AuthAction::Send(AuthProperties {
                data: Some(Bytes::from_static(b"first")),
                ..Default::default()
            })),
            AuthChallenge::Continue(properties) => {
                assert_eq!(
                    properties.unwrap().data.as_deref(),
                    Some(b"challenge".as_slice())
                );
                Ok(AuthAction::Send(AuthProperties {
                    data: Some(Bytes::from_static(b"response")),
                    ..Default::default()
                }))
            }
            AuthChallenge::Success(_) | AuthChallenge::Failed => Ok(AuthAction::Complete),
        }
    }
}

fn read_packet(stream: &mut TcpStream) -> rumqttc_v5::Packet {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    let mut frame = BytesMut::from(byte.as_slice());
    let mut length = 0;
    let mut shift = 0;
    loop {
        stream.read_exact(&mut byte).unwrap();
        frame.extend_from_slice(&byte);
        length |= usize::from(byte[0] & 127) << shift;
        if byte[0] < 128 {
            break;
        }
        shift += 7;
        assert!(shift < 28);
    }
    assert!(length < 65536);
    let header = frame.len();
    frame.resize(header + length, 0);
    stream.read_exact(&mut frame[header..]).unwrap();
    rumqttc_v5::Packet::read(&mut frame, None).unwrap()
}

fn send_auth(stream: &mut TcpStream, reason: rumqttc_v5::AuthReasonCode, data: &'static [u8]) {
    let auth = rumqttc_v5::Auth::new(
        reason,
        Some(rumqttc_v5::AuthProperties {
            method: Some("test".into()),
            data: Some(Bytes::from_static(data)),
            reason: None,
            user_properties: vec![],
        }),
    );
    let mut frame = BytesMut::new();
    auth.write(&mut frame).unwrap();
    stream.write_all(&frame).unwrap();
}

fn config(port: u16, mechanism: Arc<dyn Authenticator>) -> ClientConfig {
    let mut config = ClientConfig::v5("auth", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.authentication_method = Some("test".into());
    v5.authenticator = Some(AuthenticatorConfig::new(mechanism));
    config
}

fn async_config(
    port: u16,
    authority: Arc<dyn AsyncAuthenticator>,
    timeout: Duration,
) -> ClientConfig {
    let mut config = ClientConfig::v5("auth", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.authentication_method = Some("test".into());
    let mut authority = AsyncAuthenticatorConfig::new(authority);
    authority.exchange_timeout = timeout;
    v5.async_authenticator = Some(authority);
    config
}

#[test]
fn ready_async_callbacks_cannot_accept_responses_after_the_exchange_deadline() {
    #[derive(Clone, Copy)]
    enum Stage {
        Start,
        Continue,
        Success,
    }
    struct Authority {
        stage: Stage,
        construction: bool,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            let slow = matches!(
                (self.stage, challenge),
                (Stage::Start, AsyncAuthChallenge::Start)
                    | (Stage::Continue, AsyncAuthChallenge::Continue { .. })
                    | (Stage::Success, AsyncAuthChallenge::Success { .. })
            );
            if slow && self.construction {
                std::thread::sleep(Duration::from_millis(150));
            }
            let slow_poll = slow && !self.construction;
            Box::pin(async move {
                if slow_poll {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Ok(AuthAction::Complete)
            })
        }
    }
    for stage in [Stage::Start, Stage::Continue, Stage::Success] {
        for construction in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = support::Broker::spawn(move || {
                let mut socket = support::accept(&listener);
                if !matches!(stage, Stage::Start) {
                    read_packet(&mut socket);
                    if matches!(stage, Stage::Continue) {
                        send_auth(
                            &mut socket,
                            rumqttc_v5::AuthReasonCode::Continue,
                            b"challenge",
                        );
                    } else {
                        socket
                            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                            .unwrap();
                    }
                }
                assert_eq!(
                    socket.read(&mut [0]).unwrap(),
                    0,
                    "late response reached the broker"
                );
            });
            let mut client = support::start(async_config(
                port,
                Arc::new(Authority {
                    stage,
                    construction,
                }),
                Duration::from_millis(50),
            ))
            .unwrap();
            let mut events = client.take_events().unwrap();
            loop {
                match events.recv_timeout(support::DEADLINE).unwrap().unwrap() {
                    WrapperEvent::Connected { .. } => panic!("late authentication succeeded"),
                    WrapperEvent::Authentication(auth) => {
                        assert_ne!(auth.stage, AuthStage::Succeeded);
                    }
                    WrapperEvent::DriverTerminated(error) => {
                        assert_eq!(error.auth_failure(), Some(AuthFailure::Timeout));
                        break;
                    }
                    _ => {}
                }
            }
            client.join(support::DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[test]
fn authentication_future_destruction_panics_do_not_print_private_payloads() {
    let output = support::process_output(
        std::process::Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "authentication_future_destruction_panics_are_typed_on_completion_timeout_and_close",
            "--nocapture",
        ]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.contains("private-authentication-future-destructor"));
    assert!(!output.contains("panicked at"));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn authentication_future_destruction_panics_are_typed_on_completion_timeout_and_close() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Clone, Copy)]
    enum Mode {
        Completion,
        Timeout,
        ConstructionTimeout,
        Close,
    }
    struct PanickingFuture {
        ready: bool,
        entered: Option<std::sync::mpsc::Sender<()>>,
        dropped: Arc<AtomicUsize>,
    }
    impl std::future::Future for PanickingFuture {
        type Output = std::result::Result<AuthAction, AuthFailure>;
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
            }
            if self.ready {
                std::task::Poll::Ready(Ok(AuthAction::Complete))
            } else {
                std::task::Poll::Pending
            }
        }
    }
    impl Drop for PanickingFuture {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            panic!("private-authentication-future-destructor");
        }
    }
    struct Authority {
        mode: Mode,
        entered: std::sync::mpsc::Sender<()>,
        failed: std::sync::mpsc::Sender<AuthFailure>,
        dropped: Arc<AtomicUsize>,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            if matches!(
                (self.mode, context.exchange, challenge),
                (
                    Mode::Completion,
                    AuthExchange::Initial,
                    AsyncAuthChallenge::Success { .. }
                ) | (
                    Mode::Timeout | Mode::ConstructionTimeout,
                    AuthExchange::Initial,
                    AsyncAuthChallenge::Start
                ) | (
                    Mode::Close,
                    AuthExchange::Reauthentication,
                    AsyncAuthChallenge::Start
                )
            ) {
                if matches!(self.mode, Mode::ConstructionTimeout) {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Box::pin(PanickingFuture {
                    ready: matches!(self.mode, Mode::Completion),
                    entered: Some(self.entered.clone()),
                    dropped: self.dropped.clone(),
                })
            } else {
                Box::pin(async { Ok(AuthAction::Complete) })
            }
        }
        fn failure(&self, _: AuthContext, failure: AuthFailure) {
            self.failed.send(failure).unwrap();
        }
    }
    for mode in [
        Mode::Completion,
        Mode::Timeout,
        Mode::ConstructionTimeout,
        Mode::Close,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = support::Broker::spawn(move || {
            let mut socket = support::accept(&listener);
            if !matches!(mode, Mode::Timeout | Mode::ConstructionTimeout) {
                read_packet(&mut socket);
                socket
                    .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                    .unwrap();
            }
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (failed_tx, failed_rx) = std::sync::mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut client = support::start(async_config(
            port,
            Arc::new(Authority {
                mode,
                entered: entered_tx,
                failed: failed_tx,
                dropped: dropped.clone(),
            }),
            if matches!(mode, Mode::Timeout | Mode::ConstructionTimeout) {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(30)
            },
        ))
        .unwrap();
        let mut events = client.take_events().unwrap();
        let operation = if matches!(mode, Mode::Close) {
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            let operation = client
                .handle()
                .try_admit(Command::Reauthenticate(None))
                .unwrap();
            entered_rx.recv_timeout(support::DEADLINE).unwrap();
            let error = client.closer().close_now(support::DEADLINE).unwrap_err();
            assert_eq!(error.auth_failure(), Some(AuthFailure::Panic));
            Some(operation)
        } else {
            None
        };
        let event = support::until(&mut events, |event| {
            matches!(event, WrapperEvent::DriverTerminated(_))
        });
        let WrapperEvent::DriverTerminated(error) = event else {
            unreachable!()
        };
        assert_eq!(error.auth_failure(), Some(AuthFailure::Panic));
        assert_ne!(error.code(), ErrorCode::InternalPanic);
        client.join(support::DEADLINE).unwrap();
        assert_eq!(
            failed_rx.recv_timeout(support::DEADLINE).unwrap(),
            AuthFailure::Panic
        );
        assert!(failed_rx.try_recv().is_err());
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        if let Some(operation) = operation {
            assert_eq!(
                support::terminal(&operation).unwrap_err().auth_failure(),
                Some(AuthFailure::Panic)
            );
        }
        broker.join();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn deferred_reauthentication_survives_publish_and_keepalive_read_arbitration() {
    struct Deferred {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }
    impl AsyncAuthenticator for Deferred {
        fn respond(&self, _: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            match challenge {
                AsyncAuthChallenge::Continue { .. } => {
                    let entered = self.entered.clone();
                    let release = self.release.lock().unwrap().take().unwrap();
                    Box::pin(async move {
                        entered.send(()).unwrap();
                        release.await.unwrap();
                        Ok(AuthAction::Send(AuthProperties {
                            data: Some(Bytes::from_static(b"reply")),
                            ..Default::default()
                        }))
                    })
                }
                _ => Box::pin(async { Ok(AuthAction::Complete) }),
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Auth(_)
        ));
        send_auth(
            &mut socket,
            rumqttc_v5::AuthReasonCode::Continue,
            b"challenge",
        );
        let rumqttc_v5::Packet::Auth(response) = read_packet(&mut socket) else {
            panic!("deferred AUTH response expected before queued requests")
        };
        assert_eq!(response.code, rumqttc_v5::AuthReasonCode::Continue);
        assert_eq!(
            response.properties.unwrap().data.as_deref(),
            Some(b"reply".as_slice())
        );
        send_auth(&mut socket, rumqttc_v5::AuthReasonCode::Success, b"proof");
        let mut published = false;
        loop {
            match read_packet(&mut socket) {
                rumqttc_v5::Packet::PingReq => socket.write_all(b"\xd0\x00").unwrap(),
                rumqttc_v5::Packet::Publish(publish) => {
                    assert_eq!(publish.topic.as_ref(), b"queued");
                    published = true;
                }
                rumqttc_v5::Packet::Disconnect(_) => {
                    assert!(published);
                    break;
                }
                packet => panic!("unexpected packet {packet:?}"),
            }
        }
    });
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut config = async_config(
        port,
        Arc::new(Deferred {
            entered: entered_tx,
            release: Mutex::new(Some(release_rx)),
        }),
        support::DEADLINE,
    );
    config.common.keep_alive = Duration::from_secs(1);
    let mut client = support::start(config).unwrap();
    let _events = support::connected(&mut client);
    let reauthentication = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    entered_rx.recv_timeout(support::DEADLINE).unwrap();
    let publish = client
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "queued".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    // Both a ready request and a due keepalive must leave the consumed challenge intact.
    std::thread::sleep(Duration::from_millis(1100));
    release_tx
        .send(())
        .expect("authentication response was cancelled");
    assert!(matches!(
        support::terminal(&reauthentication).unwrap(),
        Completion::Authenticated
    ));
    assert!(matches!(
        support::terminal(&publish).unwrap(),
        Completion::Publish(_)
    ));
    client.closer().close(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn async_exchange_deadline_expires_while_waiting_for_initial_or_reauthentication_broker_packets() {
    struct Authority(std::sync::mpsc::Sender<AuthFailure>);
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AuthContext, _: AsyncAuthChallenge) -> AuthFuture {
            Box::pin(async { Ok(AuthAction::Complete) })
        }
        fn failure(&self, _: AuthContext, failure: AuthFailure) {
            self.0.send(failure).unwrap();
        }
    }
    for (reauthenticate, continue_exchange) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = support::Broker::spawn(move || {
            let mut socket = support::accept(&listener);
            read_packet(&mut socket);
            if reauthenticate {
                socket
                    .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                    .unwrap();
                assert!(matches!(
                    read_packet(&mut socket),
                    rumqttc_v5::Packet::Auth(_)
                ));
            }
            if continue_exchange {
                send_auth(
                    &mut socket,
                    rumqttc_v5::AuthReasonCode::Continue,
                    b"challenge",
                );
                let rumqttc_v5::Packet::Auth(response) = read_packet(&mut socket) else {
                    panic!("AUTH continuation expected")
                };
                assert_eq!(response.code, rumqttc_v5::AuthReasonCode::Continue);
            }
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let (failed_tx, failed_rx) = std::sync::mpsc::channel();
        let mut client = support::start(async_config(
            port,
            Arc::new(Authority(failed_tx)),
            Duration::from_millis(100),
        ))
        .unwrap();
        let mut events = client.take_events().unwrap();
        let operation = reauthenticate.then(|| {
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            client
                .handle()
                .try_admit(Command::Reauthenticate(None))
                .unwrap()
        });
        assert_eq!(
            failed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            AuthFailure::Timeout
        );
        let mut saw_failure = false;
        loop {
            match events.recv_timeout(support::DEADLINE).unwrap().unwrap() {
                WrapperEvent::Authentication(event) if event.stage == AuthStage::Failed => {
                    assert_eq!(event.failure, Some(AuthFailure::Timeout));
                    saw_failure = true;
                }
                WrapperEvent::DriverTerminated(error) => {
                    assert_eq!(error.auth_failure(), Some(AuthFailure::Timeout));
                    assert!(saw_failure);
                    break;
                }
                _ => {}
            }
        }
        client.join(support::DEADLINE).unwrap();
        if let Some(operation) = operation {
            assert_eq!(
                support::terminal(&operation).unwrap_err().auth_failure(),
                Some(AuthFailure::Timeout)
            );
        }
        assert!(failed_rx.try_recv().is_err());
        broker.join();
    }
}

#[test]
fn async_authentication_panic_payload_is_not_printed_by_the_host_hook() {
    let output = support::process_output(
        std::process::Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "async_authentication_panics_are_typed_and_release_the_authority",
            "--nocapture",
        ]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.contains("secret-async-authentication-value"));
    assert!(!output.contains("panicked at"));
}

#[test]
fn initial_auth_success_is_rejected_before_notifying_the_async_authority() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Authority(AtomicUsize);
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            if matches!(challenge, AsyncAuthChallenge::Success { .. }) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
            Box::pin(async { Ok(AuthAction::Complete) })
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        let mut packets = BytesMut::new();
        rumqttc_v5::Auth::new(
            rumqttc_v5::AuthReasonCode::Success,
            Some(rumqttc_v5::AuthProperties {
                method: Some("test".into()),
                ..Default::default()
            }),
        )
        .write(&mut packets)
        .unwrap();
        rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::Success,
            properties: Some(connack_properties(
                Some("test".into()),
                Some(Bytes::from_static(b"proof")),
            )),
        }
        .write(&mut packets)
        .unwrap();
        socket.write_all(&packets).unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let authority = Arc::new(Authority(AtomicUsize::new(0)));
    let mut client =
        support::start(async_config(port, authority.clone(), support::DEADLINE)).unwrap();
    let mut events = client.take_events().unwrap();
    loop {
        match events.recv_timeout(support::DEADLINE).unwrap().unwrap() {
            WrapperEvent::Connected { .. } => panic!("invalid initial AUTH sequence was accepted"),
            WrapperEvent::Authentication(event) => assert_ne!(event.stage, AuthStage::Succeeded),
            WrapperEvent::Disconnected { error, .. } => {
                assert_eq!(error.kind(), ErrorKind::Protocol);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(authority.0.load(Ordering::Relaxed), 0);
    client.closer().close_now(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn async_authentication_panics_are_typed_and_release_the_authority() {
    struct Panicking;
    impl AsyncAuthenticator for Panicking {
        fn respond(&self, _: AuthContext, _: AsyncAuthChallenge) -> AuthFuture {
            Box::pin(async {
                tokio::task::yield_now().await;
                panic!("secret-async-authentication-value");
            })
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let owner = Arc::new(Panicking);
    let mut client = support::start(async_config(port, owner.clone(), support::DEADLINE)).unwrap();
    let mut events = client.take_events().unwrap();
    let event = support::until(&mut events, |event| {
        matches!(event, WrapperEvent::DriverTerminated(_))
    });
    let WrapperEvent::DriverTerminated(error) = event else {
        unreachable!()
    };
    assert_eq!(error.auth_failure(), Some(AuthFailure::Panic));
    client.join(support::DEADLINE).unwrap();
    assert_eq!(Arc::strong_count(&owner), 1);
    broker.join();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn authentication_timeout_during_session_save_notifies_the_authority_with_timeout() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct PendingSave {
        reauth_started: Arc<AtomicBool>,
        entered: std::sync::mpsc::Sender<()>,
        cancelled: Arc<AtomicUsize>,
    }
    struct CancelledSave(Arc<AtomicUsize>);
    impl Drop for CancelledSave {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl SessionStore for PendingSave {
        fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            Box::pin(async { Ok(None) })
        }
        fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
            if self.reauth_started.load(Ordering::Relaxed) {
                let entered = self.entered.clone();
                let cancelled = CancelledSave(self.cancelled.clone());
                Box::pin(async move {
                    let _cancelled = cancelled;
                    entered.send(()).unwrap();
                    std::future::pending().await
                })
            } else {
                Box::pin(async { Ok(()) })
            }
        }
        fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
            Box::pin(async { Ok(()) })
        }
    }
    struct Authority {
        reauth_started: Arc<AtomicBool>,
        failed: std::sync::mpsc::Sender<AuthFailure>,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            if context.exchange == AuthExchange::Reauthentication {
                assert!(matches!(challenge, AsyncAuthChallenge::Start));
                self.reauth_started.store(true, Ordering::Relaxed);
            }
            Box::pin(async { Ok(AuthAction::Complete) })
        }
        fn failure(&self, context: AuthContext, failure: AuthFailure) {
            assert_eq!(context.exchange, AuthExchange::Reauthentication);
            self.failed.send(failure).unwrap();
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        // Persistence blocks before the reauthentication packet is written.
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });
    let reauth_started = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (failed_tx, failed_rx) = std::sync::mpsc::channel();
    let mut config = async_config(
        port,
        Arc::new(Authority {
            reauth_started: reauth_started.clone(),
            failed: failed_tx,
        }),
        Duration::from_millis(200),
    );
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.clean_start = false;
    v5.connect_properties.session_expiry_interval = Some(60);
    let mut store = SessionStoreConfig::new(
        Arc::new(PendingSave {
            reauth_started,
            entered: entered_tx,
            cancelled: cancelled.clone(),
        }),
        "auth-timeout",
    );
    store.timeout = Duration::from_secs(30);
    v5.session_store = Some(store);
    let mut client = support::start(config).unwrap();
    let mut events = support::connected(&mut client);
    let operation = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    entered_rx.recv_timeout(support::DEADLINE).unwrap();
    let mut stages = vec![];
    loop {
        let event = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
        match event {
            WrapperEvent::Authentication(event)
                if event.exchange == AuthExchange::Reauthentication =>
            {
                stages.push(event.stage);
                if event.stage == AuthStage::Failed {
                    assert_eq!(event.failure, Some(AuthFailure::Timeout));
                }
            }
            WrapperEvent::DriverTerminated(error) => {
                assert_eq!(error.auth_failure(), Some(AuthFailure::Timeout));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(stages, [AuthStage::Started, AuthStage::Failed]);
    client.join(support::DEADLINE).unwrap();
    assert_eq!(
        failed_rx.recv_timeout(support::DEADLINE).unwrap(),
        AuthFailure::Timeout
    );
    assert!(failed_rx.try_recv().is_err());
    assert_eq!(cancelled.load(Ordering::Relaxed), 1);
    assert_eq!(
        support::terminal(&operation).unwrap_err().auth_failure(),
        Some(AuthFailure::Timeout)
    );
    broker.join();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn immediate_close_notifies_pending_initial_authentication_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy, Debug)]
    enum Stage {
        Start,
        Continue,
        Success,
    }
    struct DropSignal(Arc<AtomicUsize>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    struct Authority {
        stage: Stage,
        panic_on_failure: bool,
        entered: std::sync::mpsc::Sender<()>,
        failed: std::sync::mpsc::Sender<(AuthContext, AuthFailure)>,
        dropped: Arc<AtomicUsize>,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            assert_eq!(context.exchange, AuthExchange::Initial);
            let defer = matches!(
                (self.stage, challenge),
                (Stage::Start, AsyncAuthChallenge::Start)
                    | (Stage::Continue, AsyncAuthChallenge::Continue { .. })
                    | (Stage::Success, AsyncAuthChallenge::Success { .. })
            );
            if defer {
                let entered = self.entered.clone();
                let drop_signal = DropSignal(self.dropped.clone());
                Box::pin(async move {
                    let _drop_signal = drop_signal;
                    entered.send(()).unwrap();
                    std::future::pending().await
                })
            } else {
                Box::pin(async { Ok(AuthAction::Complete) })
            }
        }
        fn failure(&self, context: AuthContext, failure: AuthFailure) {
            // The pending host future must be cancelled before notifying its owner.
            assert_eq!(self.dropped.load(Ordering::Relaxed), 1);
            self.failed.send((context, failure)).unwrap();
            assert!(
                !self.panic_on_failure,
                "private-authentication-cancellation-notification"
            );
        }
    }

    for stage in [Stage::Start, Stage::Continue, Stage::Success] {
        for panic_on_failure in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = support::Broker::spawn(move || {
                let mut socket = support::accept(&listener);
                if !matches!(stage, Stage::Start) {
                    assert!(matches!(
                        read_packet(&mut socket),
                        rumqttc_v5::Packet::Connect(..)
                    ));
                    if matches!(stage, Stage::Continue) {
                        send_auth(
                            &mut socket,
                            rumqttc_v5::AuthReasonCode::Continue,
                            b"challenge",
                        );
                    } else {
                        socket
                            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                            .unwrap();
                    }
                }
                assert_eq!(socket.read(&mut [0]).unwrap(), 0);
            });
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (failed_tx, failed_rx) = std::sync::mpsc::channel();
            let dropped = Arc::new(AtomicUsize::new(0));
            let mut client = support::start(async_config(
                port,
                Arc::new(Authority {
                    stage,
                    panic_on_failure,
                    entered: entered_tx,
                    failed: failed_tx,
                    dropped: dropped.clone(),
                }),
                Duration::from_secs(30),
            ))
            .unwrap();
            let mut events = client.take_events().unwrap();
            entered_rx.recv_timeout(support::DEADLINE).unwrap();
            let closed = client.closer().close_now(support::DEADLINE);
            if panic_on_failure {
                assert_eq!(closed.unwrap_err().auth_failure(), Some(AuthFailure::Panic));
            } else {
                closed.unwrap();
            }
            let (context, failure) = failed_rx.recv_timeout(support::DEADLINE).unwrap();
            assert_eq!(failure, AuthFailure::ConnectionClosed, "{stage:?}");
            assert_eq!(context.exchange, AuthExchange::Initial);
            assert_eq!(context.method, "test");
            assert_eq!(context.generation, 1);
            assert!(failed_rx.try_recv().is_err());
            assert_eq!(dropped.load(Ordering::Relaxed), 1);
            loop {
                let event = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
                match event {
                    WrapperEvent::Connected { .. } => panic!("cancelled handshake connected"),
                    WrapperEvent::Authentication(event) => {
                        assert_ne!(event.stage, AuthStage::Succeeded);
                    }
                    WrapperEvent::ImmediateShutdownCompleted => {
                        assert!(!panic_on_failure);
                        break;
                    }
                    WrapperEvent::DriverTerminated(error) => {
                        assert!(panic_on_failure);
                        assert_eq!(error.auth_failure(), Some(AuthFailure::Panic));
                        assert_ne!(error.code(), ErrorCode::InternalPanic);
                        break;
                    }
                    _ => {}
                }
            }
            client.join(support::DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[test]
fn immediate_close_cancels_pending_async_reauthentication_and_forwards_connect_user_properties() {
    use std::sync::mpsc::{Sender, channel};

    struct DropSignal(Sender<()>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }

    struct DeferredReauth {
        started: Sender<()>,
        dropped: Sender<()>,
    }
    impl AsyncAuthenticator for DeferredReauth {
        fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            match (context.exchange, challenge) {
                (AuthExchange::Initial, AsyncAuthChallenge::Start) => Box::pin(async {
                    Ok(AuthAction::Send(AuthProperties {
                        data: Some(Bytes::from_static(b"initial")),
                        user_properties: vec![("callback".into(), "second".into())],
                        ..Default::default()
                    }))
                }),
                (AuthExchange::Initial, AsyncAuthChallenge::Success { .. }) => {
                    Box::pin(async { Ok(AuthAction::Complete) })
                }
                (AuthExchange::Reauthentication, AsyncAuthChallenge::Start) => {
                    let started = self.started.clone();
                    let dropped = self.dropped.clone();
                    Box::pin(async move {
                        let _drop_signal = DropSignal(dropped);
                        started.send(()).unwrap();
                        std::future::pending().await
                    })
                }
                _ => panic!("unexpected authentication challenge"),
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        let rumqttc_v5::Packet::Connect(connect, _, _) = read_packet(&mut socket) else {
            panic!("CONNECT expected")
        };
        let properties = connect.properties.unwrap();
        assert_eq!(
            properties.authentication_data.as_deref(),
            Some(b"initial".as_slice())
        );
        assert_eq!(
            properties.user_properties,
            [
                ("configured".into(), "first".into()),
                ("callback".into(), "second".into())
            ]
        );
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });

    let (started_tx, started_rx) = channel();
    let (dropped_tx, dropped_rx) = channel();
    let mut config = ClientConfig::v5("auth", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.authentication_method = Some("test".into());
    v5.connect_properties.user_properties = vec![("configured".into(), "first".into())];
    let mut authority = AsyncAuthenticatorConfig::new(Arc::new(DeferredReauth {
        started: started_tx,
        dropped: dropped_tx,
    }));
    authority.exchange_timeout = Duration::from_secs(30);
    v5.async_authenticator = Some(authority);

    let mut client = support::start(config).unwrap();
    let _events = support::connected(&mut client);
    let operation = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    started_rx.recv_timeout(support::DEADLINE).unwrap();
    client.closer().close_now(Duration::from_secs(2)).unwrap();
    dropped_rx.recv_timeout(support::DEADLINE).unwrap();
    assert!(support::terminal(&operation).is_err());
    broker.join();
}

#[test]
fn immediate_close_with_idle_async_authenticator_sends_disconnect() {
    struct IdleAuthority;
    impl AsyncAuthenticator for IdleAuthority {
        fn respond(&self, _: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            assert!(matches!(
                challenge,
                AsyncAuthChallenge::Start | AsyncAuthChallenge::Success { .. }
            ));
            Box::pin(async { Ok(AuthAction::Complete) })
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Connect(..)
        ));
        socket
            .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .unwrap();
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Disconnect(_)
        ));
    });

    let mut config = ClientConfig::v5("auth", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.authentication_method = Some("test".into());
    v5.async_authenticator = Some(AsyncAuthenticatorConfig::new(Arc::new(IdleAuthority)));
    let mut client = support::start(config).unwrap();
    let _events = support::connected(&mut client);
    client.closer().close_now(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn failed_async_continuation_retains_broker_auth_details() {
    struct RejectContinuation;
    impl AsyncAuthenticator for RejectContinuation {
        fn respond(&self, _: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            match challenge {
                AsyncAuthChallenge::Start => Box::pin(async { Ok(AuthAction::Complete) }),
                AsyncAuthChallenge::Continue { .. } => {
                    Box::pin(async { Err(AuthFailure::Rejected) })
                }
                AsyncAuthChallenge::Success { .. } => Box::pin(async { Ok(AuthAction::Complete) }),
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&listener);
        read_packet(&mut socket);
        let mut packet = BytesMut::new();
        rumqttc_v5::Auth::new(
            rumqttc_v5::AuthReasonCode::Continue,
            Some(rumqttc_v5::AuthProperties {
                method: Some("test".into()),
                data: Some(Bytes::from_static(b"challenge")),
                reason: Some("broker reason".into()),
                user_properties: vec![("broker".into(), "value".into())],
            }),
        )
        .write(&mut packet)
        .unwrap();
        socket.write_all(&packet).unwrap();
        assert_eq!(socket.read(&mut [0]).unwrap(), 0);
    });

    let mut config = ClientConfig::v5("auth", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.authentication_method = Some("test".into());
    v5.async_authenticator = Some(AsyncAuthenticatorConfig::new(Arc::new(RejectContinuation)));
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let event = support::until(
        &mut events,
        |event| matches!(event, WrapperEvent::Authentication(auth) if auth.stage == AuthStage::Continue),
    );
    let WrapperEvent::Authentication(auth) = event else {
        unreachable!()
    };
    assert_eq!(auth.reason_code, Some(0x18));
    let properties = auth.properties.unwrap();
    assert_eq!(properties.data.as_deref(), Some(b"challenge".as_slice()));
    assert_eq!(properties.reason_string.as_deref(), Some("broker reason"));
    assert_eq!(
        properties.user_properties,
        [("broker".into(), "value".into())]
    );
    support::until(&mut events, |event| {
        matches!(event, WrapperEvent::DriverTerminated(_))
    });
    client.join(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn failed_async_reauthentication_delivers_broker_details_and_lifecycle_before_termination() {
    struct Authority {
        failure: AuthFailure,
        failed: std::sync::mpsc::Sender<AuthFailure>,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            if matches!(challenge, AsyncAuthChallenge::Continue { .. }) {
                assert_eq!(context.exchange, AuthExchange::Reauthentication);
                let failure = self.failure;
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    match failure {
                        AuthFailure::Rejected => Err(failure),
                        AuthFailure::Panic => panic!("private authentication response"),
                        AuthFailure::Timeout => std::future::pending().await,
                        _ => unreachable!(),
                    }
                })
            } else {
                Box::pin(async { Ok(AuthAction::Complete) })
            }
        }

        fn failure(&self, _: AuthContext, failure: AuthFailure) {
            self.failed.send(failure).unwrap();
        }
    }

    for failure in [
        AuthFailure::Rejected,
        AuthFailure::Panic,
        AuthFailure::Timeout,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = support::Broker::spawn(move || {
            let mut socket = support::accept(&listener);
            read_packet(&mut socket);
            socket
                .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                .unwrap();
            let rumqttc_v5::Packet::Auth(reauthenticate) = read_packet(&mut socket) else {
                panic!("reauthentication request expected")
            };
            assert_eq!(
                reauthenticate.code,
                rumqttc_v5::AuthReasonCode::ReAuthenticate
            );
            let mut packet = BytesMut::new();
            rumqttc_v5::Publish::new(
                "before-auth",
                rumqttc_v5::mqttbytes::QoS::AtMostOnce,
                b"message".to_vec(),
                None,
            )
            .write(&mut packet)
            .unwrap();
            rumqttc_v5::Auth::new(
                rumqttc_v5::AuthReasonCode::Continue,
                Some(rumqttc_v5::AuthProperties {
                    method: Some("test".into()),
                    data: Some(Bytes::from_static(b"challenge")),
                    reason: Some("broker reason".into()),
                    user_properties: vec![
                        ("broker".into(), "first".into()),
                        ("broker".into(), "second".into()),
                    ],
                }),
            )
            .write(&mut packet)
            .unwrap();
            socket.write_all(&packet).unwrap();
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let (failed_tx, failed_rx) = std::sync::mpsc::channel();
        let mut client = support::start(async_config(
            port,
            Arc::new(Authority {
                failure,
                failed: failed_tx,
            }),
            Duration::from_millis(500),
        ))
        .unwrap();
        let mut events = support::connected(&mut client);
        let operation = client
            .handle()
            .try_admit(Command::Reauthenticate(None))
            .unwrap();
        let mut stages = Vec::new();
        let mut published = false;
        loop {
            match events.recv_timeout(support::DEADLINE).unwrap().unwrap() {
                WrapperEvent::Authentication(auth)
                    if auth.exchange == AuthExchange::Reauthentication =>
                {
                    stages.push(auth.stage);
                    match auth.stage {
                        AuthStage::Continue => {
                            assert!(published, "a preceding broker packet was lost or reordered");
                            assert_eq!(auth.reason_code, Some(0x18));
                            let properties = auth.properties.unwrap();
                            assert_eq!(properties.method.as_deref(), Some("test"));
                            assert_eq!(properties.data.as_deref(), Some(b"challenge".as_slice()));
                            assert_eq!(properties.reason_string.as_deref(), Some("broker reason"));
                            assert_eq!(
                                properties.user_properties,
                                [
                                    ("broker".into(), "first".into()),
                                    ("broker".into(), "second".into()),
                                ]
                            );
                        }
                        AuthStage::Failed => assert_eq!(auth.failure, Some(failure)),
                        AuthStage::Started => {}
                        AuthStage::Succeeded => panic!("failed exchange reported success"),
                    }
                }
                WrapperEvent::IncomingPublish(publish) => {
                    assert!(!published);
                    assert_eq!(publish.topic.as_ref(), b"before-auth");
                    assert_eq!(publish.payload.as_ref(), b"message");
                    published = true;
                }
                WrapperEvent::DriverTerminated(error) => {
                    assert_eq!(error.auth_failure(), Some(failure));
                    assert_eq!(
                        stages,
                        [AuthStage::Started, AuthStage::Continue, AuthStage::Failed]
                    );
                    break;
                }
                _ => {}
            }
        }
        client.join(support::DEADLINE).unwrap();
        assert_eq!(
            support::terminal(&operation).unwrap_err().auth_failure(),
            Some(failure)
        );
        assert_eq!(failed_rx.recv_timeout(support::DEADLINE).unwrap(), failure);
        assert!(failed_rx.try_recv().is_err());
        broker.join();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn owned_authentication_rejects_caller_properties_and_handles_tracked_reauthentication() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let rumqttc_v5::Packet::Connect(connect, _, _) = read_packet(&mut socket) else {
            panic!("CONNECT")
        };
        assert_eq!(
            connect.properties.unwrap().authentication_data.as_deref(),
            Some(b"first".as_slice())
        );
        for reauth in [false, true] {
            if reauth {
                let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                    panic!("reauth")
                };
                assert_eq!(auth.code, rumqttc_v5::AuthReasonCode::ReAuthenticate);
                assert_eq!(
                    auth.properties.unwrap().data.as_deref(),
                    Some(b"first".as_slice())
                );
            }
            send_auth(
                &mut socket,
                rumqttc_v5::AuthReasonCode::Continue,
                b"challenge",
            );
            let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                panic!("challenge response")
            };
            assert_eq!(
                auth.properties.unwrap().data.as_deref(),
                Some(b"response".as_slice())
            );
            if reauth {
                send_auth(&mut socket, rumqttc_v5::AuthReasonCode::Success, b"final");
            } else {
                // Successful CONNACK repeats the negotiated authentication method.
                socket
                    .write_all(&[0x20, 10, 0, 0, 7, 0x15, 0, 4, b't', b'e', b's', b't'])
                    .unwrap();
            }
        }
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Disconnect(_)
        ));
    });
    let mechanism = Arc::new(Mechanism {
        contexts: Mutex::new(Vec::new()),
    });
    let mut client = support::start(config(port, mechanism.clone())).unwrap();
    let mut events = client.take_events().unwrap();
    loop {
        if matches!(
            events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Some(WrapperEvent::Connected { .. })
        ) {
            break;
        }
    }
    let error = client
        .handle()
        .try_admit(Command::Reauthenticate(Some(AuthProperties {
            data: Some(Bytes::from_static(b"caller-owned")),
            ..Default::default()
        })))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Admission);
    assert_eq!(error.code(), ErrorCode::CommandInvalid);
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    let operation = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    assert_eq!(
        operation
            .completion
            .wait_timeout(Duration::from_secs(3))
            .unwrap(),
        Completion::Authenticated
    );
    client
        .handle()
        .try_admit(Command::ImmediateDisconnect)
        .unwrap();
    client.join(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
    let contexts = mechanism.contexts.lock().unwrap();
    assert!(
        contexts
            .iter()
            .all(|context| context.generation == 1 && context.method == "test")
    );
    assert!(
        contexts
            .iter()
            .any(|context| context.exchange == AuthExchange::Reauthentication)
    );
    drop(contexts);
}

#[test]
fn enhanced_authentication_deadline_terminates_stalled_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let _ = read_packet(&mut socket);
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    let mut config = config(
        port,
        Arc::new(Mechanism {
            contexts: Mutex::new(Vec::new()),
        }),
    );
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.authenticator.as_mut().unwrap().exchange_timeout = Duration::from_millis(50);
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    loop {
        if let Some(WrapperEvent::DriverTerminated(error)) =
            events.recv_timeout(Duration::from_secs(2)).unwrap()
        {
            assert_eq!(error.auth_failure(), Some(AuthFailure::Timeout));
            break;
        }
    }
    client.join(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn tracked_reauthentication_retains_broker_rejection_code() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        read_packet(&mut socket);
        socket
            .write_all(&[0x20, 10, 0, 0, 7, 0x15, 0, 4, b't', b'e', b's', b't'])
            .unwrap();
        assert!(matches!(
            read_packet(&mut socket),
            rumqttc_v5::Packet::Auth(_)
        ));
        let mut frame = BytesMut::new();
        rumqttc_v5::Disconnect::new(rumqttc_v5::DisconnectReasonCode::NotAuthorized)
            .write(&mut frame)
            .unwrap();
        socket.write_all(&frame).unwrap();
    });
    let mut native = support::start(config(
        port,
        Arc::new(Mechanism {
            contexts: Mutex::new(vec![]),
        }),
    ))
    .unwrap();
    let mut events = native.take_events().unwrap();
    while !matches!(
        events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap(),
        WrapperEvent::Connected { .. }
    ) {}
    let operation = native
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    let error = operation
        .completion
        .wait_timeout(Duration::from_secs(3))
        .unwrap_err();
    assert_eq!(error.auth_failure(), Some(AuthFailure::BrokerRejected));
    assert_eq!(error.broker_reason(), Some(0x87));
    native.closer().close_now(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
}

#[test]
fn authenticator_panics_are_terminal_redacted_and_release_driver_ownership() {
    struct Panicking;
    impl Authenticator for Panicking {
        fn respond(
            &self,
            _: AuthContext,
            _: AuthChallenge,
        ) -> std::result::Result<AuthAction, AuthFailure> {
            panic!("secret-authentication-value");
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    let owner = Arc::new(Panicking);
    let mut native = support::start(config(port, owner.clone())).unwrap();
    let mut events = native.take_events().unwrap();
    loop {
        if let WrapperEvent::DriverTerminated(error) = events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap()
        {
            assert_eq!(error.auth_failure(), Some(AuthFailure::Panic));
            assert!(!format!("{error:?}").contains("secret-authentication-value"));
            break;
        }
    }
    native.join(Duration::from_secs(3)).unwrap();
    assert_eq!(Arc::strong_count(&owner), 1);
    broker.join().unwrap();
}

const fn connack_properties(
    method: Option<String>,
    data: Option<Bytes>,
) -> rumqttc_v5::ConnAckProperties {
    rumqttc_v5::ConnAckProperties {
        session_expiry_interval: None,
        receive_max: None,
        max_qos: None,
        retain_available: None,
        max_packet_size: None,
        assigned_client_identifier: None,
        topic_alias_max: None,
        reason_string: None,
        user_properties: vec![],
        wildcard_subscription_available: None,
        subscription_identifiers_available: None,
        shared_subscription_available: None,
        server_keep_alive: None,
        response_information: None,
        server_reference: None,
        authentication_method: method,
        authentication_data: data,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn redirect_policy_preserves_reference_and_isolates_target_credentials() {
    struct Resolver(bool, Option<u16>);
    impl SrvResolver for Resolver {
        fn resolve(&self, owner: String) -> SrvFuture {
            assert_eq!(owner.trim_end_matches('.'), "_mqtt._tcp.service.invalid");
            let panic = self.0;
            let port = self.1;
            Box::pin(async move {
                assert!(!panic, "secret host panic payload");
                Ok(port
                    .into_iter()
                    .map(|port| SrvRecord {
                        priority: 10,
                        weight: 20,
                        port,
                        target: "localhost.".into(),
                    })
                    .collect())
            })
        }
    }
    for mode in [
        "disabled",
        "malformed",
        "loop",
        "follow",
        "srv-panic",
        "srv-empty",
        "srv-follow",
    ] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_port = target.local_addr().unwrap().port();
        let reference = match mode {
            "malformed" => "mqtt://user:password@host".into(),
            "loop" => format!("127.0.0.1:{origin_port}"),
            "srv-panic" | "srv-empty" | "srv-follow" => "_mqtt._tcp.service.invalid".into(),
            _ => format!("mqtt://127.0.0.1:{target_port}"),
        };
        let advertised = reference.clone();
        let broker = std::thread::spawn(move || {
            let (mut socket, _) = origin.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            read_packet(&mut socket);
            let mut p = connack_properties(None, None);
            p.server_reference = Some(reference);
            let mut frame = BytesMut::new();
            rumqttc_v5::ConnAck {
                session_present: false,
                code: rumqttc_v5::ConnectReturnCode::ServerMoved,
                properties: Some(p),
            }
            .write(&mut frame)
            .unwrap();
            socket.write_all(&frame).unwrap();
            if matches!(mode, "follow" | "srv-follow") {
                let (mut socket, _) = target.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let rumqttc_v5::Packet::Connect(connect, _, auth) = read_packet(&mut socket) else {
                    panic!("CONNECT")
                };
                assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
                assert_ne!(connect.client_id, "origin-client");
                assert!(connect.clean_start);
                let mut p = connack_properties(None, None);
                if connect.client_id.is_empty() {
                    p.assigned_client_identifier = Some("target-client".into());
                }
                frame.clear();
                rumqttc_v5::ConnAck {
                    session_present: false,
                    code: rumqttc_v5::ConnectReturnCode::Success,
                    properties: Some(p),
                }
                .write(&mut frame)
                .unwrap();
                socket.write_all(&frame).unwrap();
                assert!(matches!(
                    read_packet(&mut socket),
                    rumqttc_v5::Packet::Disconnect(_)
                ));
            }
        });

        let mut config = ClientConfig::v5("origin-client", "127.0.0.1", origin_port);
        config.common.username = Some("secret-user".into());
        config.common.password = Some(Bytes::from_static(b"secret-password"));
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        if mode != "disabled" {
            v5.redirect_policy = RedirectPolicy::Follow {
                max_attempts: 2,
                transport: TransportConfig::Tcp,
            };
        }
        if mode.starts_with("srv-") {
            v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Resolver(
                mode == "srv-panic",
                (mode == "srv-follow").then_some(target_port),
            ))));
        }
        let mut native = support::start(config).unwrap();
        let mut events = native.take_events().unwrap();
        let mut seen_redirect = false;
        let mut seen_srv_target = false;
        loop {
            match events
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
            {
                WrapperEvent::Redirect(event) => {
                    seen_redirect = true;
                    assert_eq!(event.source, RedirectSource::ConnAck);
                    assert_eq!(event.reason, RedirectReason::ServerMoved);
                    assert_eq!(event.server_reference.as_deref(), Some(advertised.as_str()));
                    if mode == "follow" {
                        assert_eq!(
                            event.target,
                            Some(BrokerTarget::Tcp {
                                host: "127.0.0.1".into(),
                                port: target_port
                            })
                        );
                    }
                    if mode == "srv-follow" && event.target.is_some() {
                        assert_eq!(
                            event.target,
                            Some(BrokerTarget::Tcp {
                                host: "localhost".into(),
                                port: target_port
                            })
                        );
                        seen_srv_target = true;
                    } else if mode.starts_with("srv-") {
                        assert_eq!(event.target, None);
                    }
                }
                WrapperEvent::Connected { .. } => {
                    assert!(matches!(mode, "follow" | "srv-follow"));
                    assert_eq!(seen_srv_target, mode == "srv-follow");
                    break;
                }
                WrapperEvent::DriverTerminated(error) => {
                    let expected = match mode {
                        "disabled" => RedirectFailure::Disabled,
                        "malformed" => RedirectFailure::InvalidReference,
                        "loop" => RedirectFailure::Loop,
                        "srv-panic" => RedirectFailure::Callback(SrvFailure::Panic),
                        "srv-empty" => RedirectFailure::Dns,
                        _ => panic!("unexpected termination: {error:?}"),
                    };
                    assert_eq!(error.redirect_failure(), Some(expected), "mode={mode}");
                    break;
                }
                _ => {}
            }
        }
        assert!(seen_redirect);
        native.closer().close_now(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn isolated_redirect_restores_origin_authentication_and_session_expiry_admission() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin_port = origin.local_addr().unwrap().port();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    let (target_ready_tx, target_ready_rx) = std::sync::mpsc::sync_channel(0);
    let (allow_target_connack_tx, allow_target_connack_rx) = std::sync::mpsc::sync_channel(0);
    let (disconnect_target_tx, disconnect_target_rx) = std::sync::mpsc::channel();

    let origin_broker = std::thread::spawn(move || {
        let (mut first, _) = origin.accept().unwrap();
        first
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert!(matches!(
            read_packet(&mut first),
            rumqttc_v5::Packet::Connect(_, _, _)
        ));
        let mut properties = connack_properties(Some("test".into()), None);
        properties.server_reference = Some(format!("mqtt://127.0.0.1:{target_port}"));
        let mut frame = BytesMut::new();
        rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::UseAnotherServer,
            properties: Some(properties),
        }
        .write(&mut frame)
        .unwrap();
        first.write_all(&frame).unwrap();

        let (mut restored, _) = origin.accept().unwrap();
        restored
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let rumqttc_v5::Packet::Connect(connect, _, _) = read_packet(&mut restored) else {
            panic!("restored origin CONNECT")
        };
        assert_eq!(connect.client_id, "auth");
        assert_eq!(
            connect.properties.unwrap().session_expiry_interval,
            Some(60)
        );
        frame.clear();
        rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::Success,
            properties: Some(connack_properties(Some("test".into()), None)),
        }
        .write(&mut frame)
        .unwrap();
        restored.write_all(&frame).unwrap();

        let rumqttc_v5::Packet::Auth(reauth) = read_packet(&mut restored) else {
            panic!("restored origin reauthentication")
        };
        assert_eq!(reauth.code, rumqttc_v5::AuthReasonCode::ReAuthenticate);
        send_auth(
            &mut restored,
            rumqttc_v5::AuthReasonCode::Continue,
            b"challenge",
        );
        assert!(matches!(
            read_packet(&mut restored),
            rumqttc_v5::Packet::Auth(_)
        ));
        send_auth(&mut restored, rumqttc_v5::AuthReasonCode::Success, b"final");
        let rumqttc_v5::Packet::Disconnect(disconnect) = read_packet(&mut restored) else {
            panic!("restored origin DISCONNECT")
        };
        assert_eq!(
            disconnect.properties.unwrap().session_expiry_interval,
            Some(1)
        );
    });

    let target_broker = std::thread::spawn(move || {
        let (mut socket, _) = target.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let rumqttc_v5::Packet::Connect(connect, _, auth) = read_packet(&mut socket) else {
            panic!("redirect target CONNECT")
        };
        assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
        assert!(connect.clean_start);
        assert_eq!(connect.properties.unwrap().session_expiry_interval, Some(0));
        target_ready_tx.send(()).unwrap();
        allow_target_connack_rx.recv().unwrap();
        let mut properties = connack_properties(None, None);
        if connect.client_id.is_empty() {
            properties.assigned_client_identifier = Some("target-client".into());
        }
        let mut frame = BytesMut::new();
        rumqttc_v5::ConnAck {
            session_present: false,
            code: rumqttc_v5::ConnectReturnCode::Success,
            properties: Some(properties),
        }
        .write(&mut frame)
        .unwrap();
        socket.write_all(&frame).unwrap();
        disconnect_target_rx.recv().unwrap();
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });

    let mechanism = Arc::new(Mechanism {
        contexts: Mutex::new(Vec::new()),
    });
    let mut config = config(origin_port, mechanism);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties.session_expiry_interval = Some(60);
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 1,
        transport: TransportConfig::Tcp,
    };
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    while !matches!(
        events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap(),
        WrapperEvent::Redirect(_)
    ) {}
    target_ready_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap();

    let disconnect = DisconnectProtocolOptions::V5(V5DisconnectOptions {
        session_expiry_interval: Some(1),
        ..Default::default()
    });
    let error = client
        .closer()
        .close_with_options(Duration::from_secs(1), disconnect.clone())
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Admission);
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);

    allow_target_connack_tx.send(()).unwrap();
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .expect("driver terminated before the origin reconnected");
        if matches!(event, WrapperEvent::Connected { .. }) {
            break;
        }
    }
    disconnect_target_tx.send(()).unwrap();
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .expect("driver terminated before the origin reconnected");
        if matches!(event, WrapperEvent::Connected { .. }) {
            break;
        }
    }

    let reauth = client
        .handle()
        .try_admit(Command::Reauthenticate(None))
        .unwrap();
    assert_eq!(
        reauth
            .completion
            .wait_timeout(Duration::from_secs(3))
            .unwrap(),
        Completion::Authenticated
    );
    assert_eq!(
        client
            .closer()
            .close_with_options(Duration::from_secs(3), disconnect)
            .unwrap(),
        Completion::GracefulShutdown
    );
    origin_broker.join().unwrap();
    target_broker.join().unwrap();
}

#[test]
fn rejected_connack_and_broker_disconnect_preserve_properties() {
    for rejected in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            read_packet(&mut socket);
            let mut p = connack_properties(None, None);
            p.reason_string = Some("peer text".into());
            p.user_properties = vec![("key".into(), "one".into()), ("key".into(), String::new())];
            let mut frame = BytesMut::new();
            rumqttc_v5::ConnAck {
                session_present: false,
                code: if rejected {
                    rumqttc_v5::ConnectReturnCode::NotAuthorized
                } else {
                    rumqttc_v5::ConnectReturnCode::Success
                },
                properties: Some(p),
            }
            .write(&mut frame)
            .unwrap();
            socket.write_all(&frame).unwrap();
            if !rejected {
                frame.clear();
                rumqttc_v5::Disconnect::new_with_properties(
                    rumqttc_v5::DisconnectReasonCode::ServerBusy,
                    rumqttc_v5::DisconnectProperties {
                        session_expiry_interval: None,
                        reason_string: Some(String::new()),
                        user_properties: vec![("key".into(), "value".into()); 2],
                        server_reference: Some(String::new()),
                    },
                )
                .write(&mut frame)
                .unwrap();
                socket.write_all(&frame).unwrap();
            }
        });
        let mut native = support::start(ClientConfig::v5("details", "127.0.0.1", port)).unwrap();
        let mut events = native.take_events().unwrap();
        let mut seen = false;
        for _ in 0..8 {
            match events
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
            {
                WrapperEvent::ConnectionRejected(details) => {
                    assert!(rejected);
                    assert_eq!(details.reason_code, 0x87);
                    let p = details.v5_properties.unwrap();
                    assert_eq!(p.reason_string.as_deref(), Some("peer text"));
                    assert_eq!(
                        p.user_properties,
                        vec![("key".into(), "one".into()), ("key".into(), String::new())]
                    );
                    seen = true;
                }
                WrapperEvent::BrokerDisconnect(p) => {
                    assert!(!rejected);
                    assert_eq!(p.reason_code, 0x89);
                    assert_eq!(p.reason_string.as_deref(), Some(""));
                    assert_eq!(p.server_reference.as_deref(), Some(""));
                    assert_eq!(p.user_properties, vec![("key".into(), "value".into()); 2]);
                    seen = true;
                }
                WrapperEvent::Disconnected { error, .. } if seen => {
                    assert_eq!(
                        error.broker_reason(),
                        Some(if rejected { 0x87 } else { 0x89 })
                    );
                    assert!(!format!("{error:?}").contains("peer text"));
                    break;
                }
                _ => {}
            }
        }
        assert!(seen);
        native.closer().close_now(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
    }
}

#[cfg(feature = "auth-scram")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn scram_verifies_server_proof_for_initial_authentication_and_reauthentication() {
    use scram::{
        SCRAM_TYPES, ScramAuthServer, ScramCbHelper, ScramHashing, ScramNonce, ScramPassword,
        ScramServerDyn, ScramSha256RustNative, scram_sync::SyncScramServer,
    };
    use std::fmt::Write as _;
    #[derive(Debug, Clone, Copy)]
    struct Credentials;
    impl ScramCbHelper for Credentials {}
    impl ScramAuthServer<ScramSha256RustNative> for Credentials {
        fn get_password_for_user(
            &self,
            username: &str,
            _: Option<&str>,
        ) -> scram::ScramResult<ScramPassword> {
            assert_eq!(username, "scram-private-username");
            let iterations = std::num::NonZeroU32::new(4096).unwrap();
            let mut hash = vec![0; 32];
            ScramSha256RustNative::derive(
                b"scram-private-password",
                b"fixed-salt",
                iterations,
                &mut hash,
            )?;
            Ok(ScramPassword::found_secret_password(
                hash,
                scram::base64_encode_block(b"fixed-salt"),
                iterations,
                None,
            ))
        }
    }
    support::capture::start();
    for invalid_signature in [false, true] {
        let secrets = Arc::new(Mutex::new(vec![
            "scram-private-username".to_owned(),
            "scram-private-password".to_owned(),
            "fixedServerNonce".to_owned(),
        ]));
        let captured = secrets.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let rumqttc_v5::Packet::Connect(connect, _, _) = read_packet(&mut socket) else {
                panic!("CONNECT")
            };
            let mut first = connect.properties.unwrap().authentication_data.unwrap();
            for reauth in [false, true] {
                if reauth {
                    let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                        panic!("AUTH")
                    };
                    first = auth.properties.unwrap().data.unwrap();
                }
                let mut server = SyncScramServer::<ScramSha256RustNative, _, _>::new(
                    Credentials,
                    Credentials,
                    ScramNonce::base64("fixedServerNonce").unwrap(),
                    SCRAM_TYPES.get_scramtype("SCRAM-SHA-256").unwrap(),
                    false,
                )
                .unwrap();
                let response = server.parse_response(std::str::from_utf8(&first).unwrap());
                assert!(response.is_ok());
                for data in [
                    std::str::from_utf8(&first).unwrap(),
                    response.get_raw_output(),
                ] {
                    captured.lock().unwrap().push(data.to_owned());
                    for field in data.split(',') {
                        if let Some(value) = field.strip_prefix("r=") {
                            captured.lock().unwrap().push(value.to_owned());
                        }
                    }
                }
                let mut frame = BytesMut::new();
                rumqttc_v5::Auth::new(
                    rumqttc_v5::AuthReasonCode::Continue,
                    Some(rumqttc_v5::AuthProperties {
                        method: Some("SCRAM-SHA-256".into()),
                        data: Some(Bytes::copy_from_slice(response.get_raw_output().as_bytes())),
                        reason: None,
                        user_properties: vec![],
                    }),
                )
                .write(&mut frame)
                .unwrap();
                socket.write_all(&frame).unwrap();
                let rumqttc_v5::Packet::Auth(auth) = read_packet(&mut socket) else {
                    panic!("AUTH response")
                };
                let data = auth.properties.unwrap().data.unwrap();
                let data = std::str::from_utf8(&data).unwrap();
                captured.lock().unwrap().push(data.to_owned());
                for field in data.split(',') {
                    if let Some(proof) = field.strip_prefix("p=") {
                        captured.lock().unwrap().push(proof.to_owned());
                    }
                }
                let response = server.parse_response(data);
                assert!(response.is_ok());
                let proof = if invalid_signature {
                    "v=AAAA"
                } else {
                    response.get_raw_output()
                };
                captured.lock().unwrap().push(proof.to_owned());
                if let Some(proof) = proof.strip_prefix("v=") {
                    captured.lock().unwrap().push(proof.to_owned());
                }
                frame.clear();
                if reauth {
                    rumqttc_v5::Auth::new(
                        rumqttc_v5::AuthReasonCode::Success,
                        Some(rumqttc_v5::AuthProperties {
                            method: Some("SCRAM-SHA-256".into()),
                            data: Some(Bytes::copy_from_slice(proof.as_bytes())),
                            reason: None,
                            user_properties: vec![],
                        }),
                    )
                    .write(&mut frame)
                    .unwrap();
                } else {
                    rumqttc_v5::ConnAck {
                        session_present: false,
                        code: rumqttc_v5::ConnectReturnCode::Success,
                        properties: Some(connack_properties(
                            Some("SCRAM-SHA-256".into()),
                            Some(Bytes::copy_from_slice(proof.as_bytes())),
                        )),
                    }
                    .write(&mut frame)
                    .unwrap();
                }
                socket.write_all(&frame).unwrap();
                if invalid_signature {
                    let mut byte = [0];
                    // The driver may send a protocol-error disconnect before closing.
                    while socket.read(&mut byte).unwrap() != 0 {}
                    return;
                }
            }
            assert!(matches!(
                read_packet(&mut socket),
                rumqttc_v5::Packet::Disconnect(_)
            ));
        });
        let mut config = ClientConfig::v5("scram", "127.0.0.1", port);
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.connect_properties.authentication_method = Some("SCRAM-SHA-256".into());
        v5.scram = Some(ScramConfig::new(
            "scram-private-username",
            SecretBytes::new(b"scram-private-password".to_vec()),
        ));
        let mut formatted = format!("{config:?}");
        let mut native = support::start(config).unwrap();
        let mut events = native.take_events().unwrap();
        loop {
            let event = events
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            write!(formatted, "{event:?}").unwrap();
            match event {
                WrapperEvent::Connected { .. } => {
                    assert!(!invalid_signature);
                    break;
                }
                WrapperEvent::DriverTerminated(error) => {
                    formatted.push_str(&error.to_string());
                    assert!(invalid_signature);
                    assert_eq!(error.auth_failure(), Some(AuthFailure::Rejected));
                    break;
                }
                _ => {}
            }
        }
        if !invalid_signature {
            let reauth = native
                .handle()
                .try_admit(Command::Reauthenticate(None))
                .unwrap();
            assert_eq!(
                reauth
                    .completion
                    .wait_timeout(Duration::from_secs(3))
                    .unwrap(),
                Completion::Authenticated
            );
            native.closer().close_now(Duration::from_secs(3)).unwrap();
        }
        native.join(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
        let secrets = secrets.lock().unwrap();
        support::capture::assert_redacted(
            &formatted,
            &secrets.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        drop(secrets);
        support::capture::assert_activity();
    }
}

#[test]
fn async_failure_callback_distinguishes_broker_refusal_from_transport_loss() {
    struct Authority(std::sync::mpsc::Sender<AuthFailure>);
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AuthContext, _: AsyncAuthChallenge) -> AuthFuture {
            Box::pin(async { Ok(AuthAction::Complete) })
        }
        fn failure(&self, _: AuthContext, failure: AuthFailure) {
            self.0.send(failure).unwrap();
        }
    }
    #[derive(Clone, Copy, Debug)]
    enum Outcome {
        TransportLoss,
        Connack(u8),
        Disconnect,
    }
    for outcome in [
        Outcome::TransportLoss,
        Outcome::Connack(0x86),
        Outcome::Connack(0x87),
        Outcome::Connack(0x8c),
        Outcome::Disconnect,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = support::Broker::spawn(move || {
            let mut socket = support::accept(&listener);
            read_packet(&mut socket);
            match outcome {
                Outcome::TransportLoss => return,
                Outcome::Connack(reason) => {
                    socket.write_all(&[0x20, 3, 0, reason, 0]).unwrap();
                }
                Outcome::Disconnect => {
                    socket
                        .write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                        .unwrap();
                    assert!(matches!(
                        read_packet(&mut socket),
                        rumqttc_v5::Packet::Auth(_)
                    ));
                    socket.write_all(b"\xe0\x02\x87\x00").unwrap();
                }
            }
            assert_eq!(socket.read(&mut [0]).unwrap(), 0);
        });
        let (failed_tx, failed_rx) = std::sync::mpsc::channel();
        let mut client = support::start(async_config(
            port,
            Arc::new(Authority(failed_tx)),
            Duration::from_secs(30),
        ))
        .unwrap();
        let mut events = client.take_events().unwrap();
        let operation = if matches!(outcome, Outcome::Disconnect) {
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            Some(
                client
                    .handle()
                    .try_admit(Command::Reauthenticate(None))
                    .unwrap(),
            )
        } else {
            None
        };
        assert_eq!(
            failed_rx.recv_timeout(support::DEADLINE).unwrap(),
            if matches!(outcome, Outcome::TransportLoss) {
                AuthFailure::ConnectionClosed
            } else {
                AuthFailure::BrokerRejected
            },
            "outcome={outcome:?}"
        );
        if let Some(operation) = operation {
            let error = support::terminal(&operation).unwrap_err();
            assert_eq!(error.auth_failure(), Some(AuthFailure::BrokerRejected));
            assert_eq!(error.broker_reason(), Some(0x87));
        }
        client.closer().close_now(support::DEADLINE).unwrap();
        broker.join();
    }
}
