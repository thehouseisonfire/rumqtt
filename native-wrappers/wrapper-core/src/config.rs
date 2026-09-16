use std::time::Duration;

use bytes::Bytes;

use crate::{Error, ErrorKind, LastWillConfig, ProtocolVersion, Result};

const DEFAULT_REQUEST_CAPACITY: usize = 10;
const DEFAULT_EVENT_CAPACITY: usize = 256;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Local decoder limit, independent of MQTT 5's advertised Maximum Packet Size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IncomingPacketLimit {
    /// Preserve the wrapper's 10 KiB local limit.
    #[default]
    Default,
    Bytes(u32),
    Unlimited,
}

/// Portable socket tuning. Connection timeout is configured on [`CommonConfig`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkConfig {
    pub tcp_send_buffer_size: Option<u32>,
    pub tcp_receive_buffer_size: Option<u32>,
    pub tcp_nodelay: bool,
    pub local_address: Option<std::net::SocketAddr>,
    /// Supported on Linux, Android, and Fuchsia only.
    pub bind_device: Option<String>,
    /// Supported on Linux only; follows the backend's TCP fallback policy.
    pub mptcp: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AckMode {
    #[default]
    Automatic,
    Manual,
}

/// TLS inputs copied from the host runtime.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TlsConfig {
    /// Explicit backend selection, including in builds that enable both backends.
    pub backend: TlsBackend,
    pub roots: TlsRootPolicy,
    pub identity: Option<TlsClientIdentity>,
    pub alpn_protocols: Vec<Vec<u8>>,
}

/// Owned secret storage, wiped when each owned copy is dropped. Backend TLS
/// libraries control the lifetime and erasure of their parsed key material.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(zeroize::Zeroizing<Vec<u8>>);

impl SecretBytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(zeroize::Zeroizing::new(bytes))
    }
    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub enum TlsRootPolicy {
    #[default]
    Platform,
    /// Replaces (does not augment) platform roots.
    Pem(Bytes),
}

impl std::fmt::Debug for TlsRootPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Platform => "Platform",
            Self::Pem(_) => "Pem([REDACTED])",
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum TlsClientIdentity {
    RustlsPem {
        certificate: Bytes,
        private_key: SecretBytes,
    },
    NativePkcs12 {
        identity: SecretBytes,
        password: SecretBytes,
    },
}

impl std::fmt::Debug for TlsClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RustlsPem { .. } => "RustlsPem([REDACTED])",
            Self::NativePkcs12 { .. } => "NativePkcs12([REDACTED])",
        })
    }
}

