mod support;
#[path = "support/tls.rs"]
mod tls;

use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use bytes::{Bytes, BytesMut};
use rumqttc_wrapper_core::*;
use support::*;

fn redirect(socket: &mut impl Write, reference: &str, disconnect: bool) {
    redirect_with_reason(
        socket,
        reference,
        disconnect,
        RedirectReason::UseAnotherServer,
    );
}

fn redirect_with_reason(
    socket: &mut impl Write,
    reference: &str,
    disconnect: bool,
    reason: RedirectReason,
) {
    let reason = match reason {
        RedirectReason::UseAnotherServer => 0x9c,
        RedirectReason::ServerMoved => 0x9d,
    };
    let mut properties = vec![0x1c];
    properties.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
    properties.extend_from_slice(reference.as_bytes());
    let mut body = if disconnect {
        vec![reason]
    } else {
        vec![0, reason]
    };
    body.push(u8::try_from(properties.len()).unwrap());
    body.extend(properties);
    assert!(body.len() < 128);
    socket
        .write_all(&[
            if disconnect { 0xe0 } else { 0x20 },
            u8::try_from(body.len()).unwrap(),
        ])
        .unwrap();
    socket.write_all(&body).unwrap();
    socket.flush().unwrap();
}

const TARGET_CONNACK: &[u8] = b"\x20\x0c\x00\x00\x09\x12\x00\x06target";

#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback error type"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn redirect_reference_forms_select_isolated_endpoints_for_both_sources() {
    let tls = tls::Fixture::new();
    for disconnect in [false, true] {
        for scheme in ["authority", "mqtt", "mqtts", "ws", "wss"] {
            let encrypted = scheme == "mqtts" || scheme == "wss";
            let websocket = scheme == "ws" || scheme == "wss";
            if encrypted
                && !cfg!(any(
                    feature = "use-rustls-no-provider",
                    feature = "use-native-tls"
                ))
            {
                continue;
            }
            if websocket && !cfg!(feature = "websocket") {
                continue;
            }
            for ipv6 in [false, true] {
                let origin = TcpListener::bind("127.0.0.1:0").unwrap();
                let target =
                    TcpListener::bind(if ipv6 { "[::1]:0" } else { "127.0.0.1:0" }).unwrap();
                let target_port = target.local_addr().unwrap().port();
                let host = if ipv6 { "[::1]" } else { "127.0.0.1" };
                let reference = if scheme == "authority" {
                    format!("{host}:{target_port}")
                } else {
                    format!(
                        "{scheme}://{host}:{target_port}{}",
                        if websocket { "/mqtt?redirect=one" } else { "" }
                    )
                };
                let advertised = reference.clone();
                let mut config = config(true, origin.local_addr().unwrap().port());
                let backend = if cfg!(feature = "use-rustls-no-provider") {
                    TlsBackend::Rustls
                } else {
                    TlsBackend::Native
                };
                let transport = if encrypted {
                    if websocket {
                        TransportConfig::Wss(tls.client(backend))
                    } else {
                        TransportConfig::Tls(tls.client(backend))
                    }
                } else if websocket {
                    TransportConfig::WebSocket
                } else {
                    TransportConfig::Tcp
                };
                let ProtocolConfig::V5(v5) = &mut config.protocol else {
                    unreachable!()
                };
                v5.redirect_policy = RedirectPolicy::Follow {
                    max_attempts: 2,
                    transport,
                };
                let server = tls.server.clone();
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let broker = Broker::spawn(move || {
                    let mut socket = accept(&origin);
                    frame(&mut socket);
                    if disconnect {
                        socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
                        publish_id(&mut socket, true);
                    }
                    ready_tx.send(()).unwrap();
                    release_rx.recv_timeout(DEADLINE).unwrap();
                    redirect(&mut socket, &reference, disconnect);
                    let mut stream: Box<dyn tls::Duplex> = Box::new(accept(&target));
                    if encrypted {
                        stream = tls::wrap(stream, server);
                    }
                    if websocket {
                        let mut socket = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, mut response: tungstenite::handshake::server::Response| {
                            assert_eq!(request.uri().path_and_query().unwrap().as_str(), "/mqtt?redirect=one");
                            assert!(!request.headers().contains_key("authorization"));
                            response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                            Ok(response)
                        }).unwrap();
                        let mut packet =
                            BytesMut::from(socket.read().unwrap().into_data().as_ref());
                        assert_isolated(&mut packet);
                        socket
                            .send(tungstenite::Message::Binary(Bytes::from_static(
                                TARGET_CONNACK,
                            )))
                            .unwrap();
                        assert_eq!(socket.read().unwrap().into_data()[0], 0xe0);
                    } else {
                        assert_isolated(&mut frame(&mut stream));
                        stream.write_all(TARGET_CONNACK).unwrap();
                        stream.flush().unwrap();
                        assert_eq!(frame(&mut stream)[0], 0xe0);
                    }
                });
                let mut client = NativeClient::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                let pending = if disconnect {
                    until(&mut events, |event| {
                        matches!(event, WrapperEvent::Connected { .. })
                    });
                    publish(&client, b"pending redirect")
                } else {
                    client
                        .handle()
                        .try_admit(Command::Publish(PublishCommand {
                            topic: "pending".into(),
                            payload: Bytes::new(),
                            qos: QoS::AtMostOnce,
                            retain: false,
                            protocol: PublishProtocolOptions::VersionNeutral,
                        }))
                        .unwrap()
                };
                ready_rx.recv_timeout(DEADLINE).unwrap();
                release_tx.send(()).unwrap();
                let event = until(&mut events, |event| {
                    matches!(event, WrapperEvent::Redirect(_))
                });
                let WrapperEvent::Redirect(event) = event else {
                    unreachable!()
                };
                assert_eq!(
                    event.source,
                    if disconnect {
                        RedirectSource::Disconnect
                    } else {
                        RedirectSource::ConnAck
                    }
                );
                assert_eq!(event.reason, RedirectReason::UseAnotherServer);
                assert_eq!(event.server_reference.as_deref(), Some(advertised.as_str()));
                assert_eq!(event.failure, None);
                let expected = if websocket {
                    BrokerTarget::WebSocket { url: advertised }
                } else {
                    BrokerTarget::Tcp {
                        host: if ipv6 { "::1" } else { "127.0.0.1" }.into(),
                        port: target_port,
                    }
                };
                assert_eq!(event.target, Some(expected));
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                let error = terminal(&pending).unwrap_err();
                assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
                assert_eq!(error.context().generation, disconnect.then_some(1));
                client.closer().close(DEADLINE).unwrap();
                broker.join();
            }
        }
    }
}

fn assert_isolated(packet: &mut BytesMut) {
    let rumqttc_v5::Packet::Connect(connect, _, auth) =
        rumqttc_v5::Packet::read(packet, None).unwrap()
    else {
        panic!("CONNECT")
    };
    assert!(connect.clean_start);
    assert_ne!(connect.client_id, "parity");
    assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
}

