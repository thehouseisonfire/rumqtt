mod support;
#[path = "support/tls.rs"]
mod tls;

use std::io::Write;
use std::net::TcpListener;

use bytes::Bytes;
use rumqttc_wrapper_core::*;
use support::*;

const PROXY_USER: &str = "proxy-private-user";
const PROXY_PASSWORD: &str = "proxy-private-password";
const PROXY_BASIC: &str = "cHJveHktcHJpdmF0ZS11c2VyOnByb3h5LXByaXZhdGUtcGFzc3dvcmQ=";

#[test]
fn proxy_failure_process_output_is_redacted() {
    let output = process_output(
        std::process::Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "proxy_negotiation_failures_and_shutdown_resolve_pending_work",
            "--nocapture",
        ]),
    );
    assert!(output.status.success(), "proxy failure subprocess failed");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        PROXY_USER,
        PROXY_PASSWORD,
        PROXY_BASIC,
        "ws-secret",
        "cookie-secret",
    ] {
        assert!(!output.contains(secret), "secret leaked to process output");
    }
}

fn headers() -> Vec<WebSocketHeader> {
    vec![
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
        WebSocketHeader::Append {
            name: "x-remove".into(),
            value: "discard".into(),
        },
        WebSocketHeader::Remove {
            name: "x-remove".into(),
        },
        WebSocketHeader::Replace {
            name: "authorization".into(),
            value: "Bearer ws-secret".into(),
        },
        WebSocketHeader::Replace {
            name: "cookie".into(),
            value: "cookie-secret".into(),
        },
    ]
}

fn proxy_config(socks: bool, port: u16, tls: Option<TlsConfig>) -> ProxyConfig {
    let credentials = Some(ProxyCredentials {
        username: PROXY_USER.into(),
        password: PROXY_PASSWORD.into(),
    });
    if socks {
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
            tls,
        }
    }
}

fn proxy_handshake(stream: &mut impl tls::Duplex, socks: bool, host: &str, reject: bool) {
    if socks {
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting[0], 5);
        let mut methods = vec![0; usize::from(greeting[1])];
        stream.read_exact(&mut methods).unwrap();
        assert!(methods.contains(&2));
        stream.write_all(&[5, 2]).unwrap();
        stream.flush().unwrap();
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting[0], 1);
        let mut username = vec![0; usize::from(greeting[1])];
        stream.read_exact(&mut username).unwrap();
        assert_eq!(username, PROXY_USER.as_bytes());
        let mut length = [0];
        stream.read_exact(&mut length).unwrap();
        let mut password = vec![0; usize::from(length[0])];
        stream.read_exact(&mut password).unwrap();
        assert_eq!(password, PROXY_PASSWORD.as_bytes());
        stream.write_all(&[1, u8::from(reject)]).unwrap();
        stream.flush().unwrap();
        if reject {
            return;
        }
        let mut address = [0; 4];
        stream.read_exact(&mut address).unwrap();
        assert_eq!(&address[..3], &[5, 1, 0]);
        match address[3] {
            1 => {
                let mut ip = [0; 4];
                stream.read_exact(&mut ip).unwrap();
                assert_eq!(std::net::Ipv4Addr::from(ip).to_string(), host);
            }
            3 => {
                let mut length = [0];
                stream.read_exact(&mut length).unwrap();
                let mut name = vec![0; usize::from(length[0])];
                stream.read_exact(&mut name).unwrap();
                assert_eq!(name, host.as_bytes());
            }
            _ => panic!("unexpected SOCKS address type"),
        }
        let mut port = [0; 2];
        stream.read_exact(&mut port).unwrap();
        assert_eq!(u16::from_be_bytes(port), 1883);
        stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
    } else {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
        assert!(request.starts_with(&format!("connect {host}:1883 http/1.1\r\n")));
        assert!(request.contains(&format!(
            "proxy-authorization: basic {}",
            PROXY_BASIC.to_ascii_lowercase()
        )));
        stream
            .write_all(if reject {
                b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n"
            } else {
                b"HTTP/1.1 200 Connection Established\r\n\r\n"
            })
            .unwrap();
    }
    stream.flush().unwrap();
}

