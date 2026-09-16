use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use rumqttc_wrapper_core::{
    ClientConfig, Command, LastWillConfig, LastWillProtocolOptions, NativeClient, ProtocolConfig,
    QoS, V5ConnectProperties, V5WillProperties, WrapperEvent,
};

fn read_frame(stream: &mut impl Read) -> BytesMut {
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
    assert!(length < 1024 * 1024);
    let header = frame.len();
    frame.resize(header + length, 0);
    stream.read_exact(&mut frame[header..]).unwrap();
    frame
}

fn wait_connected(native: &mut NativeClient) -> rumqttc_wrapper_core::EventConsumer {
    let mut events = native.take_events().unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Some(WrapperEvent::Connected { .. })
    ));
    events
}

#[test]
fn mqtt5_batched_publishes_arrive_before_broker_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let broker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let _connect = read_frame(&mut socket);
        // One write puts CONNACK, two QoS 0 PUBLISH packets, and DISCONNECT
        // in the same network batch. Keep the socket open until observed.
        socket
            .write_all(
                b"\x20\x03\x00\x00\x00\x30\x05\x00\x01a\x00x\x30\x05\x00\x01a\x00y\xe0\x02\x00\x00",
            )
            .unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(3));
    });
    let mut native =
        NativeClient::start(ClientConfig::v5("batch-order", "127.0.0.1", port)).unwrap();
    let mut events = wait_connected(&mut native);
    for payload in [b"x", b"y"] {
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        let WrapperEvent::IncomingPublish(publish) = event else {
            panic!("expected PUBLISH before DISCONNECT, got {event:?}");
        };
        assert_eq!(publish.payload.as_ref(), payload);
    }
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Some(WrapperEvent::BrokerDisconnect(_))
    ));
    native.closer().close_now(Duration::from_secs(3)).unwrap();
    drop(release_tx);
    broker.join().unwrap();
}

#[test]
fn mqtt5_connect_and_will_properties_reach_the_wire_without_loss() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut frame = read_frame(&mut socket);
        let rumqttc_v5::Packet::Connect(connect, will, _) =
            rumqttc_v5::Packet::read(&mut frame, None).unwrap()
        else {
            panic!("expected CONNECT")
        };
        let p = connect.properties.unwrap();
        assert_eq!(p.session_expiry_interval, Some(42));
        assert_eq!(p.receive_maximum, Some(7));
        assert_eq!(p.max_packet_size, Some(12000));
        assert_eq!(p.topic_alias_max, Some(9));
        assert_eq!(p.request_response_info, Some(0));
        assert_eq!(p.request_problem_info, Some(1));
        assert_eq!(
            p.user_properties,
            vec![
                ("same".into(), "first".into()),
                ("same".into(), String::new())
            ]
        );
        // CONNECT method/data mapping is checked by the backend construction test
        // without starting an enhanced authentication exchange.
        let will = will.unwrap();
        assert_eq!(will.topic.as_ref(), b"last/will");
        assert_eq!(will.message.as_ref(), b"\0\xff");
        assert_eq!(will.qos, rumqttc_v5::QoS::ExactlyOnce);
        assert!(will.retain);
        let p = will.properties.unwrap();
        assert_eq!(p.delay_interval, Some(0));
        assert_eq!(p.message_expiry_interval, Some(u32::MAX));
        assert_eq!(p.payload_format_indicator, Some(0));
        assert_eq!(p.content_type.as_deref(), Some(""));
        assert_eq!(p.response_topic.as_deref(), Some("reply"));
        assert_eq!(p.correlation_data, Some(Bytes::new()));
        assert_eq!(p.user_properties, vec![("key".into(), "value".into()); 2]);
        socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        assert_eq!(read_frame(&mut socket)[0], 0xe0);
    });
    let mut config = ClientConfig::v5("wire", "127.0.0.1", port);
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.connect_properties = V5ConnectProperties {
        session_expiry_interval: Some(42),
        receive_maximum: Some(7),
        maximum_packet_size: Some(12000),
        topic_alias_maximum: Some(9),
        request_response_information: Some(0),
        request_problem_information: Some(1),
        user_properties: vec![
            ("same".into(), "first".into()),
            ("same".into(), String::new()),
        ],
        ..Default::default()
    };
    config.common.last_will = Some(LastWillConfig {
        topic: "last/will".into(),
        payload: Bytes::from_static(b"\0\xff"),
        qos: QoS::ExactlyOnce,
        retain: true,
        protocol: LastWillProtocolOptions::V5(V5WillProperties {
            will_delay_interval: Some(0),
            message_expiry_interval: Some(u32::MAX),
            payload_format_indicator: Some(0),
            content_type: Some(String::new()),
            response_topic: Some("reply".into()),
            correlation_data: Some(Bytes::new()),
            user_properties: vec![("key".into(), "value".into()); 2],
        }),
    });
    let mut native = NativeClient::start(config).unwrap();
    let _events = wait_connected(&mut native);
    native
        .handle()
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(2)),
        })
        .unwrap();
    native.join(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
}

