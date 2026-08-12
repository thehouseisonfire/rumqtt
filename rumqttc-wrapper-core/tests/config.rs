use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{ClientConfig, ErrorKind, NativeClient, TlsConfig, TransportConfig};

#[test]
fn rejects_password_without_username() {
    let mut config = ClientConfig::v311("client", "localhost", 1883);
    config.common.password = Some(Bytes::from_static(b"secret"));
    assert_eq!(
        config.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn accepts_password_without_username_for_mqtt5() {
    let mut config = ClientConfig::v5("client", "localhost", 1883);
    config.common.password = Some(Bytes::from_static(b"secret"));
    config.validate().unwrap();
}

#[test]
fn rejects_null_characters_in_connect_strings() {
    let client_id = ClientConfig::v5("client\0id", "localhost", 1883);
    assert_eq!(
        client_id.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );

    let mut username = ClientConfig::v5("client", "localhost", 1883);
    username.common.username = Some("user\0name".into());
    assert_eq!(
        username.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn rejects_oversized_connect_fields() {
    let client_id = ClientConfig::v5("é".repeat(32_768), "localhost", 1883);
    assert_eq!(
        client_id.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );

    let mut username = ClientConfig::v5("client", "localhost", 1883);
    username.common.username = Some("a".repeat(usize::from(u16::MAX) + 1));
    assert_eq!(
        username.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );

    let mut password = ClientConfig::v5("client", "localhost", 1883);
    password.common.password = Some(Bytes::from(vec![0; usize::from(u16::MAX) + 1]));
    assert_eq!(
        password.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn accepts_maximum_connect_field_lengths() {
    let mut config = ClientConfig::v5("a".repeat(usize::from(u16::MAX)), "localhost", 1883);
    config.common.username = Some("b".repeat(usize::from(u16::MAX)));
    config.common.password = Some(Bytes::from(vec![0; usize::from(u16::MAX)]));
    config.validate().unwrap();
}

#[test]
fn rejects_lossy_duration_conversion() {
    let mut config = ClientConfig::v5("client", "localhost", 1883);
    config.common.keep_alive = Duration::from_millis(1500);
    assert_eq!(
        config.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn rejects_unpaired_client_tls_material() {
    let mut config = ClientConfig::v5("client", "localhost", 8883);
    config.common.transport = TransportConfig::Tls(TlsConfig {
        client_certificate: Some(Bytes::from_static(b"certificate")),
        ..TlsConfig::default()
    });
    assert_eq!(
        config.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn malformed_tls_material_fails_before_driver_start() {
    let mut config = ClientConfig::v5("client", "localhost", 8883);
    config.common.transport = TransportConfig::Tls(TlsConfig {
        ca: Some(Bytes::from_static(b"not a PEM certificate")),
        ..TlsConfig::default()
    });
    assert_eq!(
        NativeClient::start(config).unwrap_err().kind(),
        ErrorKind::Tls
    );
}

#[test]
fn rejects_protocol_inappropriate_websocket_scheme() {
    let mut config = ClientConfig::v311("client", "localhost", 8080);
    config.common.transport = TransportConfig::WebSocket {
        url: "wss://localhost:8080/mqtt".into(),
    };
    assert_eq!(
        config.validate().unwrap_err().kind(),
        ErrorKind::Configuration
    );
}