#[test]
fn rejected_redirects_preserve_context_and_resolve_pending_operations() {
    for disconnect in [false, true] {
        for reference in [
            "mqtt://user:secret@host",
            "ftp://host",
            "mqtt://host:0",
            "[broken",
            "mqtt://host/path",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = config(true, listener.local_addr().unwrap().port());
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                unreachable!()
            };
            v5.redirect_policy = RedirectPolicy::Follow {
                max_attempts: 1,
                transport: TransportConfig::Tcp,
            };
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                frame(&mut socket);
                if disconnect {
                    socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
                    publish_id(&mut socket, true);
                }
                release_rx.recv_timeout(DEADLINE).unwrap();
                redirect(&mut socket, reference, disconnect);
            });
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let operation = if disconnect {
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                publish(&client, b"pending")
            } else {
                client
                    .handle()
                    .try_admit(Command::Publish(PublishCommand {
                        topic: "pending".into(),
                        payload: Bytes::new(),
                        qos: QoS::AtMostOnce,
                        retain: false,
                        protocol: PublishProtocolOptions::VersionNeutral,
                    }))
                    .unwrap()
            };
            release_tx.send(()).unwrap();
            let event = until(&mut events, |event| {
                matches!(event, WrapperEvent::Redirect(_))
            });
            let WrapperEvent::Redirect(event) = event else {
                unreachable!()
            };
            assert_eq!(
                event.source,
                if disconnect {
                    RedirectSource::Disconnect
                } else {
                    RedirectSource::ConnAck
                }
            );
            assert_eq!(event.reason, RedirectReason::UseAnotherServer);
            assert_eq!(event.server_reference.as_deref(), Some(reference));
            assert_eq!(event.target, None);
            assert_eq!(event.failure, Some(RedirectFailure::InvalidReference));
            let event = until(&mut events, |event| {
                matches!(event, WrapperEvent::DriverTerminated(_))
            });
            let WrapperEvent::DriverTerminated(error) = event else {
                unreachable!()
            };
            assert_eq!(
                error.redirect_failure(),
                Some(RedirectFailure::InvalidReference)
            );
            assert_eq!(error.context().generation, disconnect.then_some(1));
            client.join(DEADLINE).unwrap();
            assert_eq!(
                terminal(&operation).unwrap_err().delivery_status(),
                DeliveryStatus::Ambiguous
            );
            broker.join();
        }
    }
}

type SrvReply = tokio::sync::oneshot::Receiver<std::result::Result<Vec<SrvRecord>, SrvFailure>>;

fn pending_before_connack(client: &NativeClient) -> Admission {
    client
        .handle()
        .try_admit(Command::Publish(PublishCommand {
            topic: "pending".into(),
            payload: Bytes::new(),
            qos: QoS::AtMostOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap()
}

fn assert_connack_redirect(
    event: &RedirectEvent,
    reference: &str,
    failure: Option<RedirectFailure>,
) {
    assert_eq!(event.source, RedirectSource::ConnAck);
    assert_eq!(event.reason, RedirectReason::UseAnotherServer);
    assert_eq!(event.server_reference.as_deref(), Some(reference));
    assert_eq!(event.target, None);
    assert_eq!(event.failure, failure);
}

struct Resolver {
    result: Mutex<Option<SrvReply>>,
    entered: std::sync::mpsc::Sender<()>,
}
impl SrvResolver for Resolver {
    fn resolve(&self, owner: String) -> SrvFuture {
        assert_eq!(owner.trim_end_matches('.'), "_mqtt._tcp.service.invalid");
        let result = self.result.lock().unwrap().take().unwrap();
        self.entered.send(()).unwrap();
        Box::pin(async { result.await.unwrap() })
    }
}

#[test]
fn failed_followed_srv_connection_retains_terminal_redirect_diagnostics() {
    struct OneTargetResolver(u16);
    impl SrvResolver for OneTargetResolver {
        fn resolve(&self, owner: String) -> SrvFuture {
            assert_eq!(owner.trim_end_matches('.'), "_mqtt._tcp.service.invalid");
            let port = self.0;
            Box::pin(async move {
                Ok(vec![SrvRecord {
                    priority: 0,
                    weight: 0,
                    port,
                    target: "localhost.".into(),
                }])
            })
        }
    }

    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = config(true, origin.local_addr().unwrap().port());
    // localhost may resolve to IPv6, where another fixture can own the same port.
    config.common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 2,
        transport: TransportConfig::Tcp,
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(OneTargetResolver(
        target.local_addr().unwrap().port(),
    ))));
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect(&mut socket, "_mqtt._tcp.service.invalid", false);
        let mut socket = accept(&target);
        frame(&mut socket);
    });

    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let initial = until(&mut events, |event| {
        matches!(event, WrapperEvent::Redirect(_))
    });
    let WrapperEvent::Redirect(initial) = initial else {
        unreachable!()
    };
    assert_eq!(initial.failure, None);

    let terminal = until(
        &mut events,
        |event| matches!(event, WrapperEvent::Redirect(redirect) if redirect.failure.is_some()),
    );
    let WrapperEvent::Redirect(terminal) = terminal else {
        unreachable!()
    };
    assert_eq!(terminal.failure, Some(RedirectFailure::Transport));
    assert!(terminal.followed);
    assert_eq!(terminal.attempts, 1);
    assert_eq!(terminal.attempt_limit, Some(2));
    assert_eq!(terminal.visited_endpoints, 2);
    assert_eq!(terminal.srv_candidate_index, Some(1));
    assert_eq!(terminal.srv_candidate_count, Some(1));
    client.join(DEADLINE).unwrap();
    broker.join();
}

#[test]
fn srv_lookup_failure_empty_answers_and_cancellation_release_owner() {
    for mode in ["failure", "empty", "cancel"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(true, listener.local_addr().unwrap().port());
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let resolver = Arc::new(Resolver {
            result: Mutex::new(Some(result_rx)),
            entered: entered_tx,
        });
        let weak = Arc::downgrade(&resolver);
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 2,
            transport: TransportConfig::Tcp,
        };
        v5.srv_resolver = Some(SrvResolverConfig(resolver));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            frame(&mut socket);
            release_rx.recv_timeout(DEADLINE).unwrap();
            redirect(&mut socket, "_mqtt._tcp.service.invalid", false);
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let operation = pending_before_connack(&client);
        release_tx.send(()).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        let WrapperEvent::Redirect(event) = until(&mut events, |event| {
            matches!(event, WrapperEvent::Redirect(_))
        }) else {
            unreachable!()
        };
        assert_connack_redirect(&event, "_mqtt._tcp.service.invalid", None);
        if mode == "cancel" {
            client.closer().close_now(DEADLINE).unwrap();
            assert!(result_tx.send(Ok(vec![])).is_err());
        } else {
            result_tx
                .send(if mode == "failure" {
                    Err(SrvFailure::Query)
                } else {
                    Ok(vec![])
                })
                .unwrap();
            let failure = if mode == "failure" {
                RedirectFailure::Callback(SrvFailure::Query)
            } else {
                RedirectFailure::Dns
            };
            let WrapperEvent::Redirect(event) = until(&mut events, |event| {
                matches!(event, WrapperEvent::Redirect(_))
            }) else {
                unreachable!()
            };
            assert_connack_redirect(&event, "_mqtt._tcp.service.invalid", Some(failure));
            let event = until(&mut events, |event| {
                matches!(event, WrapperEvent::DriverTerminated(_))
            });
            let WrapperEvent::DriverTerminated(error) = event else {
                unreachable!()
            };
            assert_eq!(error.redirect_failure(), Some(failure));
            assert_eq!(error.context().generation, None);
            client.join(DEADLINE).unwrap();
        }
        let error = terminal(&operation).unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert_eq!(error.context().generation, None);
        assert!(weak.upgrade().is_none());
        broker.join();
    }
}

