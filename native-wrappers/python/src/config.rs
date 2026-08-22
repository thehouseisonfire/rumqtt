use base64::Engine as _;
use bytes::Bytes;
use rumqttc_wrapper_core::{AckMode, ClientConfig, ProtocolConfig, TlsConfig, TransportConfig};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    protocol: String,
    broker_host: String,
    broker_port: u16,
    client_id: String,
    transport: Transport,
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

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Transport {
    Tcp,
    Tls {
        #[serde(rename = "caBase64")]
        ca: Option<String>,
        #[serde(rename = "clientCertificateBase64")]
        cert: Option<String>,
        #[serde(rename = "privateKeyBase64")]
        key: Option<String>,
    },
    Websocket {
        url: String,
    },
    Wss {
        url: String,
        #[serde(rename = "caBase64")]
        ca: Option<String>,
        #[serde(rename = "clientCertificateBase64")]
        cert: Option<String>,
        #[serde(rename = "privateKeyBase64")]
        key: Option<String>,
    },
}

pub fn parse(value: &str) -> Result<ClientConfig, String> {
    let input: Input = serde_json::from_str(value)
        .map_err(|e| format!("invalid native client configuration: {e}"))?;
    let mut config = match input.protocol.as_str() {
        "3.1.1" => ClientConfig::v4(input.client_id, input.broker_host, input.broker_port),
        "5.0" => ClientConfig::v5(input.client_id, input.broker_host, input.broker_port),
        _ => return Err("protocol must be '3.1.1' or '5.0'".into()),
    };
    config.common.transport = match input.transport {
        Transport::Tcp => TransportConfig::Tcp,
        Transport::Tls { ca, cert, key } => TransportConfig::Tls(tls(ca, cert, key)?),
        Transport::Websocket { url } => TransportConfig::WebSocket { url },
        Transport::Wss { url, ca, cert, key } => TransportConfig::Wss {
            url,
            tls: tls(ca, cert, key)?,
        },
    };
    config.common.keep_alive = Duration::from_secs(input.keep_alive_seconds);
    config.common.connection_timeout = Duration::from_secs(input.connection_timeout_seconds);
    config.common.username = input.username;
    config.common.password = input
        .password_base64
        .map(|v| decode(&v).map(Bytes::from))
        .transpose()?;
    config.common.request_channel_capacity = input.request_capacity;
    config.common.event_buffer_capacity = input.event_capacity;
    config.common.event_delivery_timeout = Duration::from_millis(input.event_delivery_timeout_ms);
    config.common.ack_mode = match input.ack_mode.as_str() {
        "automatic" => AckMode::Automatic,
        "manual" => AckMode::Manual,
        _ => return Err("invalid acknowledgement mode".into()),
    };
    config.common.incoming_packet_size_limit = input.incoming_packet_size_limit;
    config.common.emit_outgoing_events = input.emit_outgoing_events;
    match &mut config.protocol {
        ProtocolConfig::V4(v4) => {
            if input.clean_start.is_some() || input.session_expiry_interval.is_some() {
                return Err("MQTT 5 session options require protocol '5.0'".into());
            }
            v4.clean_session = input.clean_session.unwrap_or(true);
        }
        ProtocolConfig::V5(v5) => {
            if input.clean_session.is_some() {
                return Err("cleanSession is only valid for protocol '3.1.1'".into());
            }
            v5.clean_start = input.clean_start.unwrap_or(true);
            v5.session_expiry_interval = input.session_expiry_interval;
        }
    }
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

fn tls(ca: Option<String>, cert: Option<String>, key: Option<String>) -> Result<TlsConfig, String> {
    Ok(TlsConfig {
        ca: ca.map(|v| decode(&v).map(Bytes::from)).transpose()?,
        client_certificate: cert.map(|v| decode(&v).map(Bytes::from)).transpose()?,
        private_key: key.map(|v| decode(&v).map(Bytes::from)).transpose()?,
    })
}
fn decode(value: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|e| format!("invalid base64 data: {e}"))
}