#[test]
fn mqtt4_binary_will_is_encoded_and_graceful_close_sends_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut frame = read_frame(&mut socket);
        let rumqttc_v4::Packet::Connect(connect) =
            rumqttc_v4::Packet::read(&mut frame, 1024 * 1024).unwrap()
        else {
            panic!("expected CONNECT")
        };
        let will = connect.last_will.unwrap();
        assert_eq!(will.topic, "last/will");
        assert_eq!(will.message.as_ref(), b"\0\xff");
        assert_eq!(will.qos, rumqttc_v4::QoS::AtLeastOnce);
        assert!(will.retain);
        socket.write_all(&[0x20, 2, 0, 0]).unwrap();
        assert_eq!(read_frame(&mut socket).as_ref(), &[0xe0, 0]);
    });
    let mut config = ClientConfig::v4("wire", "127.0.0.1", port);
    config.common.last_will = Some(LastWillConfig {
        topic: "last/will".into(),
        payload: Bytes::from_static(b"\0\xff"),
        qos: QoS::AtLeastOnce,
        retain: true,
        protocol: LastWillProtocolOptions::VersionNeutral,
    });
    let mut native = NativeClient::start(config).unwrap();
    let _events = wait_connected(&mut native);
    native
        .handle()
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_secs(2)),
        })
        .unwrap();
    native.join(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn unix_targets_connect_and_close_for_both_protocols() {
    use rumqttc_wrapper_core::{BrokerTarget, TransportConfig};
    use std::os::unix::net::UnixListener;
    for mqtt5 in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mqtt.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let broker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            assert_eq!(read_frame(&mut socket)[0], 0x10);
            socket
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                })
                .unwrap();
            assert_eq!(read_frame(&mut socket)[0], 0xe0);
        });
        let mut config = if mqtt5 {
            ClientConfig::v5("unix", "unused", 1)
        } else {
            ClientConfig::v4("unix", "unused", 1)
        };
        config.common.broker = BrokerTarget::Unix { path };
        config.common.transport = TransportConfig::Unix;
        let mut native = NativeClient::start(config).unwrap();
        let _events = wait_connected(&mut native);
        native
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        native.join(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
    }
}

#[test]
fn mqtt5_disconnect_payload_is_exact_for_graceful_and_immediate_close() {
    use rumqttc_wrapper_core::{DisconnectProtocolOptions, V5DisconnectOptions};
    for immediate in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            read_frame(&mut socket);
            socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
            let mut frame = read_frame(&mut socket);
            let rumqttc_v5::Packet::Disconnect(packet) =
                rumqttc_v5::Packet::read(&mut frame, None).unwrap()
            else {
                panic!("expected DISCONNECT");
            };
            assert_eq!(packet.reason_code as u8, 4);
            let p = packet.properties.unwrap();
            assert_eq!(p.session_expiry_interval, Some(0));
            assert_eq!(p.reason_string.as_deref(), Some(""));
            assert_eq!(
                p.user_properties,
                vec![("key".into(), "one".into()), ("key".into(), String::new())]
            );
            assert_eq!(p.server_reference, None);
        });
        let mut native =
            NativeClient::start(ClientConfig::v5("disconnect", "127.0.0.1", port)).unwrap();
        let _events = wait_connected(&mut native);
        let payload = DisconnectProtocolOptions::V5(V5DisconnectOptions {
            reason_code: 4,
            session_expiry_interval: Some(0),
            reason_string: Some(String::new()),
            user_properties: vec![("key".into(), "one".into()), ("key".into(), String::new())],
            server_reference: None,
        });
        let closer = native.closer();
        if immediate {
            closer
                .close_now_with_options(Duration::from_secs(3), payload.clone())
                .unwrap();
            closer
                .close_now_with_options(Duration::from_secs(3), payload)
                .unwrap();
        } else {
            closer
                .close_with_options(Duration::from_secs(3), payload.clone())
                .unwrap();
            closer
                .close_with_options(Duration::from_secs(3), payload)
                .unwrap();
        }
        assert_eq!(
            closer.close_now(Duration::from_secs(1)).unwrap_err().kind(),
            rumqttc_wrapper_core::ErrorKind::Shutdown
        );
        broker.join().unwrap();
    }
}

