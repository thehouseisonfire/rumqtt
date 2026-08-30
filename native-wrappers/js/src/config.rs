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
        TransportInput::Websocket { url } => TransportConfig::WebSocket { url },
        TransportInput::Wss {
            url,
            ca_base64,
            client_certificate_base64,
            private_key_base64,
        } => TransportConfig::Wss {
            url,
            tls: tls(ca_base64, client_certificate_base64, private_key_base64)?,
        },
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
    config.common.incoming_packet_size_limit = input.incoming_packet_size_limit;
    config.common.emit_outgoing_events = input.emit_outgoing_events;
    match &mut config.protocol {
        ProtocolConfig::V4(protocol) => {
            if input.clean_start.is_some() || input.session_expiry_interval.is_some() {
                return Err("MQTT 5 session options require protocol '5.0'".to_owned());
            }
            protocol.clean_session = input.clean_session.unwrap_or(true);
        }
        ProtocolConfig::V5(protocol) => {
            if input.clean_session.is_some() {
                return Err("cleanSession is only valid for protocol '3.1.1'".to_owned());
            }
            protocol.clean_start = input.clean_start.unwrap_or(true);
            protocol.session_expiry_interval = input.session_expiry_interval;
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
    Ok(TlsConfig {
        ca: ca
            .map(|value| decode(&value, "TLS CA").map(Bytes::from))
            .transpose()?,
        client_certificate: certificate
            .map(|value| decode(&value, "TLS client certificate").map(Bytes::from))
            .transpose()?,
        private_key: private_key
            .map(|value| decode(&value, "TLS private key").map(Bytes::from))
            .transpose()?,
    })
}

fn decode(value: &str, name: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| format!("invalid base64 {name}: {error}"))
}