#[test]
fn redirect_loops_and_attempt_exhaustion_are_terminal() {
    for loop_back in [false, true] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_port = origin.local_addr().unwrap().port();
        let target_port = target.local_addr().unwrap().port();
        let mut config = config(true, origin_port);
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: if loop_back { 2 } else { 1 },
            transport: TransportConfig::Tcp,
        };
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&origin);
            frame(&mut socket);
            release_rx.recv_timeout(DEADLINE).unwrap();
            redirect(&mut socket, &format!("127.0.0.1:{target_port}"), false);
            let mut socket = accept(&target);
            frame(&mut socket);
            redirect(
                &mut socket,
                &format!("127.0.0.1:{}", if loop_back { origin_port } else { 1 }),
                false,
            );
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let operation = pending_before_connack(&client);
        release_tx.send(()).unwrap();
        let failure = if loop_back {
            RedirectFailure::Loop
        } else {
            RedirectFailure::AttemptLimit
        };
        let WrapperEvent::Redirect(event) = until(&mut events, |event| {
            matches!(
                event,
                WrapperEvent::Redirect(RedirectEvent {
                    failure: Some(_),
                    ..
                })
            )
        }) else {
            unreachable!()
        };
        assert_connack_redirect(
            &event,
            &format!("127.0.0.1:{}", if loop_back { origin_port } else { 1 }),
            Some(failure),
        );
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::DriverTerminated(_))
        });
        let WrapperEvent::DriverTerminated(error) = event else {
            unreachable!()
        };
        assert_eq!(error.redirect_failure(), Some(failure));
        assert_eq!(error.context().generation, None);
        client.join(DEADLINE).unwrap();
        let error = terminal(&operation).unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert_eq!(error.context().generation, None);
        broker.join();
    }
}

#[test]
fn srv_priority_precedes_weight_and_selected_endpoint_is_reported() {
    assert_resolved_srv_diagnostics(RedirectReason::UseAnotherServer);
}

#[test]
fn server_moved_reports_resolved_srv_diagnostics_before_resetting_redirect_state() {
    assert_resolved_srv_diagnostics(RedirectReason::ServerMoved);
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn assert_resolved_srv_diagnostics(reason: RedirectReason) {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    // Reserve both address families at one port. Retry if another fixture
    // already owns the IPv6 endpoint chosen by the IPv4 ephemeral allocator.
    let deadline = std::time::Instant::now() + DEADLINE;
    let (preferred, competing) = loop {
        let preferred = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = preferred.local_addr().unwrap().port();
        match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)) {
            Ok(competing) => break (preferred, competing),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "fixture port reservation timed out"
                );
            }
            Err(error) => panic!("IPv6 fixture bind failed: {error}"),
        }
    };
    let backup = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = preferred.local_addr().unwrap().port();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let mut config = config(true, origin.local_addr().unwrap().port());
    config.common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = if reason == RedirectReason::UseAnotherServer {
        application_policy(|request| {
            assert_eq!(
                request.references[0].srv_owner.as_deref(),
                Some("_mqtt._tcp.service.invalid")
            );
            RedirectResponse::follow(request, 0, RedirectTargetConfig::new(TransportConfig::Tcp))
        })
    } else {
        RedirectPolicy::Follow {
            max_attempts: 2,
            transport: TransportConfig::Tcp,
        }
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Resolver {
        result: Mutex::new(Some(result_rx)),
        entered: entered_tx,
    })));
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect_with_reason(&mut socket, "_mqtt._tcp.service.invalid", false, reason);
        let mut socket = accept(&preferred);
        frame(&mut socket);
        socket.write_all(TARGET_CONNACK).unwrap();
        assert_eq!(frame(&mut socket)[0], 0xe0);
    });
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    entered_rx.recv_timeout(DEADLINE).unwrap();
    result_tx
        .send(Ok(vec![
            SrvRecord {
                priority: 20,
                weight: u16::MAX,
                port: backup.local_addr().unwrap().port(),
                target: "localhost.".into(),
            },
            SrvRecord {
                priority: 10,
                weight: 0,
                port,
                target: "localhost.".into(),
            },
        ]))
        .unwrap();
    let event = until(&mut events, |event| {
        matches!(
            event,
            WrapperEvent::Redirect(RedirectEvent {
                target: Some(_),
                ..
            })
        )
    });
    let WrapperEvent::Redirect(event) = event else {
        unreachable!()
    };
    assert_eq!(
        event.target,
        Some(BrokerTarget::Tcp {
            host: "localhost".into(),
            port
        })
    );
    assert_eq!(event.source, RedirectSource::ConnAck);
    assert_eq!(event.reason, reason);
    assert_eq!(
        event.server_reference.as_deref(),
        Some("_mqtt._tcp.service.invalid")
    );
    assert_eq!(event.failure, None);
    assert_eq!(event.attempts, 1);
    assert_eq!(event.attempt_limit, Some(2));
    assert_eq!(event.visited_endpoints, 2);
    assert_eq!(event.srv_candidate_index, Some(1));
    assert_eq!(event.srv_candidate_count, Some(2));
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    client.closer().close(DEADLINE).unwrap();
    backup.set_nonblocking(true).unwrap();
    assert_eq!(
        backup.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    broker.join();
    competing.set_nonblocking(true).unwrap();
    assert_eq!(
        competing.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn isolated_redirect_never_reads_or_writes_the_origin_store_scope() {
    #[derive(Default)]
    struct Store(Mutex<Vec<SessionStoreKey>>);
    impl SessionStore for Store {
        fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            self.0.lock().unwrap().push(key);
            Box::pin(async { Ok(None) })
        }
        fn save(&self, key: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
            self.0.lock().unwrap().push(key);
            Box::pin(async { Ok(()) })
        }
        fn clear(&self, key: SessionStoreKey) -> StoreFuture<()> {
            self.0.lock().unwrap().push(key);
            Box::pin(async { Ok(()) })
        }
    }
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let target_port = target.local_addr().unwrap().port();
    let store = Arc::new(Store::default());
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.clean_start = false;
    v5.connect_properties.session_expiry_interval = Some(60);
    v5.session_store = Some(SessionStoreConfig::new(
        store.clone(),
        "private-origin-scope",
    ));
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 1,
        transport: TransportConfig::Tcp,
    };
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect(&mut socket, &format!("127.0.0.1:{target_port}"), false);
        let mut socket = accept(&target);
        assert_isolated(&mut frame(&mut socket));
        socket.write_all(TARGET_CONNACK).unwrap();
        let id = publish_id(&mut socket, true);
        puback(&mut socket, id);
        assert_eq!(frame(&mut socket)[0], 0xe0);
    });
    let mut client = NativeClient::start(config).unwrap();
    let _events = connected(&mut client);
    let before = store.0.lock().unwrap().clone();
    assert!(!before.is_empty());
    assert!(
        before
            .iter()
            .all(|key| key.scope == "private-origin-scope" && key.client_id == "parity")
    );
    let operation = publish(&client, b"target-only");
    assert_eq!(
        terminal(&operation).unwrap(),
        Completion::Publish(PublishCompletion::Qos1Acknowledged)
    );
    client.closer().close(DEADLINE).unwrap();
    assert_eq!(
        *store.0.lock().unwrap(),
        before,
        "redirected session crossed the origin store scope"
    );
    broker.join();
}