impl TlsConfig {
    /// Behavior-preserving migration from the original PEM-oriented fields.
    /// A supplied CA replaces platform roots; a certificate and key must be paired.
    pub fn rustls_pem(
        ca: Option<Bytes>,
        certificate: Option<Bytes>,
        key: Option<Vec<u8>>,
    ) -> Result<Self> {
        let identity = match (certificate, key.map(SecretBytes::new)) {
            (None, None) => None,
            (Some(certificate), Some(private_key)) => Some(TlsClientIdentity::RustlsPem {
                certificate,
                private_key,
            }),
            _ => {
                return Err(Error::configuration(
                    "TLS client certificate and private key must be supplied together",
                ));
            }
        };
        Ok(Self {
            roots: ca.map_or(TlsRootPolicy::Platform, TlsRootPolicy::Pem),
            identity,
            ..Self::default()
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsBackend {
    #[default]
    Rustls,
    Native,
}

impl std::fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConfig")
            .field("backend", &self.backend)
            .field("roots", &self.roots)
            .field("identity", &self.identity)
            .field("alpn_protocols", &self.alpn_protocols)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportConfig {
    Tcp,
    Tls(TlsConfig),
    Unix,
    WebSocket,
    Wss(TlsConfig),
}

/// An endpoint with no unused TCP address fields for Unix or WebSocket targets.
#[derive(Clone, PartialEq, Eq)]
pub enum BrokerTarget {
    Tcp { host: String, port: u16 },
    Unix { path: std::path::PathBuf },
    WebSocket { url: String },
}

impl std::fmt::Debug for BrokerTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp { host, port } => f
                .debug_struct("Tcp")
                .field("host", host)
                .field("port", port)
                .finish(),
            Self::Unix { path } => f.debug_struct("Unix").field("path", path).finish(),
            // URLs can carry query tokens or user information.
            Self::WebSocket { .. } => f.write_str("WebSocket([REDACTED])"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CommonConfig {
    pub broker: BrokerTarget,
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
    pub incoming_packet_size_limit: IncomingPacketLimit,
    /// Zero preserves legacy single-request processing.
    pub max_request_batch: usize,
    /// Zero selects adaptive network batching.
    pub read_batch_size: usize,
    /// Delay between pending retransmissions; zero disables throttling.
    pub pending_throttle: Duration,
    pub network: NetworkConfig,
    pub last_will: Option<LastWillConfig>,
    pub proxy: Option<crate::ProxyConfig>,
    pub websocket_headers: Vec<crate::WebSocketHeader>,
    pub emit_outgoing_events: bool,
}

impl std::fmt::Debug for CommonConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonConfig")
            .field("broker", &self.broker)
            .field("client_id", &self.client_id)
            .field("transport", &self.transport)
            .field("keep_alive", &self.keep_alive)
            .field("connection_timeout", &self.connection_timeout)
            .field("username", &self.username.as_ref().map(|_| "[REDACTED]"))
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("request_channel_capacity", &self.request_channel_capacity)
            .field("event_buffer_capacity", &self.event_buffer_capacity)
            .field("ack_mode", &self.ack_mode)
            .field(
                "incoming_packet_size_limit",
                &self.incoming_packet_size_limit,
            )
            .field("max_request_batch", &self.max_request_batch)
            .field("read_batch_size", &self.read_batch_size)
            .field("pending_throttle", &self.pending_throttle)
            .field("network", &self.network)
            .field("last_will", &self.last_will.as_ref().map(|_| "[REDACTED]"))
            .field("proxy", &self.proxy)
            .field("websocket_headers", &self.websocket_headers)
            .finish_non_exhaustive()
    }
}

impl CommonConfig {
    #[must_use]
    pub fn new(client_id: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            broker: BrokerTarget::Tcp {
                host: host.into(),
                port,
            },
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
            incoming_packet_size_limit: IncomingPacketLimit::Default,
            max_request_batch: 0,
            read_batch_size: 0,
            pending_throttle: Duration::ZERO,
            network: NetworkConfig::default(),
            last_will: None,
            proxy: None,
            websocket_headers: Vec::new(),
            emit_outgoing_events: false,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(proxy) = &self.proxy {
            proxy.validate()?;
            if let crate::ProxyConfig::Http { tls: Some(tls), .. } = proxy {
                // Reuse the same TLS validation for the independent proxy layer.
                let mut proxy_config = Self::new("proxy-validation", "proxy", 1);
                proxy_config.transport = TransportConfig::Tls(tls.clone());
                proxy_config.validate()?;
            }
        }
        if !self.websocket_headers.is_empty()
            && !matches!(
                self.transport,
                TransportConfig::WebSocket | TransportConfig::Wss(_)
            )
        {
            return Err(Error::configuration(
                "WebSocket headers require a WebSocket transport",
            ));
        }
        for header in &self.websocket_headers {
            header.validate()?;
        }
        match (&self.broker, &self.transport) {
            (BrokerTarget::Tcp { host, port }, TransportConfig::Tcp | TransportConfig::Tls(_)) => {
                if host.is_empty() || host.contains(['\0', '\r', '\n']) || *port == 0 {
                    return Err(Error::configuration("invalid TCP broker endpoint"));
                }
            }
            (BrokerTarget::Unix { path }, TransportConfig::Unix) => {
                if !cfg!(unix) {
                    return Err(Error::configuration(
                        "Unix sockets are unsupported on this platform",
                    ));
                }
                if path.as_os_str().is_empty() || path.as_os_str().as_encoded_bytes().contains(&0) {
                    return Err(Error::configuration("invalid Unix socket path"));
                }
                if self.proxy.is_some() || self.network != NetworkConfig::default() {
                    return Err(Error::configuration(
                        "Unix sockets cannot use proxy or TCP network options",
                    ));
                }
            }
            (BrokerTarget::WebSocket { url }, TransportConfig::WebSocket)
                if url.starts_with("ws://") => {}
            (BrokerTarget::WebSocket { url }, TransportConfig::Wss(_))
                if url.starts_with("wss://") => {}
            _ => {
                return Err(Error::configuration(
                    "broker target is incompatible with transport",
                ));
            }
        }
        if self.request_channel_capacity == 0 || self.event_buffer_capacity == 0 {
            return Err(Error::configuration("channel capacities must be nonzero"));
        }
        if self.event_delivery_timeout.is_zero() || self.connection_timeout.is_zero() {
            return Err(Error::configuration("timeouts must be nonzero"));
        }
        if self.incoming_packet_size_limit == IncomingPacketLimit::Bytes(0) {
            return Err(Error::configuration(
                "incoming packet size limit must be nonzero",
            ));
        }
        duration_seconds(self.keep_alive, "keep alive")?;
        if self.keep_alive.as_secs() > u64::from(u16::MAX) {
            return Err(Error::configuration("keep alive exceeds 65535 seconds"));
        }
        duration_seconds(self.connection_timeout, "connection timeout")?;
        for duration in [
            self.connection_timeout,
            self.event_delivery_timeout,
            self.pending_throttle,
        ] {
            if std::time::Instant::now().checked_add(duration).is_none() {
                return Err(Error::configuration(
                    "duration exceeds the platform timer range",
                ));
            }
        }
        if self.network.tcp_send_buffer_size == Some(0)
            || self.network.tcp_receive_buffer_size == Some(0)
        {
            return Err(Error::configuration("TCP buffer sizes must be nonzero"));
        }
        if let Some(device) = &self.network.bind_device {
            if !cfg!(any(
                target_os = "linux",
                target_os = "android",
                target_os = "fuchsia"
            )) {
                return Err(Error::configuration(
                    "bind-device is unsupported on this platform",
                ));
            }
            if device.is_empty() || device.contains('\0') {
                return Err(Error::configuration(
                    "bind-device must be nonempty and contain no NUL",
                ));
            }
        }
        if self.network.mptcp && !cfg!(target_os = "linux") {
            return Err(Error::configuration(
                "MPTCP is unsupported on this platform",
            ));
        }
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
            TransportConfig::Tls(tls) | TransportConfig::Wss(tls) => Some(tls),
            TransportConfig::Tcp | TransportConfig::WebSocket | TransportConfig::Unix => None,
        };
        if let Some(tls) = tls {
            let enabled = match tls.backend {
                TlsBackend::Rustls => cfg!(feature = "use-rustls"),
                TlsBackend::Native => cfg!(feature = "use-native-tls"),
            };
            if !enabled {
                return Err(Error::configuration("selected TLS backend is disabled"));
            }
            if matches!(
                (&tls.backend, &tls.identity),
                (
                    TlsBackend::Rustls,
                    Some(TlsClientIdentity::NativePkcs12 { .. })
                ) | (
                    TlsBackend::Native,
                    Some(TlsClientIdentity::RustlsPem { .. })
                )
            ) {
                return Err(Error::configuration(
                    "client identity is incompatible with selected TLS backend",
                ));
            }
            if tls
                .alpn_protocols
                .iter()
                .any(|value| value.is_empty() || value.len() > 255)
            {
                return Err(Error::configuration(
                    "ALPN identifiers must contain 1 to 255 bytes",
                ));
            }
            if tls.backend == TlsBackend::Native
                && tls
                    .alpn_protocols
                    .iter()
                    .any(|value| std::str::from_utf8(value).is_err())
            {
                return Err(Error::configuration(
                    "native TLS ALPN identifiers must be UTF-8",
                ));
            }
        }
        if matches!(
            self.transport,
            TransportConfig::WebSocket | TransportConfig::Wss(_)
        ) && !cfg!(feature = "websocket")
        {
            return Err(Error::configuration("WebSocket feature is disabled"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4Config {
    pub clean_session: bool,
    pub max_outgoing_packet_size: usize,
    pub inflight_limit: u16,
    pub session_store: Option<crate::SessionStoreConfig>,
}

impl Default for V4Config {
    fn default() -> Self {
        Self {
            clean_session: true,
            max_outgoing_packet_size: usize::MAX,
            inflight_limit: 100,
            session_store: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V5Config {
    pub clean_start: bool,
    pub connect_properties: V5ConnectProperties,
    pub topic_alias_policy: TopicAliasPolicy,
    pub outgoing_inflight_upper_limit: Option<u16>,
    pub session_store: Option<crate::SessionStoreConfig>,
    pub broker_session_resume_policy: crate::BrokerSessionResumePolicy,
    pub authenticator: Option<crate::AuthenticatorConfig>,
    /// Built-in SCRAM-SHA-256, mutually exclusive with a host authenticator.
    pub scram: Option<crate::ScramConfig>,
    pub redirect_policy: crate::RedirectPolicy,
    pub srv_resolver: Option<crate::SrvResolverConfig>,
}

/// Automatic outgoing alias assignment; explicit per-PUBLISH aliases remain
/// subject to negotiated-capability admission and the backend's replay rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TopicAliasPolicy {
    #[default]
    Disabled,
    Monotonic,
    Lru,
}

/// MQTT 5 CONNECT properties. `None` and `Some(0)` remain distinct.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct V5ConnectProperties {
    pub session_expiry_interval: Option<u32>,
    pub receive_maximum: Option<u16>,
    pub maximum_packet_size: Option<u32>,
    pub topic_alias_maximum: Option<u16>,
    pub request_response_information: Option<u8>,
    pub request_problem_information: Option<u8>,
    pub user_properties: Vec<(String, String)>,
    pub authentication_method: Option<String>,
    pub authentication_data: Option<Bytes>,
}

impl std::fmt::Debug for V5ConnectProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V5ConnectProperties")
            .field("session_expiry_interval", &self.session_expiry_interval)
            .field("receive_maximum", &self.receive_maximum)
            .field("maximum_packet_size", &self.maximum_packet_size)
            .field("topic_alias_maximum", &self.topic_alias_maximum)
            .field(
                "request_response_information",
                &self.request_response_information,
            )
            .field(
                "request_problem_information",
                &self.request_problem_information,
            )
            .field("user_properties", &self.user_properties)
            .field("authentication_method", &self.authentication_method)
            .field(
                "authentication_data",
                &self.authentication_data.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl V5ConnectProperties {
    fn validate(&self) -> Result<()> {
        if self.receive_maximum == Some(0) || self.maximum_packet_size == Some(0) {
            return Err(Error::configuration(
                "CONNECT receive maximum and maximum packet size must be nonzero",
            ));
        }
        for value in [
            self.request_response_information,
            self.request_problem_information,
        ]
        .into_iter()
        .flatten()
        {
            if value > 1 {
                return Err(Error::configuration(
                    "CONNECT information flags must be zero or one",
                ));
            }
        }
        crate::will::validate_user_properties(&self.user_properties)?;
        if let Some(method) = &self.authentication_method {
            validate_mqtt_utf8_string(method, "authentication method")?;
        }
        if let Some(data) = &self.authentication_data {
            if self.authentication_method.is_none() {
                return Err(Error::configuration(
                    "authentication data requires an authentication method",
                ));
            }
            crate::will::validate_binary(data, "authentication data")?;
        }
        Ok(())
    }
}

impl Default for V5Config {
    fn default() -> Self {
        Self {
            clean_start: true,
            connect_properties: V5ConnectProperties {
                maximum_packet_size: Some(10 * 1024),
                ..V5ConnectProperties::default()
            },
            topic_alias_policy: TopicAliasPolicy::Disabled,
            outgoing_inflight_upper_limit: None,
            session_store: None,
            broker_session_resume_policy: crate::BrokerSessionResumePolicy::Strict,
            authenticator: None,
            scram: None,
            redirect_policy: crate::RedirectPolicy::Reject,
            srv_resolver: None,
        }
    }
}

/// Protocol and protocol-specific session behavior selected for one client.
///
/// The selected variant remains fixed for the lifetime of the resulting [`crate::NativeClient`].
/// The wrapper does not negotiate, fall back to, or switch protocol versions. Construct another
/// client with the other variant to use a different MQTT version.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "cold-path configuration keeps existing inline protocol variants; it is not queued per operation"
)]
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
            protocol: ProtocolConfig::V4(V4Config::default()),
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
        if let Some(will) = &self.common.last_will {
            will.validate(self.protocol_version())?;
        }
        match &self.protocol {
            ProtocolConfig::V4(v4) => {
                if let Some(store) = &v4.session_store {
                    store.validate()?;
                    if v4.clean_session || self.common.client_id.is_empty() {
                        return Err(Error::configuration(
                            "durable v4 sessions require clean_session=false and a client identifier",
                        ));
                    }
                }
                if v4.inflight_limit == 0 || v4.max_outgoing_packet_size == 0 {
                    return Err(Error::configuration(
                        "v4 inflight and outgoing packet limits must be nonzero",
                    ));
                }
            }
            ProtocolConfig::V5(v5) => {
                if let crate::RedirectPolicy::Follow {
                    max_attempts,
                    transport,
                } = &v5.redirect_policy
                {
                    if *max_attempts == 0 || matches!(transport, TransportConfig::Unix) {
                        return Err(Error::configuration(
                            "redirect attempts must be nonzero and Unix redirects are unsupported",
                        ));
                    }
                    let mut target = CommonConfig::new("redirect-validation", "redirect", 1);
                    target.transport = transport.clone();
                    if matches!(
                        transport,
                        TransportConfig::WebSocket | TransportConfig::Wss(_)
                    ) {
                        target.broker = BrokerTarget::WebSocket {
                            url: if matches!(transport, TransportConfig::WebSocket) {
                                "ws://redirect:1/"
                            } else {
                                "wss://redirect:1/"
                            }
                            .into(),
                        };
                    }
                    target.validate()?;
                }
                if let Some(scram) = &v5.scram {
                    scram.validate()?;
                    if v5.authenticator.is_some()
                        || v5.connect_properties.authentication_method.as_deref()
                            != Some("SCRAM-SHA-256")
                        || v5.connect_properties.authentication_data.is_some()
                    {
                        return Err(Error::configuration(
                            "SCRAM requires method SCRAM-SHA-256 and sole ownership of authentication data",
                        ));
                    }
                }
                if let Some(auth) = &v5.authenticator {
                    if v5.connect_properties.authentication_method.is_none() {
                        return Err(Error::configuration(
                            "an authenticator requires a CONNECT authentication method",
                        ));
                    }
                    if v5.connect_properties.authentication_data.is_some() {
                        return Err(Error::configuration(
                            "initial authentication data must come from the configured authenticator",
                        ));
                    }
                    if auth.exchange_timeout.is_zero()
                        || std::time::Instant::now()
                            .checked_add(auth.exchange_timeout)
                            .is_none()
                    {
                        return Err(Error::configuration(
                            "invalid authentication exchange timeout",
                        ));
                    }
                }
                if let Some(store) = &v5.session_store {
                    store.validate()?;
                    if v5.clean_start
                        || self.common.client_id.is_empty()
                        || v5.connect_properties.session_expiry_interval.unwrap_or(0) == 0
                    {
                        return Err(Error::configuration(
                            "durable v5 sessions require clean_start=false, nonzero session expiry, and a client identifier",
                        ));
                    }
                }
                v5.connect_properties.validate()?;
                if v5.outgoing_inflight_upper_limit == Some(0) {
                    return Err(Error::configuration(
                        "outgoing inflight upper limit must be nonzero",
                    ));
                }
            }
        }
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
                clean_session: false,
                ..
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

pub fn validate_mqtt_utf8_string(value: &str, name: &str) -> Result<()> {
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
