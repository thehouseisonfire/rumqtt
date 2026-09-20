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
    let mut properties = vec![0x1c];
    properties.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
    properties.extend_from_slice(reference.as_bytes());
    let mut body = if disconnect {
        vec![0x9c]
    } else {
        vec![0, 0x9c]
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
fn redirect_reference_forms_select_isolated_endpoints_for_both_sources() {
    let tls = tls::Fixture::new();
    for disconnect in [false, true] {
        for scheme in ["authority", "mqtt", "mqtts", "ws", "wss"] {
            let encrypted = scheme == "mqtts" || scheme == "wss";
            let websocket = scheme == "ws" || scheme == "wss";
            if encrypted && !cfg!(any(feature = "use-rustls", feature = "use-native-tls")) {
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
                let backend = if cfg!(feature = "use-rustls") {
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
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let preferred = TcpListener::bind("127.0.0.1:0").unwrap();
    let backup = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = preferred.local_addr().unwrap().port();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let mut config = config(true, origin.local_addr().unwrap().port());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 2,
        transport: TransportConfig::Tcp,
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Resolver {
        result: Mutex::new(Some(result_rx)),
        entered: entered_tx,
    })));
    let broker = Broker::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        redirect(&mut socket, "_mqtt._tcp.service.invalid", false);
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
                target: "127.0.0.1".into(),
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
    assert_eq!(event.reason, RedirectReason::UseAnotherServer);
    assert_eq!(
        event.server_reference.as_deref(),
        Some("_mqtt._tcp.service.invalid")
    );
    assert_eq!(event.failure, None);
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
fn websocket_redirect_uses_target_uri_and_clears_origin_header_edits() {
    capture::start();
    let fixture = tls::Fixture::new();
    for encrypted in [false, true] {
        if encrypted && !cfg!(any(feature = "use-rustls", feature = "use-native-tls")) {
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
        let backend = if cfg!(feature = "use-rustls") {
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
                            for header in ["x-order", "authorization", "cookie"] {
                                assert!(!request.headers().contains_key(header));
                            }
                        } else {
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
