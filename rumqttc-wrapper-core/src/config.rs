use std::time::Duration;

use bytes::Bytes;

use crate::{Error, ErrorKind, ProtocolVersion, Result};

const DEFAULT_REQUEST_CAPACITY: usize = 10;
const DEFAULT_EVENT_CAPACITY: usize = 256;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_INCOMING_LIMIT: u32 = 10 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AckMode {
    #[default]
    Automatic,
    Manual,
}

/// TLS inputs copied from the host runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsConfig {
    pub ca: Option<Bytes>,
    pub client_certificate: Option<Bytes>,
    pub private_key: Option<Bytes>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportConfig {
    Tcp,
    Tls(TlsConfig),
    WebSocket { url: String },
    Wss { url: String, tls: TlsConfig },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonConfig {
    pub broker_host: String,
    pub broker_port: u16,
    pub client_id: String,
    pub transport: TransportConfig,
    pub keep_alive: Duration,
    pub connection_timeout: Duration,
    pub username: Option<String>,
    pub password: Option<Bytes>,
    pub request_channel_capacity: usize,
    pub event_buffer_capacity: usize,
    pub event_delivery_timeout: Duration,
    pub ack_mode: AckMode,
    pub incoming_packet_size_limit: u32,
    pub emit_outgoing_events: bool,
}

impl CommonConfig {
    #[must_use]
    pub fn new(client_id: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            broker_host: host.into(),
            broker_port: port,
            client_id: client_id.into(),
            transport: TransportConfig::Tcp,
            keep_alive: Duration::from_secs(60),
            connection_timeout: DEFAULT_TIMEOUT,
            username: None,
            password: None,
            request_channel_capacity: DEFAULT_REQUEST_CAPACITY,
            event_buffer_capacity: DEFAULT_EVENT_CAPACITY,
            event_delivery_timeout: DEFAULT_TIMEOUT,
            ack_mode: AckMode::Automatic,
            incoming_packet_size_limit: DEFAULT_INCOMING_LIMIT,
            emit_outgoing_events: false,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.broker_host.is_empty() {
            return Err(Error::configuration("broker host must not be empty"));
        }
        if self.broker_port == 0 {
            return Err(Error::configuration("broker port must be nonzero"));
        }
        if self.request_channel_capacity == 0 || self.event_buffer_capacity == 0 {
            return Err(Error::configuration("channel capacities must be nonzero"));
        }
        if self.event_delivery_timeout.is_zero() || self.connection_timeout.is_zero() {
            return Err(Error::configuration("timeouts must be nonzero"));
        }
        if self.incoming_packet_size_limit == 0 {
            return Err(Error::configuration(
                "incoming packet size limit must be nonzero",
            ));
        }
        duration_seconds(self.keep_alive, "keep alive")?;
        duration_seconds(self.connection_timeout, "connection timeout")?;
        validate_mqtt_utf8_string(&self.client_id, "client identifier")?;
        if let Some(username) = &self.username {
            validate_mqtt_utf8_string(username, "username")?;
        }
        if let Some(password) = &self.password
            && password.len() > usize::from(u16::MAX)
        {
            return Err(Error::configuration(format!(
                "password exceeds the MQTT binary-data limit of {} bytes",
                u16::MAX,
            )));
        }
        let tls = match &self.transport {
            TransportConfig::Tls(tls) | TransportConfig::Wss { tls, .. } => Some(tls),
            TransportConfig::Tcp | TransportConfig::WebSocket { .. } => None,
        };
        if let Some(tls) = tls
            && tls.client_certificate.is_some() != tls.private_key.is_some()
        {
            return Err(Error::configuration(
                "TLS client certificate and private key must be supplied together",
            ));
        }
        match &self.transport {
            TransportConfig::WebSocket { url } if !url.starts_with("ws://") => {
                return Err(Error::configuration("WebSocket URL must use ws://"));
            }
            TransportConfig::Wss { url, .. } if !url.starts_with("wss://") => {
                return Err(Error::configuration("secure WebSocket URL must use wss://"));
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V4Config {
    pub clean_session: bool,
}

impl Default for V4Config {
    fn default() -> Self {
        Self {
            clean_session: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V5Config {
    pub clean_start: bool,
    pub session_expiry_interval: Option<u32>,
}

impl Default for V5Config {
    fn default() -> Self {
        Self {
            clean_start: true,
            session_expiry_interval: None,
        }
    }
}

/// Protocol and protocol-specific session behavior selected for one client.
///
/// The selected variant remains fixed for the lifetime of the resulting [`crate::NativeClient`].
/// The wrapper does not negotiate, fall back to, or switch protocol versions. Construct another
/// client with the other variant to use a different MQTT version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolConfig {
    /// MQTT 3.1.1 configuration.
    V4(V4Config),
    /// MQTT 5 configuration.
    V5(V5Config),
}

/// Owned configuration for one client using exactly one MQTT protocol version.
///
/// [`Self::protocol`] is an explicit, immutable per-client selection. Common options are stored in
/// [`Self::common`], while settings whose semantics differ between MQTT 3.1.1 and MQTT 5 remain in
/// the selected [`ProtocolConfig`] variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    pub common: CommonConfig,
    pub protocol: ProtocolConfig,
}

impl ClientConfig {
    #[must_use]
    pub fn v4(client_id: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            common: CommonConfig::new(client_id, host, port),
            protocol: ProtocolConfig::V4(V4Config {
                clean_session: true,
            }),
        }
    }

    #[must_use]
    pub fn v5(client_id: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            common: CommonConfig::new(client_id, host, port),
            protocol: ProtocolConfig::V5(V5Config::default()),
        }
    }

    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        match self.protocol {
            ProtocolConfig::V4(_) => ProtocolVersion::V4,
            ProtocolConfig::V5(_) => ProtocolVersion::V5,
        }
    }

    /// Validates protocol-neutral and protocol-specific configuration invariants.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when any option is invalid or inconsistent with another
    /// option.
    pub fn validate(&self) -> Result<()> {
        self.common.validate()?;
        if matches!(self.protocol, ProtocolConfig::V4(_))
            && self.common.password.is_some()
            && self.common.username.is_none()
        {
            return Err(Error::configuration(
                "an MQTT 3.1.1 password requires a username",
            ));
        }
        if matches!(
            self.protocol,
            ProtocolConfig::V4(V4Config {
                clean_session: false
            })
        ) && self.common.client_id.is_empty()
        {
            return Err(Error::configuration(
                "MQTT 3.1.1 persistent sessions require a client identifier",
            ));
        }
        Ok(())
    }
}

fn validate_mqtt_utf8_string(value: &str, name: &str) -> Result<()> {
    if value.len() > usize::from(u16::MAX) {
        return Err(Error::configuration(format!(
            "{name} exceeds the MQTT UTF-8 string limit of {} bytes",
            u16::MAX,
        )));
    }
    if value.contains('\0') {
        return Err(Error::configuration(format!(
            "{name} cannot contain the null character U+0000",
        )));
    }
    Ok(())
}

pub fn duration_seconds(duration: Duration, name: &str) -> Result<u64> {
    if duration.subsec_nanos() != 0 {
        return Err(Error::new(
            ErrorKind::Configuration,
            format!("{name} must be an integral number of seconds"),
        ));
    }
    Ok(duration.as_secs())
}
