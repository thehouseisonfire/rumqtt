use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use rumqttc_wrapper_core::*;

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

#[test]
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
    let mut client = NativeClient::start(config(port, mechanism.clone())).unwrap();
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
    let mut client = NativeClient::start(config).unwrap();
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
    let mut native = NativeClient::start(config(
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
    let mut native = NativeClient::start(config(port, owner.clone())).unwrap();
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
fn redirect_policy_preserves_reference_and_isolates_target_credentials() {
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
        let mut native = NativeClient::start(config).unwrap();
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
    let mut client = NativeClient::start(config).unwrap();
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
        let mut native =
            NativeClient::start(ClientConfig::v5("details", "127.0.0.1", port)).unwrap();
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
fn scram_verifies_server_proof_for_initial_authentication_and_reauthentication() {
    use scram::{
        SCRAM_TYPES, ScramAuthServer, ScramCbHelper, ScramHashing, ScramNonce, ScramPassword,
        ScramServerDyn, ScramSha256RustNative, scram_sync::SyncScramServer,
    };
    #[derive(Debug, Clone, Copy)]
    struct Credentials;
    impl ScramCbHelper for Credentials {}
    impl ScramAuthServer<ScramSha256RustNative> for Credentials {
        fn get_password_for_user(
            &self,
            username: &str,
            _: Option<&str>,
        ) -> scram::ScramResult<ScramPassword> {
            assert_eq!(username, "user");
            let iterations = std::num::NonZeroU32::new(4096).unwrap();
            let mut hash = vec![0; 32];
            ScramSha256RustNative::derive(b"pencil", b"fixed-salt", iterations, &mut hash)?;
            Ok(ScramPassword::found_secret_password(
                hash,
                scram::base64_encode_block(b"fixed-salt"),
                iterations,
                None,
            ))
        }
    }
    for invalid_signature in [false, true] {
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
                let response = server.parse_response(
                    std::str::from_utf8(&auth.properties.unwrap().data.unwrap()).unwrap(),
                );
                assert!(response.is_ok());
                let proof = if invalid_signature {
                    "v=AAAA"
                } else {
                    response.get_raw_output()
                };
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
            "user",
            SecretBytes::new(b"pencil".to_vec()),
        ));
        assert!(!format!("{config:?}").contains("pencil"));
        let mut native = NativeClient::start(config).unwrap();
        let mut events = native.take_events().unwrap();
        loop {
            match events
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
            {
                WrapperEvent::Connected { .. } => {
                    assert!(!invalid_signature);
                    break;
                }
                WrapperEvent::DriverTerminated(error) => {
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
    }
}