#[cfg(feature = "websocket")]
#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback error type"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn websocket_redirect_uses_target_uri_and_clears_origin_header_edits() {
    struct OriginHandshake(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl WebSocketHandshake for OriginHandshake {
        fn prepare(&self, _: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {
                let mut response = WebSocketHandshakeResponse::default();
                response.append_header("x-dynamic-origin", b"dynamic-private-token")?;
                Ok(response)
            })
        }
    }
    capture::start();
    let fixture = tls::Fixture::new();
    for encrypted in [false, true] {
        if encrypted
            && !cfg!(any(
                feature = "use-rustls-no-provider",
                feature = "use-native-tls"
            ))
        {
            continue;
        }
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let scheme = if encrypted { "wss" } else { "ws" };
        let reference = format!(
            "{scheme}://127.0.0.1:{}/target?token=target-query-private",
            target.local_addr().unwrap().port()
        );
        let advertised = reference.clone();
        let backend = if cfg!(feature = "use-rustls-no-provider") {
            TlsBackend::Rustls
        } else {
            TlsBackend::Native
        };
        let transport = if encrypted {
            TransportConfig::Wss(fixture.client(backend))
        } else {
            TransportConfig::WebSocket
        };
        let mut config = config(true, 1);
        config.common.broker = BrokerTarget::WebSocket {
            url: format!(
                "{scheme}://127.0.0.1:{}/origin?one=two",
                origin.local_addr().unwrap().port()
            ),
        };
        config.common.transport = transport.clone();
        config.common.websocket_headers = vec![
            WebSocketHeader::Append {
                name: "x-order".into(),
                value: "discard".into(),
            },
            WebSocketHeader::Replace {
                name: "x-order".into(),
                value: "first".into(),
            },
            WebSocketHeader::Append {
                name: "x-order".into(),
                value: "second".into(),
            },
            WebSocketHeader::Replace {
                name: "authorization".into(),
                value: "Bearer origin-private-token".into(),
            },
            WebSocketHeader::Replace {
                name: "cookie".into(),
                value: "origin-private-cookie".into(),
            },
            WebSocketHeader::Remove {
                name: "x-absent".into(),
            },
        ];

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        config.common.websocket_handshake = Some(WebSocketHandshakeConfig(std::sync::Arc::new(
            OriginHandshake(calls.clone()),
        )));
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 1,
            transport,
        };
        let server = fixture.server.clone();
        let broker = Broker::spawn(move || {
            for (redirected, listener) in [(false, origin), (true, target)] {
                let mut stream: Box<dyn tls::Duplex> = Box::new(accept(&listener));
                if encrypted {
                    stream = tls::wrap(stream, server.clone());
                }
                let mut socket = tungstenite::accept_hdr(
                    stream,
                    |request: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        assert_eq!(
                            request.uri().path_and_query().unwrap().as_str(),
                            if redirected {
                                "/target?token=target-query-private"
                            } else {
                                "/origin?one=two"
                            }
                        );
                        assert!(!request.headers().contains_key("x-absent"));
                        if redirected {
                            for header in ["x-order", "authorization", "cookie", "x-dynamic-origin"]
                            {
                                assert!(!request.headers().contains_key(header));
                            }
                        } else {
                            assert_eq!(
                                request.headers()["x-dynamic-origin"],
                                "dynamic-private-token"
                            );
                            let values: Vec<_> = request
                                .headers()
                                .get_all("x-order")
                                .iter()
                                .map(|v| v.to_str().unwrap())
                                .collect();
                            assert_eq!(values, ["first", "second"]);
                            assert_eq!(
                                request.headers()["authorization"],
                                "Bearer origin-private-token"
                            );
                            assert_eq!(request.headers()["cookie"], "origin-private-cookie");
                        }
                        response
                            .headers_mut()
                            .insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                        Ok(response)
                    },
                )
                .unwrap();
                assert_eq!(socket.read().unwrap().into_data()[0], 0x10);
                socket
                    .send(tungstenite::Message::Binary(Bytes::from_static(
                        if redirected {
                            TARGET_CONNACK
                        } else {
                            &[0x20, 3, 0, 0, 0]
                        },
                    )))
                    .unwrap();
                if redirected {
                    assert_eq!(socket.read().unwrap().into_data()[0], 0xe0);
                } else {
                    assert_eq!(socket.read().unwrap().into_data()[0] >> 4, 3);
                    let mut packet = Vec::new();
                    redirect(&mut packet, &reference, true);
                    socket
                        .send(tungstenite::Message::Binary(packet.into()))
                        .unwrap();
                }
            }
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = connected(&mut client);
        let pending = publish(&client, b"pending");
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::Redirect(_))
        });
        let WrapperEvent::Redirect(event) = event else {
            unreachable!()
        };
        assert_eq!(event.server_reference, Some(advertised.clone()));
        assert_eq!(
            event.target,
            Some(BrokerTarget::WebSocket { url: advertised })
        );
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert_eq!(
            terminal(&pending).unwrap_err().delivery_status(),
            DeliveryStatus::Ambiguous
        );
        client.closer().close(DEADLINE).unwrap();
        broker.join();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        capture::assert_redacted(
            "",
            &[
                "origin-private-token",
                "origin-private-cookie",
                "target-query-private",
            ],
        );
    }
}

