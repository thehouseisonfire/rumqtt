use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    BrokerTarget, ClientConfig, ErrorKind, IncomingPacketLimit, LastWillConfig,
    LastWillProtocolOptions, NativeClient, ProtocolConfig, QoS, SecretBytes, TlsBackend,
    TlsClientIdentity, TlsConfig, TransportConfig, V5WillProperties, WebSocketHeader,
};

fn will() -> LastWillConfig {
    LastWillConfig {
        topic: "last/will".into(),
        payload: Bytes::from_static(b"\0\xff"),
        qos: QoS::ExactlyOnce,
        retain: true,
        protocol: LastWillProtocolOptions::VersionNeutral,
    }
}

#[test]
fn will_protocol_is_checked_before_start() {
    let mut config = ClientConfig::v4("will", "127.0.0.1", 1);
    let mut value = will();
    value.protocol = LastWillProtocolOptions::V5(V5WillProperties::default());
    config.common.last_will = Some(value);
    assert_eq!(
        NativeClient::start(config).unwrap_err().kind(),
        ErrorKind::Configuration
    );
}

#[test]
fn will_topics_and_binary_boundaries_are_validated() {
    for mqtt5 in [false, true] {
        let mut config = if mqtt5 {
            ClientConfig::v5("will", "localhost", 1)
        } else {
            ClientConfig::v4("will", "localhost", 1)
        };
        for topic in ["", "a/+", "#", "a\0b"] {
            let mut value = will();
            value.topic = topic.into();
            config.common.last_will = Some(value);
            assert!(config.validate().is_err());
        }
        for length in [0, 65535, 65536] {
            let mut value = will();
            value.payload = Bytes::from(vec![0xff; length]);
            config.common.last_will = Some(value);
            assert_eq!(config.validate().is_ok(), length <= 65535);
        }
    }
}

#[test]
fn will_property_boundaries_preserve_legal_empty_and_zero_values() {
    let mut config = ClientConfig::v5("will", "localhost", 1);
    let check = |properties: V5WillProperties| {
        let mut value = will();
        value.protocol = LastWillProtocolOptions::V5(properties);
        value
    };
    let legal = V5WillProperties {
        will_delay_interval: Some(0),
        message_expiry_interval: Some(0),
        content_type: Some(String::new()),
        correlation_data: Some(Bytes::new()),
        user_properties: vec![(String::new(), String::new()); 2],
        ..Default::default()
    };
    config.common.last_will = Some(check(legal.clone()));
    config.validate().unwrap();
    for field in 0..5 {
        for length in [65535, 65536] {
            let mut properties = legal.clone();
            match field {
                0 => properties.content_type = Some("a".repeat(length)),
                1 => properties.response_topic = Some("a".repeat(length)),
                2 => properties.correlation_data = Some(Bytes::from(vec![0; length])),
                3 => properties.user_properties = vec![("a".repeat(length), String::new())],
                _ => properties.user_properties = vec![(String::new(), "a".repeat(length))],
            }
            config.common.last_will = Some(check(properties));
            assert_eq!(config.validate().is_ok(), length <= 65535);
        }
    }
    for indicator in [0, 1, 2, 255] {
        let mut properties = legal.clone();
        properties.payload_format_indicator = Some(indicator);
        config.common.last_will = Some(check(properties));
        assert_eq!(config.validate().is_ok(), indicator == 0);
    }
}

#[test]
fn limits_and_timer_ranges_fail_without_panicking() {
    let mut config = ClientConfig::v5("limits", "localhost", 1);
    for limit in [
        IncomingPacketLimit::Default,
        IncomingPacketLimit::Unlimited,
        IncomingPacketLimit::Bytes(u32::MAX),
        IncomingPacketLimit::Bytes(0),
    ] {
        config.common.incoming_packet_size_limit = limit;
        assert_eq!(
            config.validate().is_ok(),
            limit != IncomingPacketLimit::Bytes(0)
        );
    }
    config.common.incoming_packet_size_limit = IncomingPacketLimit::Default;
    for duration in [
        Duration::from_secs(u64::MAX),
        Duration::from_millis(1),
        Duration::ZERO,
    ] {
        config.common.connection_timeout = duration;
        assert!(config.validate().is_err());
    }
    config.common.connection_timeout = Duration::from_secs(17);
    config.common.pending_throttle = Duration::from_nanos(3);
    config.common.max_request_batch = usize::MAX;
    config.common.read_batch_size = usize::MAX;
    config.validate().unwrap();
}

#[test]
fn connect_property_singletons_and_authentication_pairing_are_validated() {
    for field in 0..5 {
        let mut config = ClientConfig::v5("connect", "localhost", 1);
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        match field {
            0 => v5.connect_properties.receive_maximum = Some(0),
            1 => v5.connect_properties.maximum_packet_size = Some(0),
            2 => v5.connect_properties.request_response_information = Some(2),
            3 => v5.connect_properties.request_problem_information = Some(2),
            _ => v5.connect_properties.authentication_data = Some(Bytes::new()),
        }
        assert_eq!(
            config.validate().unwrap_err().kind(),
            ErrorKind::Configuration
        );
    }
}

#[test]
fn sensitive_debug_and_tls_pairing() {
    let tls = TlsConfig {
        backend: TlsBackend::Native,
        identity: Some(TlsClientIdentity::NativePkcs12 {
            identity: SecretBytes::new(b"identity-secret".to_vec()),
            password: SecretBytes::new(b"password-secret".to_vec()),
        }),
        ..Default::default()
    };
    let debug = format!("{tls:?}");
    assert!(!debug.contains("identity-secret"));
    assert!(!debug.contains("password-secret"));
    let mut config = ClientConfig::v4("tls", "localhost", 1);
    config.common.transport = TransportConfig::Tls(TlsConfig {
        alpn_protocols: vec![vec![]],
        ..Default::default()
    });
    assert!(config.validate().is_err());
}

#[test]
fn websocket_headers_reject_protected_names_and_injection() {
    for name in [
        "Host",
        "connection",
        "SEC-WEBSOCKET-KEY",
        "transfer-encoding",
        "bad name",
    ] {
        let mut config = ClientConfig::v5("headers", "localhost", 1);
        config.common.broker = BrokerTarget::WebSocket {
            url: "ws://localhost:1/mqtt".into(),
        };
        config.common.transport = TransportConfig::WebSocket;
        config.common.websocket_headers = vec![WebSocketHeader::Remove { name: name.into() }];
        assert!(config.validate().is_err());
    }
    let header = WebSocketHeader::Append {
        name: "authorization".into(),
        value: "secret\r\nX: y".into(),
    };
    assert!(!format!("{header:?}").contains("secret"));
    let mut config = ClientConfig::v4("headers", "localhost", 1);
    config.common.broker = BrokerTarget::WebSocket {
        url: "ws://localhost:1/mqtt".into(),
    };
    config.common.transport = TransportConfig::WebSocket;
    config.common.websocket_headers = vec![header];
    assert!(config.validate().is_err());
}

#[test]
fn endpoint_transport_mismatches_are_rejected() {
    let mut config = ClientConfig::v4("unix", "localhost", 1);
    config.common.broker = BrokerTarget::Unix {
        path: "/tmp/mqtt.sock".into(),
    };
    assert!(config.validate().is_err());
    config.common.transport = TransportConfig::Unix;
    assert_eq!(config.validate().is_ok(), cfg!(unix));
}
