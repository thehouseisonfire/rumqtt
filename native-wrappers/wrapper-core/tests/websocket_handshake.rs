#![cfg(feature = "websocket")]
mod support;
#[path = "support/tls.rs"]
mod tls;
use rumqttc_wrapper_core::*;
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use support::*;

struct Tokens(Arc<AtomicUsize>);
impl WebSocketHandshake for Tokens {
    fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        let generation = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(request.attempt, generation as u64);
        assert_eq!(request.method, "GET");
        assert_eq!(request.version, "HTTP/1.1");
        assert_eq!(request.path_and_query, "/initial");
        assert_eq!(request.broker_host, "localhost");
        assert_eq!(
            request.dial_target,
            format!("localhost:{}", request.broker_port)
        );
        assert!(
            request.tls_authority.is_none()
                || request.tls_authority.as_deref() == Some("localhost")
        );
        assert_eq!(
            request.uri.split('/').nth(2).unwrap(),
            format!("localhost:{}", request.broker_port)
        );
        assert!(request.deadline > std::time::Instant::now());
        assert_eq!(
            request
                .headers
                .iter()
                .filter(|h| h.name == "x-static")
                .map(|h| h.value.as_ref())
                .collect::<Vec<_>>(),
            vec![b"one".as_slice(), b"two".as_slice()]
        );
        assert!(!format!("{request:?}").contains("secret"));
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let mut response = WebSocketHandshakeResponse::default();
            response
                .set_authority(&format!("customer-{generation}.example:8443"))
                .unwrap();
            response
                .set_path_and_query(&format!("/mqtt?token=secret-{generation}&encoded=%2F"))
                .unwrap();
            response
                .replace_header(
                    "authorization",
                    format!("Bearer secret-{generation}").as_bytes(),
                )
                .unwrap();
            response.replace_header("x-static", b"three").unwrap();
            response.append_header("x-static", b"").unwrap();
            response.append_header("x-octets", &[0xe9]).unwrap();
            response.remove_header("x-remove").unwrap();
            response
                .append_header(
                    "x-signed-target",
                    format!("GET:customer-{generation}.example:8443:/mqtt?token=secret-{generation}&encoded=%2F").as_bytes(),
                )
                .unwrap();
            Ok(response)
        })
    }
}