struct ApplicationPolicy<F>(F);
impl<F> RedirectAuthority for ApplicationPolicy<F>
where
    F: Fn(Arc<RedirectRequest>) -> std::result::Result<RedirectResponse, RedirectDecisionFailure>
        + Send
        + Sync
        + 'static,
{
    fn decide(
        &self,
        request: Arc<RedirectRequest>,
    ) -> std::result::Result<RedirectResponse, RedirectDecisionFailure> {
        (self.0)(request)
    }
}
fn application_policy(
    decide: impl Fn(
        Arc<RedirectRequest>,
    ) -> std::result::Result<RedirectResponse, RedirectDecisionFailure>
    + Send
    + Sync
    + 'static,
) -> RedirectPolicy {
    RedirectPolicy::Application(RedirectAuthorityConfig {
        authority: Arc::new(ApplicationPolicy(decide)),
        max_attempts: 2,
        decision_timeout: DEADLINE,
    })
}
#[derive(Default)]
struct RedirectStore(Mutex<Vec<(u8, SessionStoreKey)>>);
impl SessionStore for RedirectStore {
    fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        self.0.lock().unwrap().push((1, key));
        Box::pin(async { Ok(None) })
    }
    fn save(&self, key: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
        self.0.lock().unwrap().push((2, key));
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, key: SessionStoreKey) -> StoreFuture<()> {
        self.0.lock().unwrap().push((3, key));
        Box::pin(async { Ok(()) })
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "wire, checkpoint and lifetime assertions belong to one redirect scenario"
)]
fn application_redirect_selects_second_reference_with_copied_credentials_and_scoped_store() {
    for disconnect in [false, true] {
        for (changed_id, changed_scope) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let changed_key = changed_id || changed_scope;
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
            let advertised = format!("127.0.0.2:1 {reference}");
            let store = Arc::new(RedirectStore::default());
            let saved_request = Arc::new(Mutex::new(None));
            let capture = saved_request.clone();
            let mut config = config(true, origin.local_addr().unwrap().port());
            config.common.username = Some("origin-private-username".into());
            config.common.password = Some(Bytes::from_static(b"origin-private-password"));
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                unreachable!()
            };
            v5.clean_start = false;
            v5.connect_properties.session_expiry_interval = Some(60);
            v5.session_store = Some(SessionStoreConfig::new(store.clone(), "origin"));
            v5.redirect_policy = application_policy(move |request| {
                assert_eq!(request.attempt, 1);
                assert_eq!(
                    request.source,
                    if disconnect {
                        RedirectSource::Disconnect
                    } else {
                        RedirectSource::ConnAck
                    }
                );
                assert_eq!(request.references.len(), 2);
                assert_eq!(request.store_scope, "origin");
                let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
                profile.client_id = if changed_id {
                    RedirectClientId::Replace("target-id".into())
                } else {
                    RedirectClientId::Reuse
                };
                profile.session = RedirectSession::Reuse {
                    store_scope: if changed_scope { "target" } else { "origin" }.into(),
                };
                profile.username = Some("target-private-username".into());
                profile.password = Some(SecretBytes::new(b"target-private-password".to_vec()));
                assert!(!format!("{profile:?}").contains("target-private"));
                *capture.lock().unwrap() = Some(request.clone());
                RedirectResponse::follow(request, 1, profile)
            });
            let expected_id = if changed_id { "target-id" } else { "parity" };
            let broker = Broker::spawn(move || {
                let mut socket = accept(&origin);
                frame(&mut socket);
                if disconnect {
                    socket.write_all(b"\x20\x03\x00\x00\x00").unwrap();
                }
                redirect_with_reason(
                    &mut socket,
                    &advertised,
                    disconnect,
                    RedirectReason::ServerMoved,
                );
                let mut socket = accept(&target);
                let rumqttc_v5::Packet::Connect(connect, _, auth) =
                    rumqttc_v5::Packet::read(&mut frame(&mut socket), None).unwrap()
                else {
                    panic!("CONNECT")
                };
                assert_eq!(connect.client_id, expected_id);
                assert!(!connect.clean_start);
                assert_eq!(
                    auth,
                    rumqttc_v5::ConnectAuth::UsernamePassword {
                        username: "target-private-username".into(),
                        password: Bytes::from_static(b"target-private-password"),
                    }
                );
                socket.write_all(b"\x20\x03\x00\x00\x00").unwrap();
                let id = publish_id(&mut socket, true);
                puback(&mut socket, id);
                assert_eq!(frame(&mut socket)[0], 0xe0);
            });
            let mut client = NativeClient::start(config.clone()).unwrap();
            let mut events = client.take_events().unwrap();
            let event = until(&mut events, |e| matches!(e, WrapperEvent::Redirect(_)));
            let WrapperEvent::Redirect(event) = event else {
                unreachable!()
            };
            assert_eq!(
                event.selected_reference.as_deref(),
                Some(reference.as_str())
            );
            until(&mut events, |e| {
                matches!(e, WrapperEvent::Connected { .. })
                    && saved_request.lock().unwrap().is_some()
            });
            // The target lease conflicts with a second driver before any target store I/O.
            let mut probe = config.clone();
            probe.common.client_id = expected_id.into();
            let ProtocolConfig::V5(v5) = &mut probe.protocol else {
                unreachable!()
            };
            v5.redirect_policy = RedirectPolicy::Reject;
            v5.session_store.as_mut().unwrap().scope =
                if changed_scope { "target" } else { "origin" }.into();
            assert_eq!(
                NativeClient::start(probe.clone())
                    .unwrap_err()
                    .store_failure(),
                Some(StoreFailure::InUse)
            );
            if changed_key {
                let mut origin_probe = config.clone();
                let ProtocolConfig::V5(v5) = &mut origin_probe.protocol else {
                    unreachable!()
                };
                v5.redirect_policy = RedirectPolicy::Reject;
                let origin_owner = NativeClient::start(origin_probe).unwrap();
                origin_owner.closer().close_now(DEADLINE).unwrap();
            }
            assert_eq!(
                terminal(&publish(&client, b"target-work")).unwrap(),
                Completion::Publish(PublishCompletion::Qos1Acknowledged)
            );
            client.closer().close(DEADLINE).unwrap();
            broker.join();
            let calls = store.0.lock().unwrap().clone();
            assert!(calls.iter().any(|(_, key)| key.scope == "origin"));
            assert!(calls.iter().any(|(_, key)| key.scope
                == if changed_scope { "target" } else { "origin" }
                && key.client_id == expected_id));
            drop(client);
            let owner = NativeClient::start(probe).unwrap();
            owner.closer().close_now(DEADLINE).unwrap();
            let request = saved_request.lock().unwrap().take().unwrap();
            assert_eq!(request.references[1].raw, reference);
        }
    }
}