#[test]
fn concurrent_close_selects_one_payload_and_coalesces_only_matching_callers() {
    use rumqttc_wrapper_core::{DisconnectProtocolOptions, V5DisconnectOptions};
    use std::sync::{Arc, Barrier};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        read_frame(&mut socket);
        socket.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        let mut frame = read_frame(&mut socket);
        let rumqttc_v5::Packet::Disconnect(packet) =
            rumqttc_v5::Packet::read(&mut frame, None).unwrap()
        else {
            panic!("expected DISCONNECT");
        };
        packet.properties.unwrap().reason_string.unwrap()
    });
    let mut native = NativeClient::start(ClientConfig::v5("race", "127.0.0.1", port)).unwrap();
    let _events = wait_connected(&mut native);
    let barrier = Arc::new(Barrier::new(8));
    let callers: Vec<_> = (0..8)
        .map(|index| {
            let closer = native.closer();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let value = (index % 2).to_string();
                let payload = DisconnectProtocolOptions::V5(V5DisconnectOptions {
                    reason_string: Some(value.clone()),
                    ..Default::default()
                });
                barrier.wait();
                (
                    value,
                    closer.close_with_options(Duration::from_secs(3), payload),
                )
            })
        })
        .collect();
    let observed = broker.join().unwrap();
    for caller in callers {
        let (payload, result) = caller.join().unwrap();
        if payload == observed {
            assert_eq!(
                result.unwrap(),
                rumqttc_wrapper_core::Completion::GracefulShutdown
            );
        } else {
            assert_eq!(
                result.unwrap_err().delivery_status(),
                rumqttc_wrapper_core::DeliveryStatus::NotAdmitted
            );
        }
    }
}

#[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
#[test]
fn proxy_authentication_and_remote_broker_address_are_preserved() {
    use rumqttc_wrapper_core::{ProxyConfig, ProxyCredentials};
    for mqtt5 in [false, true] {
        for socks in [false, true] {
            if (socks && !cfg!(feature = "socks-proxy"))
                || (!socks && !cfg!(feature = "http-proxy"))
            {
                continue;
            }
            for reject in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let proxy = thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    if socks {
                        let mut greeting = [0; 2];
                        socket.read_exact(&mut greeting).unwrap();
                        assert_eq!(greeting[0], 5);
                        let mut methods = vec![0; usize::from(greeting[1])];
                        socket.read_exact(&mut methods).unwrap();
                        assert!(methods.contains(&2));
                        socket.write_all(&[5, 2]).unwrap();
                        socket.read_exact(&mut greeting).unwrap();
                        assert_eq!(greeting, [1, 4]);
                        let mut user = [0; 4];
                        socket.read_exact(&mut user).unwrap();
                        assert_eq!(&user, b"user");
                        let mut length = [0];
                        socket.read_exact(&mut length).unwrap();
                        assert_eq!(length[0], 4);
                        let mut password = [0; 4];
                        socket.read_exact(&mut password).unwrap();
                        assert_eq!(&password, b"pass");
                        socket.write_all(&[1, u8::from(reject)]).unwrap();
                        if reject {
                            return;
                        }
                        let mut header = [0; 5];
                        socket.read_exact(&mut header).unwrap();
                        assert_eq!(&header[..4], &[5, 1, 0, 3]);
                        let mut host = vec![0; usize::from(header[4])];
                        socket.read_exact(&mut host).unwrap();
                        assert_eq!(host, b"broker.invalid");
                        let mut port = [0; 2];
                        socket.read_exact(&mut port).unwrap();
                        assert_eq!(u16::from_be_bytes(port), 1883);
                        socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
                    } else {
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            socket.read_exact(&mut byte).unwrap();
                            request.push(byte[0]);
                            assert!(request.len() < 8192);
                        }
                        let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                        assert!(request.starts_with("connect broker.invalid:1883 http/1.1\r\n"));
                        assert!(request.contains("proxy-authorization: basic dxnlcjpwyxnz"));
                        socket.write_all(if reject { b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n" } else { b"HTTP/1.1 200 Connection Established\r\n\r\n" }).unwrap();
                        if reject {
                            return;
                        }
                    }
                    assert_eq!(read_frame(&mut socket)[0], 0x10);
                    socket
                        .write_all(if mqtt5 {
                            &[0x20, 3, 0, 0, 0]
                        } else {
                            &[0x20, 2, 0, 0]
                        })
                        .unwrap();
                    assert_eq!(read_frame(&mut socket)[0], 0xe0);
                });
                let mut config = if mqtt5 {
                    ClientConfig::v5("proxy", "broker.invalid", 1883)
                } else {
                    ClientConfig::v4("proxy", "broker.invalid", 1883)
                };
                let credentials = Some(ProxyCredentials {
                    username: "user".into(),
                    password: "pass".into(),
                });
                config.common.proxy = Some(if socks {
                    ProxyConfig::Socks5 {
                        host: "127.0.0.1".into(),
                        port,
                        credentials,
                    }
                } else {
                    ProxyConfig::Http {
                        host: "127.0.0.1".into(),
                        port,
                        credentials,
                        tls: None,
                    }
                });
                let mut native = NativeClient::start(config).unwrap();
                let mut events = native.take_events().unwrap();
                let event = events
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .unwrap();
                if reject {
                    let WrapperEvent::Disconnected { error, .. } = event else {
                        panic!("expected connection failure")
                    };
                    assert!(!format!("{error:?}").contains("pass"));
                } else {
                    assert!(matches!(event, WrapperEvent::Connected { .. }));
                }
                native.closer().close_now(Duration::from_secs(3)).unwrap();
                proxy.join().unwrap();
            }
        }
    }
}

