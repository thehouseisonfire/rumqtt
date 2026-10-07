#![cfg(feature = "websocket")]
mod support;
#[path = "support/tls.rs"]
mod tls;

use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rumqttc_wrapper_core::*;
use support::*;

fn websocket_config(mqtt5: bool, port: u16, encrypted: bool, tls: &tls::Fixture) -> ClientConfig {
    let mut config = config(mqtt5, port);
    let scheme = if encrypted { "wss" } else { "ws" };
    config.common.broker = BrokerTarget::WebSocket {
        url: format!("{scheme}://localhost:{port}/mqtt"),
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
    config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
        initial_delay: Duration::from_millis(5),
        maximum_delay: Duration::from_millis(5),
        jitter: ReconnectJitter::None,
        budget: RetryBudget::Limited(1),
        ..ReconnectConfig::default()
    });
    config
}

#[expect(clippy::result_large_err, reason = "tungstenite callback API")]
fn connect_websocket(
    listener: &TcpListener,
    mqtt5: bool,
    server: Option<Arc<rustls::ServerConfig>>,
) -> tungstenite::WebSocket<Box<dyn tls::Duplex>> {
    let socket: Box<dyn tls::Duplex> = Box::new(accept(listener));
    let socket = if let Some(server) = server {
        tls::wrap(socket, server)
    } else {
        socket
    };
    let mut websocket = tungstenite::accept_hdr(
        socket,
        |_: &tungstenite::handshake::server::Request,
         mut response: tungstenite::handshake::server::Response| {
            response
                .headers_mut()
                .insert("sec-websocket-protocol", "mqtt".parse().unwrap());
            Ok(response)
        },
    )
    .unwrap();
    assert_eq!(websocket.read().unwrap().into_data()[0], 0x10);
    websocket
        .send(tungstenite::Message::Binary(bytes::Bytes::copy_from_slice(
            if mqtt5 {
                &[0x20, 3, 0, 0, 0]
            } else {
                &[0x20, 2, 0, 0]
            },
        )))
        .unwrap();
    websocket
}

const fn encrypted_supported() -> bool {
    cfg!(any(
        feature = "use-rustls-no-provider",
        feature = "use-native-tls"
    ))
}

#[test]
fn websocket_protocol_violations_stop_classified_clients_and_preserve_legacy_retries() {
    let tls = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for encrypted in [false, true] {
            if encrypted && !encrypted_supported() {
                continue;
            }
            for classified in [true, false] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut config = websocket_config(
                    mqtt5,
                    listener.local_addr().unwrap().port(),
                    encrypted,
                    &tls,
                );
                config.common.reconnect = if classified {
                    ReconnectPolicy::Classified(ReconnectConfig::default())
                } else {
                    ReconnectPolicy::Legacy
                };
                let broker_listener = listener.try_clone().unwrap();
                let server = encrypted.then(|| tls.server.clone());
                let (send_frame, receive_frame) = mpsc::channel();
                let (release, hold) = mpsc::channel();
                let broker = std::thread::spawn(move || {
                    let mut websocket = connect_websocket(&broker_listener, mqtt5, server);
                    receive_frame.recv_timeout(DEADLINE).unwrap();
                    // FIN with reserved opcode 3, unmasked as required for server frames.
                    // Keep the stream open so a subsequent EOF cannot mask the violation.
                    websocket.get_mut().write_all(&[0x83, 0]).unwrap();
                    websocket.get_mut().flush().unwrap();
                    let _ = hold.recv_timeout(DEADLINE);
                });
                let mut client = start(config).unwrap();
                let mut events = connected(&mut client);
                send_frame.send(()).unwrap();
                let event = until(&mut events, |event| {
                    matches!(
                        event,
                        WrapperEvent::Disconnected { .. } | WrapperEvent::DriverTerminated(_)
                    )
                });
                if classified {
                    let WrapperEvent::DriverTerminated(error) = event else {
                        panic!("protocol violation retried: {event:?}");
                    };
                    assert!(!error.retryable());
                    assert_ne!(error.code(), ErrorCode::ReconnectExhausted);
                    let snapshot = client.handle().reconnect_diagnostics();
                    assert_eq!(snapshot.phase, ReconnectPhase::Stopped);
                    assert_eq!(snapshot.stop_reason, ReconnectStopReason::TerminalFailure);
                    assert_eq!(snapshot.cycles_started, 1);
                    assert_eq!(snapshot.retries_since_reset, 0);
                    assert!(!snapshot.last_failure.unwrap().retryable());
                    listener.set_nonblocking(true).unwrap();
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                } else {
                    assert!(matches!(event, WrapperEvent::Disconnected { .. }));
                    client.closer().close_now(DEADLINE).unwrap();
                }
                release.send(()).unwrap();
                broker.join().unwrap();
            }
        }
    }
}

#[test]
fn websocket_peer_closure_reconnects_for_both_protocols() {
    let tls = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for encrypted in [false, true] {
            if encrypted && !encrypted_supported() {
                continue;
            }
            for clean_close in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let config = websocket_config(
                    mqtt5,
                    listener.local_addr().unwrap().port(),
                    encrypted,
                    &tls,
                );
                let server = encrypted.then(|| tls.server.clone());
                let (disconnect, receive) = mpsc::channel();
                let broker = std::thread::spawn(move || {
                    let mut first = connect_websocket(&listener, mqtt5, server.clone());
                    receive.recv_timeout(DEADLINE).unwrap();
                    if clean_close {
                        first.close(None).unwrap();
                    }
                    drop(first);
                    let mut second = connect_websocket(&listener, mqtt5, server);
                    assert_eq!(second.read().unwrap().into_data()[0], 0xe0);
                });
                let mut client = start(config).unwrap();
                let mut events = connected(&mut client);
                disconnect.send(()).unwrap();
                let event = until(&mut events, |event| {
                    matches!(event, WrapperEvent::Disconnected { .. })
                });
                let WrapperEvent::Disconnected { phase, error } = event else {
                    unreachable!()
                };
                assert_eq!(phase, ConnectionPhase::Established);
                assert!(error.retryable());
                until(&mut events, |event| {
                    matches!(event, WrapperEvent::Connected { .. })
                });
                let snapshot = client.handle().reconnect_diagnostics();
                assert_eq!(snapshot.cycles_started, 2);
                assert_eq!(snapshot.retries_since_reset, 1);
                client.closer().close(DEADLINE).unwrap();
                broker.join().unwrap();
            }
        }
    }
}