#[test]
fn application_rejection_failures_and_expired_decisions_are_terminal_before_target_dial() {
    for mode in [
        "reject", "invalid", "panic", "timeout", "callback", "resource",
    ] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config(true, origin.local_addr().unwrap().port());
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = application_policy(move |request| match mode {
            "reject" => Ok(RedirectResponse::reject(request)),
            "invalid" => RedirectResponse::follow(request, 0, {
                let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
                profile.client_id = RedirectClientId::Replace("bad\0id".into());
                profile
            }),
            "panic" => panic!("private-host-error"),
            "resource" => RedirectResponse::follow(request, 0, {
                let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
                profile.username = Some("x".repeat(MAX_REDIRECT_RESPONSE_BYTES + 1));
                profile
            }),
            "timeout" => {
                std::thread::sleep(std::time::Duration::from_millis(15));
                Ok(RedirectResponse::reject(request))
            }
            _ => Err(RedirectDecisionFailure::Callback),
        });
        if mode == "timeout" {
            let RedirectPolicy::Application(policy) = &mut v5.redirect_policy else {
                unreachable!()
            };
            policy.decision_timeout = std::time::Duration::from_millis(1);
        }
        let broker = Broker::spawn(move || {
            let mut socket = accept(&origin);
            frame(&mut socket);
            redirect(&mut socket, "127.0.0.2:1", false);
        });
        let mut client = NativeClient::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let WrapperEvent::Redirect(event) =
            until(&mut events, |e| matches!(e, WrapperEvent::Redirect(_)))
        else {
            unreachable!()
        };
        let expected = match mode {
            "reject" => RedirectFailure::Rejected,
            "invalid" => RedirectFailure::Policy(RedirectDecisionFailure::InvalidResponse),
            "panic" => RedirectFailure::Policy(RedirectDecisionFailure::Panic),
            "timeout" => RedirectFailure::Policy(RedirectDecisionFailure::Timeout),
            "resource" => RedirectFailure::Policy(RedirectDecisionFailure::ResourceLimit),
            _ => RedirectFailure::Policy(RedirectDecisionFailure::Callback),
        };
        assert_eq!(event.failure, Some(expected));
        assert!(!event.followed);
        assert!(!format!("{event:?}").contains("private-host-error"));
        until(&mut events, |e| {
            matches!(e, WrapperEvent::DriverTerminated(_))
        });
        let _ = client.closer().close_now(DEADLINE);
        broker.join();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "keep resumed/refused QoS and alias wire assertions together"
)]
fn approved_same_session_redirect_preserves_tracked_qos_flows_and_packet_ids() {
    for resume in [false, true] {
        for qos2 in [false, true] {
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
            let mut config = config(true, origin.local_addr().unwrap().port());
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                unreachable!()
            };
            v5.topic_alias_policy = TopicAliasPolicy::Monotonic;
            v5.clean_start = false;
            v5.connect_properties.session_expiry_interval = Some(60);
            v5.session_store = Some(SessionStoreConfig::new(
                Arc::new(RedirectStore::default()),
                "cluster",
            ));
            v5.redirect_policy = application_policy(|request| {
                let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
                profile.client_id = RedirectClientId::Reuse;
                profile.session = RedirectSession::Reuse {
                    store_scope: "cluster".into(),
                };
                RedirectResponse::follow(request, 0, profile)
            });
            let broker = Broker::spawn(move || {
                let mut socket = accept(&origin);
                frame(&mut socket);
                socket
                    .write_all(b"\x20\x06\x00\x00\x03\x22\x00\x01")
                    .unwrap();
                let rumqttc_v5::Packet::Publish(original) =
                    rumqttc_v5::Packet::read(&mut frame(&mut socket), None).unwrap()
                else {
                    panic!("origin PUBLISH")
                };
                assert_eq!(
                    original.properties.as_ref().and_then(|p| p.topic_alias),
                    Some(1)
                );
                let id = original.pkid;
                redirect_with_reason(&mut socket, &reference, true, RedirectReason::ServerMoved);
                let mut socket = accept(&target);
                frame(&mut socket);
                socket
                    .write_all(&[0x20, 3, u8::from(resume), 0, 0])
                    .unwrap();
                if !resume {
                    assert_eq!(frame(&mut socket)[0], 0xe0);
                    return;
                }
                let rumqttc_v5::Packet::Publish(replay) =
                    rumqttc_v5::Packet::read(&mut frame(&mut socket), None).unwrap()
                else {
                    panic!("replayed PUBLISH")
                };
                assert_eq!(replay.pkid, id);
                assert!(replay.dup);
                assert_eq!(replay.topic.as_ref(), b"replay");
                assert!(
                    replay
                        .properties
                        .as_ref()
                        .and_then(|p| p.topic_alias)
                        .is_none()
                );
                assert_eq!(replay.payload.as_ref(), b"tracked-origin-work");
                if qos2 {
                    let [high, low] = id.to_be_bytes();
                    socket.write_all(&[0x50, 2, high, low]).unwrap();
                    assert_eq!(frame(&mut socket)[0], 0x62);
                    socket.write_all(&[0x70, 2, high, low]).unwrap();
                } else {
                    puback(&mut socket, id);
                }
                assert_eq!(frame(&mut socket)[0], 0xe0);
            });
            let mut client = NativeClient::start(config).unwrap();
            let mut events = connected(&mut client);
            let operation = client
                .handle()
                .try_admit(Command::Publish(PublishCommand {
                    topic: "replay".into(),
                    payload: Bytes::from_static(b"tracked-origin-work"),
                    qos: if qos2 {
                        QoS::ExactlyOnce
                    } else {
                        QoS::AtLeastOnce
                    },
                    retain: false,
                    protocol: PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            until(&mut events, |e| matches!(e, WrapperEvent::Redirect(_)));
            until(&mut events, |e| matches!(e, WrapperEvent::Connected { .. }));
            if resume {
                assert_eq!(
                    terminal(&operation).unwrap(),
                    Completion::Publish(if qos2 {
                        PublishCompletion::Qos2Completed
                    } else {
                        PublishCompletion::Qos1Acknowledged
                    })
                );
            } else {
                assert_eq!(
                    terminal(&operation).unwrap_err().delivery_status(),
                    DeliveryStatus::Ambiguous
                );
            }
            client.closer().close(DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[derive(Default)]
struct IdentityAuthority(Mutex<Vec<(String, bool)>>);
impl Authenticator for IdentityAuthority {
    fn respond(
        &self,
        context: AuthContext,
        challenge: AuthChallenge,
    ) -> std::result::Result<AuthAction, AuthFailure> {
        self.0
            .lock()
            .unwrap()
            .push((context.client_id, matches!(challenge, AuthChallenge::Start)));
        if matches!(challenge, AuthChallenge::Start) {
            Ok(AuthAction::Send(AuthProperties::default()))
        } else {
            Ok(AuthAction::Complete)
        }
    }
}
impl AsyncAuthenticator for IdentityAuthority {
    fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
        let start = matches!(challenge, AsyncAuthChallenge::Start);
        self.0.lock().unwrap().push((context.client_id, start));
        Box::pin(async move {
            if start {
                Ok(AuthAction::Send(AuthProperties::default()))
            } else {
                Ok(AuthAction::Complete)
            }
        })
    }
}
#[test]
fn reused_authentication_authority_observes_effective_redirect_and_assigned_identities() {
    for asynchronous in [false, true] {
        for fresh in [false, true] {
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
            let authority = Arc::new(IdentityAuthority::default());
            let mut config = config(true, origin.local_addr().unwrap().port());
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                unreachable!()
            };
            v5.connect_properties.authentication_method = Some("test".into());
            if asynchronous {
                v5.async_authenticator = Some(AsyncAuthenticatorConfig::new(authority.clone()));
            } else {
                v5.authenticator = Some(AuthenticatorConfig::new(authority.clone()));
            }
            v5.redirect_policy = application_policy(move |request| {
                let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
                profile.client_id = if fresh {
                    RedirectClientId::Fresh
                } else {
                    RedirectClientId::Replace("replacement".into())
                };
                profile.reuse_authentication_authority = true;
                RedirectResponse::follow(request, 0, profile)
            });
            let broker = Broker::spawn(move || {
                let mut socket = accept(&origin);
                frame(&mut socket);
                redirect(&mut socket, &reference, false);
                let mut socket = accept(&target);
                frame(&mut socket);
                let mut properties = b"\x15\x00\x04test".to_vec();
                if fresh {
                    properties.extend_from_slice(b"\x12\x00\x08assigned");
                }
                let mut packet = vec![
                    0x20,
                    u8::try_from(properties.len() + 3).unwrap(),
                    0,
                    0,
                    u8::try_from(properties.len()).unwrap(),
                ];
                packet.extend(properties);
                socket.write_all(&packet).unwrap();
                assert_eq!(frame(&mut socket)[0], 0xe0);
            });
            let mut client = NativeClient::start(config).unwrap();
            let _events = connected(&mut client);
            let contexts = authority.0.lock().unwrap().clone();
            assert!(contexts.contains(&("parity".into(), true)));
            assert!(contexts.contains(&(if fresh { "" } else { "replacement" }.into(), true)));
            assert!(
                contexts.contains(&(if fresh { "assigned" } else { "replacement" }.into(), false)),
                "async={asynchronous} fresh={fresh}: {contexts:?}"
            );
            client.closer().close(DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "nested wire hops and lease assertions form one scenario"
)]
fn temporary_nested_scoped_redirect_restores_origin_and_releases_target_leases() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let intermediate = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let first = format!("127.0.0.1:{}", intermediate.local_addr().unwrap().port());
    let second = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
    let store = Arc::new(RedirectStore::default());
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.clean_start = false;
    v5.connect_properties.session_expiry_interval = Some(60);
    v5.session_store = Some(SessionStoreConfig::new(store.clone(), "origin"));
    v5.redirect_policy = application_policy(|request| {
        let mut target = RedirectTargetConfig::new(TransportConfig::Tcp);
        let id = if request.attempt == 1 {
            "intermediate"
        } else {
            "target"
        };
        target.client_id = RedirectClientId::Replace(id.into());
        target.session = RedirectSession::Reuse {
            store_scope: id.into(),
        };
        RedirectResponse::follow(request, 0, target)
    });
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect(&mut socket, &first, false);
        let mut socket = accept(&intermediate);
        frame(&mut socket);
        socket.write_all(b"\x20\x03\x00\x00\x00").unwrap();
        redirect(&mut socket, &second, true);
        let mut socket = accept(&target);
        frame(&mut socket);
        socket.write_all(b"\x20\x03\x00\x00\x00").unwrap();
        release_rx.recv_timeout(DEADLINE).unwrap();
        drop(socket);
        let mut socket = accept(&origin);
        let rumqttc_v5::Packet::Connect(connect, _, _) =
            rumqttc_v5::Packet::read(&mut frame(&mut socket), None).unwrap()
        else {
            panic!("origin CONNECT")
        };
        assert_eq!(connect.client_id, "parity");
        socket.write_all(b"\x20\x03\x00\x00\x00").unwrap();
        assert_eq!(frame(&mut socket)[0], 0xe0);
    });
    let mut client = NativeClient::start(config.clone()).unwrap();
    let mut events = connected(&mut client);
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    let probe = |scope: &str, id: &str| {
        let mut probe = config.clone();
        probe.common.client_id = id.into();
        let ProtocolConfig::V5(v5) = &mut probe.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Reject;
        v5.session_store.as_mut().unwrap().scope = scope.into();
        probe.common.broker = BrokerTarget::Tcp {
            host: "127.0.0.2".into(),
            port: 1,
        };
        NativeClient::start(probe).map(|owner| owner.closer().close_now(DEADLINE).unwrap())
    };
    assert_eq!(
        probe("origin", "parity").unwrap_err().store_failure(),
        Some(StoreFailure::InUse)
    );
    assert_eq!(
        probe("target", "target").unwrap_err().store_failure(),
        Some(StoreFailure::InUse)
    );
    probe("intermediate", "intermediate").unwrap();
    release_tx.send(()).unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    probe("target", "target").unwrap();
    assert_eq!(
        probe("origin", "parity").unwrap_err().store_failure(),
        Some(StoreFailure::InUse)
    );
    let calls = store.0.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .any(|(_, key)| key.scope == "target" && key.client_id == "target")
    );
    assert!(
        calls
            .iter()
            .filter(|(op, key)| *op == 1 && key.scope == "origin")
            .count()
            >= 2
    );
    client.closer().close(DEADLINE).unwrap();
    broker.join();
    probe("origin", "parity").unwrap();
}

#[test]
fn application_target_lease_conflict_is_typed_and_never_dials_the_target() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.clean_start = false;
    v5.connect_properties.session_expiry_interval = Some(60);
    v5.session_store = Some(SessionStoreConfig::new(
        Arc::new(RedirectStore::default()),
        "origin",
    ));
    let mut competing_config = config.clone();
    competing_config.common.client_id = "target".into();
    competing_config.common.broker = BrokerTarget::Tcp {
        host: "127.0.0.2".into(),
        port: 1,
    };
    let ProtocolConfig::V5(v5) = &mut competing_config.protocol else {
        unreachable!()
    };
    v5.session_store.as_mut().unwrap().scope = "target".into();
    let competing = NativeClient::start(competing_config).unwrap();
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = application_policy(|request| {
        let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
        profile.client_id = RedirectClientId::Replace("target".into());
        profile.session = RedirectSession::Reuse {
            store_scope: "target".into(),
        };
        RedirectResponse::follow(request, 0, profile)
    });
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect(&mut socket, &reference, false);
    });
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let WrapperEvent::Redirect(event) =
        until(&mut events, |e| matches!(e, WrapperEvent::Redirect(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        event.failure,
        Some(RedirectFailure::Policy(RedirectDecisionFailure::StoreInUse))
    );
    let WrapperEvent::DriverTerminated(error) = until(&mut events, |e| {
        matches!(e, WrapperEvent::DriverTerminated(_))
    }) else {
        unreachable!()
    };
    assert_eq!(error.store_failure(), Some(StoreFailure::InUse));
    target.set_nonblocking(true).unwrap();
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let _ = client.closer().close_now(DEADLINE);
    competing.closer().close_now(DEADLINE).unwrap();
    broker.join();
}

#[test]
fn reentrant_shutdown_from_redirect_callback_prevents_target_dial() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
    let handle = Arc::new(Mutex::new(None::<ClientHandle>));
    let captured = handle.clone();
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = application_policy(move |request| {
        captured
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        RedirectResponse::follow(request, 0, RedirectTargetConfig::new(TransportConfig::Tcp))
    });
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        ready_rx.recv_timeout(DEADLINE).unwrap();
        redirect(&mut socket, &reference, false);
    });
    let mut client = NativeClient::start(config).unwrap();
    *handle.lock().unwrap() = Some(client.handle());
    ready_tx.send(()).unwrap();
    let mut events = client.take_events().unwrap();
    while let Some(event) = events.recv_timeout(DEADLINE).unwrap() {
        assert!(!matches!(event, WrapperEvent::Connected { .. }));
    }
    target.set_nonblocking(true).unwrap();
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    broker.join();
    let _ = client.closer().close_now(DEADLINE);
}

