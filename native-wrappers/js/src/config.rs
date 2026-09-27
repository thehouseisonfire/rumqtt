use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use rumqttc_wrapper_core::{AckMode, ClientConfig, ProtocolConfig, TlsConfig, TransportConfig};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigInput {
    protocol: String,
    broker_host: String,
    broker_port: u16,
    client_id: String,
    transport: TransportInput,
    keep_alive_seconds: u64,
    connection_timeout_seconds: u64,
    username: Option<String>,
    password_base64: Option<String>,
    request_capacity: usize,
    event_capacity: usize,
    event_delivery_timeout_ms: u64,
    ack_mode: String,
    incoming_packet_size_limit: u32,
    emit_outgoing_events: bool,
    clean_session: Option<bool>,
    session_present_mismatch_policy: Option<String>,
    clean_start: Option<bool>,
    session_expiry_interval: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum TransportInput {
    Tcp,
    Tls {
        #[serde(rename = "caBase64")]
        ca_base64: Option<String>,
        #[serde(rename = "clientCertificateBase64")]
        client_certificate_base64: Option<String>,
        #[serde(rename = "privateKeyBase64")]
        private_key_base64: Option<String>,
    },
    Websocket {
        url: String,
    },
    Wss {
        url: String,
        #[serde(rename = "caBase64")]
        ca_base64: Option<String>,
        #[serde(rename = "clientCertificateBase64")]
        client_certificate_base64: Option<String>,
        #[serde(rename = "privateKeyBase64")]
        private_key_base64: Option<String>,
    },
}

pub fn parse(input: &str) -> Result<ClientConfig, String> {
    let input: ConfigInput = serde_json::from_str(input)
        .map_err(|error| format!("invalid native client configuration: {error}"))?;
    let mut config = match input.protocol.as_str() {
        "3.1.1" => ClientConfig::v4(input.client_id, input.broker_host, input.broker_port),
        "5.0" => ClientConfig::v5(input.client_id, input.broker_host, input.broker_port),
        _ => return Err("protocol must be '3.1.1' or '5.0'".to_owned()),
    };
    config.common.transport = match input.transport {
        TransportInput::Tcp => TransportConfig::Tcp,
        TransportInput::Tls {
            ca_base64,
            client_certificate_base64,
            private_key_base64,
        } => TransportConfig::Tls(tls(
            ca_base64,
            client_certificate_base64,
            private_key_base64,
        )?),
        TransportInput::Websocket { url } => {
            config.common.broker = rumqttc_wrapper_core::BrokerTarget::WebSocket { url };
            TransportConfig::WebSocket
        }
        TransportInput::Wss {
            url,
            ca_base64,
            client_certificate_base64,
            private_key_base64,
        } => {
            config.common.broker = rumqttc_wrapper_core::BrokerTarget::WebSocket { url };
            TransportConfig::Wss(tls(
                ca_base64,
                client_certificate_base64,
                private_key_base64,
            )?)
        }
    };
    config.common.keep_alive = Duration::from_secs(input.keep_alive_seconds);
    config.common.connection_timeout = Duration::from_secs(input.connection_timeout_seconds);
    config.common.username = input.username;
    config.common.password = input
        .password_base64
        .map(|value| decode(&value, "password").map(Bytes::from))
        .transpose()?;
    config.common.request_channel_capacity = input.request_capacity;
    config.common.event_buffer_capacity = input.event_capacity;
    config.common.event_delivery_timeout = Duration::from_millis(input.event_delivery_timeout_ms);
    config.common.ack_mode = match input.ack_mode.as_str() {
        "automatic" => AckMode::Automatic,
        "manual" => AckMode::Manual,
        _ => return Err("ackMode must be 'automatic' or 'manual'".to_owned()),
    };
    config.common.incoming_packet_size_limit =
        rumqttc_wrapper_core::IncomingPacketLimit::Bytes(input.incoming_packet_size_limit);
    config.common.emit_outgoing_events = input.emit_outgoing_events;
    match &mut config.protocol {
        ProtocolConfig::V4(protocol) => {
            if input.clean_start.is_some() || input.session_expiry_interval.is_some() {
                return Err("MQTT 5 session options require protocol '5.0'".to_owned());
            }
            protocol.clean_session = input.clean_session.unwrap_or(true);
            protocol.session_present_mismatch_policy = match input
                .session_present_mismatch_policy
                .as_deref()
            {
                None | Some("error") => rumqttc_wrapper_core::SessionPresentMismatchPolicy::Error,
                Some("acceptAsClean") => {
                    rumqttc_wrapper_core::SessionPresentMismatchPolicy::AcceptAsClean
                }
                _ => {
                    return Err(
                        "sessionPresentMismatchPolicy must be 'error' or 'acceptAsClean'"
                            .to_owned(),
                    );
                }
            };
        }
        ProtocolConfig::V5(protocol) => {
            if input.session_present_mismatch_policy.is_some() {
                return Err(
                    "sessionPresentMismatchPolicy is only valid for protocol '3.1.1'".to_owned(),
                );
            }
            if input.clean_session.is_some() {
                return Err("cleanSession is only valid for protocol '3.1.1'".to_owned());
            }
            protocol.clean_start = input.clean_start.unwrap_or(true);
            protocol.connect_properties.session_expiry_interval = input.session_expiry_interval;
            protocol.connect_properties.maximum_packet_size =
                Some(input.incoming_packet_size_limit);
        }
    }
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn tls(
    ca: Option<String>,
    certificate: Option<String>,
    private_key: Option<String>,
) -> Result<TlsConfig, String> {
    TlsConfig::rustls_pem(
        ca.map(|value| decode(&value, "TLS CA").map(Bytes::from))
            .transpose()?,
        certificate
            .map(|value| decode(&value, "TLS client certificate").map(Bytes::from))
            .transpose()?,
        private_key
            .map(|value| decode(&value, "TLS private key"))
            .transpose()?,
    )
    .map_err(|error| error.to_string())
}

fn decode(value: &str, name: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| format!("invalid base64 {name}: {error}"))
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    fn input(protocol: &str, policy: Option<&str>) -> serde_json::Value {
        let mut input = serde_json::json!({
            "protocol": protocol, "brokerHost": "localhost", "brokerPort": 1883,
            "clientId": "compatibility", "transport": { "kind": "tcp" },
            "keepAliveSeconds": 60, "connectionTimeoutSeconds": 5,
            "requestCapacity": 10, "eventCapacity": 256, "eventDeliveryTimeoutMs": 5000,
            "ackMode": "automatic", "incomingPacketSizeLimit": 10240,
            "emitOutgoingEvents": false,
        });
        if let Some(policy) = policy {
            input["sessionPresentMismatchPolicy"] = serde_json::json!(policy);
        }
        input
    }

    #[test]
    fn session_present_policy_defaults_and_protocol_validation() {
        use rumqttc_wrapper_core::SessionPresentMismatchPolicy::{AcceptAsClean, Error};
        for (policy, expected) in [
            (None, Error),
            (Some("error"), Error),
            (Some("acceptAsClean"), AcceptAsClean),
        ] {
            let config = parse(&input("3.1.1", policy).to_string()).unwrap();
            let ProtocolConfig::V4(v4) = config.protocol else {
                panic!("expected v4")
            };
            assert_eq!(v4.session_present_mismatch_policy, expected);
        }
        assert!(parse(&input("3.1.1", Some("ignore")).to_string()).is_err());
        for policy in ["error", "acceptAsClean"] {
            assert!(parse(&input("5.0", Some(policy)).to_string()).is_err());
        }
        assert!(parse(&input("5.0", None).to_string()).is_ok());
    }
}
