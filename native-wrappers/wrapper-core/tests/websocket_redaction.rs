#![cfg(feature = "websocket")]
mod support;
use rumqttc_wrapper_core::*;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::*;

static LOGS: Mutex<String> = Mutex::new(String::new());
struct Logs;
impl log::Log for Logs {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("rumqttc") || metadata.target().starts_with("mqtt.wrapper")
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            writeln!(LOGS.lock().unwrap(), "{}", record.args()).unwrap();
        }
    }
    fn flush(&self) {}
}
struct Secret;
impl WebSocketHandshake for Secret {
    fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        assert!(!format!("{request:?}").contains("/initial"));
        Box::pin(async {
            let mut response = WebSocketHandshakeResponse::default();
            response.set_authority("private-authority.example:8443")?;
            response.set_path_and_query("/mqtt?signature=private-query-token")?;
            response.append_header("authorization", b"Bearer private-auth-token")?;
            response.append_header("x-signature", b"private-signature-token")?;
            assert!(!format!("{response:?}").contains("private-"));
            Ok(response)
        })
    }
}

#[test]
#[expect(clippy::result_large_err, reason = "tungstenite callback API")]
fn wrapper_diagnostics_redact_handshake_credentials() {
    log::set_logger(&Logs).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    for mqtt5 in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = Broker::spawn(move || {
            let mut ws = tungstenite::accept_hdr(
                accept(&listener),
                |_: &tungstenite::handshake::server::Request,
                 mut response: tungstenite::handshake::server::Response| {
                    response
                        .headers_mut()
                        .insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                    Ok(response)
                },
            )
            .unwrap();
            ws.read().unwrap();
            ws.send(tungstenite::Message::Binary(bytes::Bytes::copy_from_slice(
                if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                },
            )))
            .unwrap();
            assert_eq!(ws.read().unwrap().into_data()[0], 0xe0);
        });
        let mut config = config(mqtt5, port);
        config.common.broker = BrokerTarget::WebSocket {
            url: format!("ws://127.0.0.1:{port}/initial"),
        };
        config.common.transport = TransportConfig::WebSocket;
        config.common.websocket_handshake = Some(WebSocketHandshakeConfig(Arc::new(Secret)));
        let debug = format!("{config:?}");
        let mut client = NativeClient::start(config).unwrap();
        let _events = connected(&mut client);
        client.closer().close(Duration::from_secs(3)).unwrap();
        broker.join();
        assert!(!debug.contains("private-"));
    }
    let logs = LOGS.lock().unwrap();
    assert!(!logs.is_empty(), "client diagnostics were not captured");
    for secret in [
        "private-authority.example",
        "private-query-token",
        "private-auth-token",
        "private-signature-token",
    ] {
        assert!(!logs.contains(secret), "credential leaked");
    }
    drop(logs);
}