#[test]
fn same_session_redirect_invalidates_manual_ack_tokens_for_the_previous_connection() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
    let mut config = config(true, origin.local_addr().unwrap().port());
    config.common.ack_mode = AckMode::Manual;
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.clean_start = false;
    v5.connect_properties.session_expiry_interval = Some(60);
    v5.redirect_policy = application_policy(|request| {
        let mut profile = RedirectTargetConfig::new(TransportConfig::Tcp);
        profile.client_id = RedirectClientId::Reuse;
        profile.session = RedirectSession::Reuse {
            store_scope: String::new(),
        };
        RedirectResponse::follow(request, 0, profile)
    });
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        connect(&mut socket, true);
        socket.write_all(b"\x32\x07\x00\x01a\x00\x07\x00x").unwrap();
        release_rx.recv_timeout(DEADLINE).unwrap();
        redirect_with_reason(&mut socket, &reference, true, RedirectReason::ServerMoved);
        let mut socket = accept(&target);
        frame(&mut socket);
        socket.write_all(b"\x20\x03\x01\x00\x00").unwrap();
        // Same packet ID, newly delivered on the target connection.
        socket.write_all(b"\x3a\x07\x00\x01a\x00\x07\x00x").unwrap();
        assert_eq!(frame(&mut socket).as_ref(), b"\x40\x02\x00\x07");
        assert_eq!(frame(&mut socket)[0], 0xe0);
    });
    let mut client = NativeClient::start(config).unwrap();
    let mut events = connected(&mut client);
    let publication = |events: &mut EventConsumer| {
        let WrapperEvent::IncomingPublish(publish) =
            until(events, |e| matches!(e, WrapperEvent::IncomingPublish(_)))
        else {
            unreachable!()
        };
        publish
    };
    let previous = publication(&mut events).ack_token.unwrap();
    release_tx.send(()).unwrap();
    until(&mut events, |e| matches!(e, WrapperEvent::Connected { .. }));
    let current = publication(&mut events).ack_token.unwrap();
    assert_ne!(previous, current);
    assert!(
        client
            .handle()
            .try_admit(Command::Acknowledge(previous))
            .is_err()
    );
    terminal(
        &client
            .handle()
            .try_admit(Command::Acknowledge(current))
            .unwrap(),
    )
    .unwrap();
    client.closer().close(DEADLINE).unwrap();
    broker.join();
}