#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback error type"
)]
fn proxy_tls_and_websocket_compositions_reconnect_for_both_protocols() {
    capture::start();
    let broker_tls = tls::Fixture::new();
    let proxy_tls = tls::Fixture::new();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        let enabled = match backend {
            TlsBackend::Rustls => cfg!(feature = "use-rustls"),
            TlsBackend::Native => cfg!(feature = "use-native-tls"),
        };
        for mqtt5 in [false, true] {
            // A direct connection also covers WSS header edits without proxies.
            for proxy in ["direct", "http", "https", "socks"] {
                if (proxy == "http" || proxy == "https") && !cfg!(feature = "http-proxy") {
                    continue;
                }
                if proxy == "socks" && !cfg!(feature = "socks-proxy") {
                    continue;
                }
                if proxy == "https" && !enabled {
                    continue;
                }
                for transport in ["tcp", "tls", "ws", "wss"] {
                    let encrypted = transport == "tls" || transport == "wss";
                    let websocket = transport == "ws" || transport == "wss";
                    if encrypted && !enabled {
                        continue;
                    }
                    if websocket && !cfg!(feature = "websocket") {
                        continue;
                    }
                    if !encrypted && proxy != "https" && backend == TlsBackend::Native {
                        continue;
                    }
                    for hostname in [false, true] {
                        if proxy == "direct" && hostname {
                            continue;
                        }
                        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                        let port = listener.local_addr().unwrap().port();
                        let host = if proxy == "direct" {
                            "127.0.0.1"
                        } else if hostname {
                            "broker.invalid"
                        } else {
                            "127.0.0.2"
                        };
                        let broker_port = if proxy == "direct" { port } else { 1883 };
                        let mut config = config(mqtt5, broker_port);
                        config.common.broker = BrokerTarget::Tcp {
                            host: host.into(),
                            port: broker_port,
                        };
                        config.common.transport = match transport {
                            "tcp" => TransportConfig::Tcp,
                            "tls" => TransportConfig::Tls(broker_tls.client(backend)),
                            "ws" => TransportConfig::WebSocket,
                            "wss" => TransportConfig::Wss(broker_tls.client(backend)),
                            _ => unreachable!(),
                        };
                        if websocket {
                            config.common.broker = BrokerTarget::WebSocket {
                                url: format!(
                                    "{}://{host}:{broker_port}/mqtt?session=one",
                                    if encrypted { "wss" } else { "ws" }
                                ),
                            };
                            config.common.websocket_headers = headers();
                        }
                        if proxy != "direct" {
                            config.common.proxy = Some(proxy_config(
                                proxy == "socks",
                                port,
                                (proxy == "https").then(|| proxy_tls.client(backend)),
                            ));
                        }
                        let broker_server = broker_tls.server.clone();
                        let proxy_server = proxy_tls.server.clone();
                        let (release_tx, release_rx) = std::sync::mpsc::channel();
                        let broker = Broker::spawn(move || {
                            for generation in 0..2 {
                                let mut stream: Box<dyn tls::Duplex> = Box::new(accept(&listener));
                                if proxy == "https" {
                                    stream = tls::wrap(stream, proxy_server.clone());
                                }
                                if proxy != "direct" {
                                    proxy_handshake(&mut stream, proxy == "socks", host, false);
                                }
                                if encrypted {
                                    stream = tls::wrap(stream, broker_server.clone());
                                }
                                if websocket {
                                    let mut socket = tungstenite::accept_hdr(stream, |request: &tungstenite::handshake::server::Request, mut response: tungstenite::handshake::server::Response| {
                                        assert_eq!(request.uri().path_and_query().unwrap().as_str(), "/mqtt?session=one");
                                        let values: Vec<_> = request.headers().get_all("x-order").iter().map(|value| value.to_str().unwrap()).collect();
                                        assert_eq!(values, ["first", "second"]);
                                        assert!(!request.headers().contains_key("x-remove"));
                                        assert_eq!(request.headers()["authorization"], "Bearer ws-secret");
                                        assert_eq!(request.headers()["cookie"], "cookie-secret");
                                        response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                                        Ok(response)
                                    }).unwrap();
                                    assert_eq!(socket.read().unwrap().into_data()[0], 0x10);
                                    socket
                                        .send(tungstenite::Message::Binary(Bytes::from_static(
                                            if mqtt5 {
                                                &[0x20, 3, 0, 0, 0]
                                            } else {
                                                &[0x20, 2, 0, 0]
                                            },
                                        )))
                                        .unwrap();
                                    if generation == 0 {
                                        release_rx.recv_timeout(DEADLINE).unwrap();
                                    } else {
                                        assert_eq!(socket.read().unwrap().into_data()[0], 0xe0);
                                    }
                                } else {
                                    assert_eq!(frame(&mut stream)[0], 0x10);
                                    stream
                                        .write_all(if mqtt5 {
                                            &[0x20, 3, 0, 0, 0]
                                        } else {
                                            &[0x20, 2, 0, 0]
                                        })
                                        .unwrap();
                                    stream.flush().unwrap();
                                    if generation == 0 {
                                        release_rx.recv_timeout(DEADLINE).unwrap();
                                    } else {
                                        assert_eq!(frame(&mut stream)[0], 0xe0);
                                    }
                                }
                            }
                        });
                        capture::assert_redacted(
                            &format!("{config:?}"),
                            &["ws-secret", "cookie-secret"],
                        );
                        let mut client = NativeClient::start(config).unwrap();
                        let mut events = connected(&mut client);
                        release_tx.send(()).unwrap();
                        until(&mut events, |event| {
                            matches!(event, WrapperEvent::Disconnected { .. })
                        });
                        until(&mut events, |event| {
                            matches!(event, WrapperEvent::Connected { .. })
                        });
                        client.closer().close(DEADLINE).unwrap();
                        broker.join();
                        capture::assert_redacted(
                            "",
                            &[
                                "ws-secret",
                                "cookie-secret",
                                PROXY_USER,
                                PROXY_PASSWORD,
                                PROXY_BASIC,
                            ],
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn disabled_transports_fail_before_opening_a_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for mqtt5 in [false, true] {
        for backend in [TlsBackend::Rustls, TlsBackend::Native] {
            let enabled = match backend {
                TlsBackend::Rustls => cfg!(feature = "use-rustls"),
                TlsBackend::Native => cfg!(feature = "use-native-tls"),
            };
            if enabled {
                continue;
            }
            let mut config = config(mqtt5, listener.local_addr().unwrap().port());
            config.common.transport = TransportConfig::Tls(TlsConfig {
                backend,
                ..Default::default()
            });
            assert_eq!(
                NativeClient::start(config).unwrap_err().kind(),
                ErrorKind::Configuration
            );
        }
        if !cfg!(feature = "websocket") {
            let mut config = config(mqtt5, 1);
            config.common.broker = BrokerTarget::WebSocket {
                url: format!(
                    "ws://127.0.0.1:{}/mqtt",
                    listener.local_addr().unwrap().port()
                ),
            };
            config.common.transport = TransportConfig::WebSocket;
            config.common.websocket_headers = headers();
            assert_eq!(
                NativeClient::start(config).unwrap_err().kind(),
                ErrorKind::Configuration
            );
        }
    }
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn disabled_redirect_transports_fail_before_driver_start() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for (transport, enabled) in [
        (TransportConfig::WebSocket, cfg!(feature = "websocket")),
        (
            TransportConfig::Tls(TlsConfig {
                backend: TlsBackend::Rustls,
                ..Default::default()
            }),
            cfg!(feature = "use-rustls"),
        ),
        (
            TransportConfig::Tls(TlsConfig {
                backend: TlsBackend::Native,
                ..Default::default()
            }),
            cfg!(feature = "use-native-tls"),
        ),
        (
            TransportConfig::Wss(TlsConfig::default()),
            cfg!(all(feature = "websocket", feature = "use-rustls")),
        ),
    ] {
        if enabled {
            continue;
        }
        let mut config = config(true, listener.local_addr().unwrap().port());
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 1,
            transport,
        };
        assert_eq!(
            NativeClient::start(config).unwrap_err().kind(),
            ErrorKind::Configuration
        );
    }
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(all(
    feature = "http-proxy",
    any(feature = "use-rustls", feature = "use-native-tls")
))]
#[test]
fn proxy_and_broker_tls_trust_policies_are_independent() {
    capture::start();
    let proxy = tls::Fixture::new();
    let broker = tls::Fixture::new();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls"))
            || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mqtt5 in [false, true] {
            for wrong_proxy_roots in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut config = config(mqtt5, 1883);
                config.common.broker = BrokerTarget::Tcp {
                    host: "broker.invalid".into(),
                    port: 1883,
                };
                config.common.proxy = Some(proxy_config(
                    false,
                    listener.local_addr().unwrap().port(),
                    Some(if wrong_proxy_roots {
                        broker.client(backend)
                    } else {
                        proxy.client(backend)
                    }),
                ));
                config.common.transport = TransportConfig::Tls(if wrong_proxy_roots {
                    broker.client(backend)
                } else {
                    proxy.client(backend)
                });
                let proxy_server = proxy.server.clone();
                let broker_server = broker.server.clone();
                let peer = Broker::spawn(move || {
                    let mut stream = tls::wrap(Box::new(accept(&listener)), proxy_server);
                    if !wrong_proxy_roots {
                        proxy_handshake(&mut stream, false, "broker.invalid", false);
                        stream = tls::wrap(stream, broker_server);
                    }
                    assert!(
                        stream.read(&mut [0]).is_err(),
                        "untrusted TLS peer accepted"
                    );
                });
                let mut client = NativeClient::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                let event = until(&mut events, |event| {
                    matches!(event, WrapperEvent::Disconnected { .. })
                });
                let WrapperEvent::Disconnected { error, .. } = event else {
                    unreachable!()
                };
                assert_eq!(
                    error.kind(),
                    if wrong_proxy_roots {
                        ErrorKind::Network
                    } else {
                        ErrorKind::Tls
                    }
                );
                capture::assert_redacted(
                    &format!("{error} {error:?}"),
                    &[&broker.key_pem, &proxy.key_pem, PROXY_USER, PROXY_PASSWORD],
                );
                client.closer().close_now(DEADLINE).unwrap();
                peer.join();
            }
        }
    }
}

#[test]
fn proxy_negotiation_failures_and_shutdown_resolve_pending_work() {
    capture::start();
    for mqtt5 in [false, true] {
        for socks in [false, true] {
            if (socks && !cfg!(feature = "socks-proxy"))
                || (!socks && !cfg!(feature = "http-proxy"))
            {
                continue;
            }
            for mode in ["reject", "reset", "timeout", "shutdown"] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut config = config(mqtt5, 1883);
                config.common.broker = BrokerTarget::Tcp {
                    host: "broker.invalid".into(),
                    port: 1883,
                };
                config.common.connection_timeout = std::time::Duration::from_secs(1);
                config.common.proxy = Some(proxy_config(
                    socks,
                    listener.local_addr().unwrap().port(),
                    None,
                ));
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let broker = Broker::spawn(move || {
                    let mut socket = accept(&listener);
                    ready_tx.send(()).unwrap();
                    release_rx.recv_timeout(DEADLINE).unwrap();
                    if mode == "reject" {
                        proxy_handshake(&mut socket, socks, "broker.invalid", true);
                    } else if mode != "reset" {
                        use std::io::Read;
                        let mut bytes = [0; 1024];
                        while socket.read(&mut bytes).unwrap() != 0 {}
                    }
                });
                let mut client = NativeClient::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                ready_rx.recv_timeout(DEADLINE).unwrap();
                let operation = client
                    .handle()
                    .try_admit(Command::Publish(PublishCommand {
                        topic: "queued".into(),
                        payload: Bytes::new(),
                        qos: QoS::AtMostOnce,
                        retain: false,
                        protocol: PublishProtocolOptions::VersionNeutral,
                    }))
                    .unwrap();
                release_tx.send(()).unwrap();
                if mode != "shutdown" {
                    let event = until(&mut events, |event| {
                        matches!(event, WrapperEvent::Disconnected { .. })
                    });
                    let WrapperEvent::Disconnected { error, phase } = event else {
                        unreachable!()
                    };
                    assert_eq!(phase, ConnectionPhase::Attempt);
                    assert_eq!(
                        error.kind(),
                        if mode == "timeout" {
                            ErrorKind::Timeout
                        } else {
                            ErrorKind::Network
                        }
                    );
                    let output = format!("{error} {error:?}");
                    capture::assert_redacted(&output, &[PROXY_USER, PROXY_PASSWORD, PROXY_BASIC]);
                }
                client.closer().close_now(DEADLINE).unwrap();
                assert_eq!(
                    terminal(&operation).unwrap_err().delivery_status(),
                    DeliveryStatus::Ambiguous
                );
                broker.join();
            }
        }
    }
}