#[cfg(feature = "websocket")]
#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback's error type"
)]
fn websocket_headers_preserve_order_and_are_reapplied_on_reconnect() {
    use rumqttc_wrapper_core::{BrokerTarget, TransportConfig, WebSocketHeader};
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = thread::spawn(move || {
            for reconnect in [false, true] {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut socket = tungstenite::accept_hdr(
                    socket,
                    |request: &tungstenite::handshake::server::Request,
                     mut response: tungstenite::handshake::server::Response| {
                        assert_eq!(request.uri().path(), "/mqtt");
                        let values: Vec<_> = request
                            .headers()
                            .get_all("x-test")
                            .iter()
                            .map(|value| value.to_str().unwrap())
                            .collect();
                        assert_eq!(values, ["first", "second"]);
                        assert!(!request.headers().contains_key("x-removed"));
                        assert_eq!(request.headers()["authorization"], "Bearer secret-token");
                        response
                            .headers_mut()
                            .insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                        Ok(response)
                    },
                )
                .unwrap();
                assert_eq!(socket.read().unwrap().into_data()[0], 0x10);
                socket
                    .send(tungstenite::Message::Binary(Bytes::from_static(if mqtt5 {
                        &[0x20, 3, 0, 0, 0]
                    } else {
                        &[0x20, 2, 0, 0]
                    })))
                    .unwrap();
                if reconnect {
                    assert_eq!(socket.read().unwrap().into_data()[0], 0xe0);
                }
            }
        });
        let mut config = if mqtt5 {
            ClientConfig::v5("ws", "unused", 1)
        } else {
            ClientConfig::v4("ws", "unused", 1)
        };
        config.common.broker = BrokerTarget::WebSocket {
            url: format!("ws://127.0.0.1:{port}/mqtt"),
        };
        config.common.transport = TransportConfig::WebSocket;
        config.common.websocket_headers = vec![
            WebSocketHeader::Replace {
                name: "x-test".into(),
                value: "first".into(),
            },
            WebSocketHeader::Append {
                name: "x-test".into(),
                value: "second".into(),
            },
            WebSocketHeader::Append {
                name: "x-removed".into(),
                value: "discard".into(),
            },
            WebSocketHeader::Remove {
                name: "x-removed".into(),
            },
            WebSocketHeader::Replace {
                name: "authorization".into(),
                value: "Bearer secret-token".into(),
            },
        ];
        assert!(!format!("{config:?}").contains("secret-token"));
        let mut native = NativeClient::start(config).unwrap();
        let mut events = native.take_events().unwrap();
        let mut connected = 0;
        while connected != 2 {
            if matches!(
                events
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .unwrap(),
                WrapperEvent::Connected { .. }
            ) {
                connected += 1;
            }
        }
        native.closer().close_now(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
    }
}