#[cfg(feature = "websocket")]
#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback error type"
)]
#[allow(
    clippy::too_many_lines,
    reason = "exercise independent reuse flags on both network and MQTT wire"
)]
fn application_authentication_and_websocket_network_reuse_are_independent() {
    for reuse_auth in [false, true] {
        for reuse_network in [false, true] {
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = TcpListener::bind("127.0.0.1:0").unwrap();
            let reference = format!(
                "ws://127.0.0.1:{}/target",
                target.local_addr().unwrap().port()
            );
            let authority = Arc::new(IdentityAuthority::default());
            let mut config = config(true, 1);
            config.common.broker = BrokerTarget::WebSocket {
                url: format!(
                    "ws://127.0.0.1:{}/origin",
                    origin.local_addr().unwrap().port()
                ),
            };
            config.common.transport = TransportConfig::WebSocket;
            config.common.username = Some("private-origin-user".into());
            config.common.password = Some(Bytes::from_static(b"private-origin-password"));
            config.common.websocket_headers = vec![WebSocketHeader::Replace {
                name: "authorization".into(),
                value: "Bearer private-origin-header".into(),
            }];
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                unreachable!()
            };
            v5.connect_properties.authentication_method = Some("test".into());
            v5.authenticator = Some(AuthenticatorConfig::new(authority.clone()));
            v5.redirect_policy = application_policy(move |request| {
                let mut profile = RedirectTargetConfig::new(TransportConfig::WebSocket);
                profile.client_id = RedirectClientId::Replace("target".into());
                profile.reuse_authentication_authority = reuse_auth;
                profile.reuse_network_credentials = reuse_network;
                RedirectResponse::follow(request, 0, profile)
            });
            let broker = Broker::spawn(move || {
                for (listener, redirected) in [(&origin, false), (&target, true)] {
                    let mut socket = tungstenite::accept_hdr(accept(listener),
                        |request: &tungstenite::handshake::server::Request,
                         mut response: tungstenite::handshake::server::Response| {
                            assert_eq!(request.headers().contains_key("authorization"), !redirected || reuse_network);
                            assert_eq!(request.uri().path(), if redirected { "/target" } else { "/origin" });
                            response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                            Ok(response)
                        }).unwrap();
                    let mut packet = BytesMut::from(socket.read().unwrap().into_data().as_ref());
                    let rumqttc_v5::Packet::Connect(connect, _, auth) =
                        rumqttc_v5::Packet::read(&mut packet, None).unwrap()
                    else {
                        panic!("CONNECT")
                    };
                    assert_eq!(
                        connect
                            .properties
                            .as_ref()
                            .and_then(|p| p.authentication_method.as_ref())
                            .is_some(),
                        !redirected || reuse_auth
                    );
                    if redirected {
                        assert_eq!(connect.client_id, "target");
                        assert_eq!(auth, rumqttc_v5::ConnectAuth::None);
                        let connack: &[u8] = if reuse_auth {
                            b"\x20\x0a\x00\x00\x07\x15\x00\x04test"
                        } else {
                            b"\x20\x03\x00\x00\x00"
                        };
                        socket
                            .send(tungstenite::Message::Binary(Bytes::copy_from_slice(
                                connack,
                            )))
                            .unwrap();
                        assert_eq!(socket.read().unwrap().into_data()[0], 0xe0);
                    } else {
                        let mut packet = Vec::new();
                        redirect_with_reason(
                            &mut packet,
                            &reference,
                            false,
                            RedirectReason::ServerMoved,
                        );
                        socket
                            .send(tungstenite::Message::Binary(packet.into()))
                            .unwrap();
                    }
                }
            });
            let mut client = NativeClient::start(config).unwrap();
            let _events = connected(&mut client);
            let contexts = authority.0.lock().unwrap().clone();
            assert_eq!(contexts.contains(&("target".into(), true)), reuse_auth);
            assert_eq!(contexts.contains(&("target".into(), false)), reuse_auth);
            client.closer().close(DEADLINE).unwrap();
            broker.join();
        }
    }
}