#[test]
#[expect(clippy::result_large_err, reason = "tungstenite callback API")]
fn dynamic_tokens_and_request_targets_refresh_across_ws_and_wss_reconnects() {
    let tls = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for encrypted in [false, true] {
            if encrypted
                && !cfg!(any(
                    feature = "use-rustls-no-provider",
                    feature = "use-native-tls"
                ))
            {
                continue;
            }
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tls.server.clone();
            let broker = Broker::spawn(move || {
                for generation in 1..=2 {
                    let socket = accept(&listener);
                    let socket: Box<dyn tls::Duplex> = if encrypted {
                        tls::wrap(Box::new(socket), server.clone())
                    } else {
                        Box::new(socket)
                    };
                    let mut ws = tungstenite::accept_hdr(socket, |request: &tungstenite::handshake::server::Request, mut response: tungstenite::handshake::server::Response| {
                        let path = format!("/mqtt?token=secret-{generation}&encoded=%2F");
                        assert_eq!(request.uri().path_and_query().unwrap().as_str(), path);
                        assert_eq!(request.headers()["authorization"], format!("Bearer secret-{generation}"));
                        let authority = format!("customer-{generation}.example:8443");
                        assert_eq!(request.headers()["host"], authority);
                        assert_eq!(request.headers().get_all("host").iter().count(), 1);
                        assert_eq!(request.headers()["x-signed-target"], format!("GET:{authority}:{path}"));
                        assert_eq!(request.headers().get_all("x-static").iter().map(tungstenite::http::HeaderValue::as_bytes).collect::<Vec<_>>(), vec![b"three".as_slice(), b"".as_slice()]);
                        assert_eq!(request.headers()["x-octets"].as_bytes(), &[0xe9]);
                        assert!(!request.headers().contains_key("x-remove"));
                        response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                        Ok(response)
                    }).unwrap();
                    assert_eq!(ws.read().unwrap().into_data()[0], 0x10);
                    ws.send(tungstenite::Message::Binary(bytes::Bytes::copy_from_slice(
                        if mqtt5 {
                            &[0x20, 3, 0, 0, 0]
                        } else {
                            &[0x20, 2, 0, 0]
                        },
                    )))
                    .unwrap();
                    if generation == 2 {
                        assert_eq!(ws.read().unwrap().into_data()[0], 0xe0);
                    }
                }
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let mut config = config(mqtt5, port);
            config.common.broker = BrokerTarget::WebSocket {
                url: format!(
                    "{}://localhost:{port}/initial",
                    if encrypted { "wss" } else { "ws" }
                ),
            };
            config.common.transport = if encrypted {
                TransportConfig::Wss(tls.client(if cfg!(feature = "use-rustls-no-provider") {
                    TlsBackend::Rustls
                } else {
                    TlsBackend::Native
                }))
            } else {
                TransportConfig::WebSocket
            };
            config.common.websocket_headers = vec![
                WebSocketHeader::Append {
                    name: "x-static".into(),
                    value: "one".into(),
                },
                WebSocketHeader::Append {
                    name: "x-static".into(),
                    value: "two".into(),
                },
                WebSocketHeader::Append {
                    name: "x-remove".into(),
                    value: "remove".into(),
                },
            ];
            config.common.websocket_handshake =
                Some(WebSocketHandshakeConfig(Arc::new(Tokens(calls.clone()))));
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            client.closer().close(DEADLINE).unwrap();
            broker.join();
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
}

struct Pending;
impl WebSocketHandshake for Pending {
    fn prepare(&self, _: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        Box::pin(std::future::pending())
    }
}

struct PendingDropPanic(std::sync::mpsc::Sender<()>);
struct PendingDropPanicFuture(Option<std::sync::mpsc::Sender<()>>);
impl std::future::Future for PendingDropPanicFuture {
    type Output = std::result::Result<WebSocketHandshakeResponse, WebSocketHandshakeFailure>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(entered) = self.0.take() {
            entered.send(()).unwrap();
        }
        std::task::Poll::Pending
    }
}
impl Drop for PendingDropPanicFuture {
    fn drop(&mut self) {
        panic!("private-cancelled-destructor-panic");
    }
}
impl WebSocketHandshake for PendingDropPanic {
    fn prepare(&self, _: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        Box::pin(PendingDropPanicFuture(Some(self.0.clone())))
    }
}

#[test]
fn pending_handshake_destructor_failures_are_terminal_on_timeout_and_close() {
    for mqtt5 in [false, true] {
        for close in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (entered, ready) = std::sync::mpsc::channel();
            let mut config = config(mqtt5, port);
            config.common.broker = BrokerTarget::WebSocket {
                url: format!("ws://127.0.0.1:{port}/initial"),
            };
            config.common.transport = TransportConfig::WebSocket;
            config.common.connection_timeout = if close {
                DEADLINE * 2
            } else {
                Duration::from_secs(1)
            };
            config.common.websocket_handshake = Some(WebSocketHandshakeConfig(Arc::new(
                PendingDropPanic(entered),
            )));
            let mut client = NativeClient::start(config).unwrap();
            let _socket = accept(&listener);
            ready.recv_timeout(DEADLINE).unwrap();
            let pending = client
                .handle()
                .try_admit(Command::Subscribe(SubscribeCommand {
                    filters: vec![Subscription {
                        filter: "pending".into(),
                        qos: QoS::AtLeastOnce,
                        protocol: SubscriptionProtocolOptions::VersionNeutral,
                    }],
                    protocol: SubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            let mut events = client.take_events().unwrap();
            if close {
                client.closer().close_now(DEADLINE).unwrap();
            }
            let terminal = until(&mut events, |event| {
                matches!(
                    event,
                    WrapperEvent::DriverTerminated(_) | WrapperEvent::ImmediateShutdownCompleted
                )
            });
            let WrapperEvent::DriverTerminated(error) = terminal else {
                panic!("shutdown suppressed the handshake destructor failure");
            };
            assert_eq!(
                error.websocket_failure(),
                Some(WebSocketHandshakeFailure::Panic)
            );
            assert!(!error.retryable());
            let error = pending.completion.wait_timeout(DEADLINE).unwrap_err();
            assert_eq!(
                error.websocket_failure(),
                Some(WebSocketHandshakeFailure::Panic)
            );
            client.closer().close_now(DEADLINE).unwrap();
        }
    }
}

#[path = "support/custom_transport.rs"]
mod custom;

struct Snapshot(std::sync::mpsc::Sender<WebSocketHandshakeRequest>);
impl WebSocketHandshake for Snapshot {
    fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        self.0.send(request).unwrap();
        Box::pin(std::future::ready(Err(WebSocketHandshakeFailure::Rejected)))
    }
}

#[test]
fn ipv6_handshake_snapshots_match_direct_and_proxy_connector_targets() {
    use std::io::{Read, Write};
    let tls = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for proxy in [false, true] {
            if proxy && !cfg!(feature = "http-proxy") {
                continue;
            }
            for encrypted in [false, true] {
                if encrypted
                    && !cfg!(any(
                        feature = "use-rustls-no-provider",
                        feature = "use-native-tls"
                    ))
                {
                    continue;
                }
                for connector in [false, true] {
                    let listener = TcpListener::bind("[::1]:0").unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let broker_port = if proxy { 1883 } else { port };
                    let (snapshots, received) = std::sync::mpsc::channel();
                    let mut config = config(mqtt5, broker_port);
                    config.common.broker = BrokerTarget::WebSocket {
                        url: format!(
                            "{}://[::1]:{broker_port}/mqtt",
                            if encrypted { "wss" } else { "ws" }
                        ),
                    };
                    config.common.transport = if encrypted {
                        TransportConfig::Wss(tls.client(
                            if cfg!(feature = "use-rustls-no-provider") {
                                TlsBackend::Rustls
                            } else {
                                TlsBackend::Native
                            },
                        ))
                    } else {
                        TransportConfig::WebSocket
                    };
                    if proxy {
                        config.common.proxy = Some(ProxyConfig::Http {
                            host: "::1".into(),
                            port,
                            credentials: None,
                            tls: None,
                        });
                    }
                    let (custom, requests) = custom::configured();
                    if connector {
                        config.common.connector = Some(custom);
                    }
                    config.common.websocket_handshake =
                        Some(WebSocketHandshakeConfig(Arc::new(Snapshot(snapshots))));
                    let client = NativeClient::start(config).unwrap();
                    let mut socket = accept(&listener);
                    if proxy {
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            socket.read_exact(&mut byte).unwrap();
                            request.push(byte[0]);
                            assert!(request.len() < 8192);
                        }
                        socket
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .unwrap();
                    }
                    let snapshot = received.recv_timeout(DEADLINE).unwrap();
                    client.closer().close_now(DEADLINE).unwrap();
                    let expected = format!("[::1]:{port}");
                    assert_eq!(snapshot.dial_target, expected);
                    assert_eq!(
                        snapshot
                            .dial_target
                            .parse::<std::net::SocketAddr>()
                            .unwrap(),
                        listener.local_addr().unwrap()
                    );
                    assert_eq!(snapshot.broker_host, "::1");
                    assert_eq!(snapshot.broker_port, broker_port);
                    assert_eq!(
                        snapshot.tls_authority.as_deref(),
                        encrypted.then_some("::1")
                    );
                    if connector {
                        let requests = requests.lock().unwrap();
                        assert!(!requests.is_empty());
                        assert!(
                            requests
                                .iter()
                                .all(|request| request.target == snapshot.dial_target)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn pending_handshakes_use_native_deadline_and_close_cancels_them() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = config(mqtt5, port);
        config.common.broker = BrokerTarget::WebSocket {
            url: format!("ws://127.0.0.1:{port}/initial"),
        };
        config.common.transport = TransportConfig::WebSocket;
        config.common.connection_timeout = Duration::from_secs(1);
        config.common.websocket_handshake = Some(WebSocketHandshakeConfig(Arc::new(Pending)));
        let mut client = NativeClient::start(config).unwrap();
        let _socket = accept(&listener);
        let mut events = client.take_events().unwrap();
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(
            error.websocket_failure(),
            Some(WebSocketHandshakeFailure::Timeout)
        );
        assert!(error.retryable());
        client.closer().close_now(DEADLINE).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::ImmediateShutdownCompleted)
        });
    }
}

#[test]
fn builders_protect_upgrade_fields_and_reject_invalid_or_oversized_edits() {
    let mut response = WebSocketHandshakeResponse::default();
    for name in [
        "Host",
        "Connection",
        "Upgrade",
        "content-length",
        "transfer-encoding",
        "Sec-WebSocket-Protocol",
    ] {
        assert_eq!(
            response.replace_header(name, b"secret"),
            Err(WebSocketHandshakeFailure::InvalidResponse)
        );
    }
    for path in [
        "https://other.invalid/mqtt",
        "mqtt",
        "/mqtt#fragment",
        "/mqtt\r\nx: injected",
    ] {
        assert!(response.set_path_and_query(path).is_err());
    }
    assert!(response.append_header("x-ok", b"a\r\nb: injected").is_err());
    assert_eq!(
        response.append_header("x-ok", &vec![b'x'; MAX_WEBSOCKET_BYTES]),
        Err(WebSocketHandshakeFailure::ResourceLimit)
    );
    assert!(!format!("{response:?}").contains("secret"));
}

#[derive(Clone, Copy)]
enum PanicStage {
    Construction,
    Poll,
    Destruction,
}
struct Panics(PanicStage);
struct PanicFuture(PanicStage);
impl std::future::Future for PanicFuture {
    type Output = std::result::Result<WebSocketHandshakeResponse, WebSocketHandshakeFailure>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if matches!(self.0, PanicStage::Poll) {
            panic!("private-callback-panic");
        }
        std::task::Poll::Ready(Ok(WebSocketHandshakeResponse::default()))
    }
}
impl Drop for PanicFuture {
    fn drop(&mut self) {
        if matches!(self.0, PanicStage::Destruction) {
            panic!("private-destructor-panic");
        }
    }
}
impl WebSocketHandshake for Panics {
    fn prepare(&self, _: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        if matches!(self.0, PanicStage::Construction) {
            panic!("private-construction-panic");
        }
        Box::pin(PanicFuture(self.0))
    }
}
#[test]
fn panicking_handshake_authorities_terminate_with_a_typed_failure() {
    for mqtt5 in [false, true] {
        for stage in [
            PanicStage::Construction,
            PanicStage::Poll,
            PanicStage::Destruction,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut config = config(mqtt5, port);
            config.common.broker = BrokerTarget::WebSocket {
                url: format!("ws://127.0.0.1:{port}/initial"),
            };
            config.common.transport = TransportConfig::WebSocket;
            config.common.websocket_handshake =
                Some(WebSocketHandshakeConfig(Arc::new(Panics(stage))));
            let mut client = NativeClient::start(config).unwrap();
            let _socket = accept(&listener);
            let mut events = client.take_events().unwrap();
            let event = until(&mut events, |e| {
                matches!(e, WrapperEvent::DriverTerminated(_))
            });
            let WrapperEvent::DriverTerminated(error) = event else {
                unreachable!()
            };
            assert_eq!(
                error.websocket_failure(),
                Some(WebSocketHandshakeFailure::Panic)
            );
            assert!(!error.retryable());
            client.closer().close_now(DEADLINE).unwrap();
        }
    }
}

#[test]
fn handshake_panic_payloads_do_not_reach_process_output() {
    for test in [
        "panicking_handshake_authorities_terminate_with_a_typed_failure",
        "pending_handshake_destructor_failures_are_terminal_on_timeout_and_close",
    ] {
        let output = process_output(
            std::process::Command::new(std::env::current_exe().unwrap()).args([
                "--exact",
                test,
                "--nocapture",
            ]),
        );
        assert!(output.status.success());
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for secret in [
            "private-callback-panic",
            "private-destructor-panic",
            "private-cancelled-destructor-panic",
            "private-construction-panic",
        ] {
            assert!(!output.contains(secret), "panic payload leaked");
        }
    }
}
