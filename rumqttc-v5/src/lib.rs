#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(all(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
compile_error!(
    "Features `use-rustls-ring` and `use-rustls-aws-lc` are mutually exclusive. Enable only one rustls provider feature."
);

#[macro_use]
extern crate log;

use bytes::Bytes;
use std::fmt::{self, Debug, Formatter};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpStream, lookup_host};
use tokio::task::JoinSet;

#[cfg(all(feature = "url", unix))]
use percent_encoding::percent_decode_str;

#[cfg(all(feature = "url", unix))]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

#[cfg(feature = "websocket")]
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
};

mod auth;
mod client;
mod eventloop;
mod framed;
pub mod mqttbytes;
mod notice;
mod session;
mod state;
mod transport;

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
mod tls;

#[cfg(feature = "websocket")]
mod websockets;

#[cfg(feature = "proxy")]
mod proxy;

pub use client::{
    AsyncClient, AsyncClientBuilder, Client, ClientBuilder, ClientError, Connection, InvalidTopic,
    Iter, ManualAck, PublishTopic, RecvError, RecvTimeoutError, TryRecvError, ValidatedTopic,
};
pub use eventloop::{ConnectionError, Event, EventLoop};
pub use mqttbytes::v5::*;
pub use mqttbytes::*;
pub use notice::{
    AuthNotice, AuthNoticeError, NoticeFailureReason, PublishNotice, PublishNoticeError,
    PublishResult, SubscribeNotice, SubscribeNoticeError, UnsubscribeNotice,
    UnsubscribeNoticeError,
};
pub use rumqttc_core::NetworkOptions;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::TlsConfiguration;
pub use rumqttc_core::default_socket_connect;
pub use session::{
    PersistedAckMode, PersistedFilter, PersistedIncomingQos2, PersistedPubRel, PersistedPublish,
    PersistedPublishProperties, PersistedQoS, PersistedRequest, PersistedRetainForwardRule,
    PersistedSession, PersistedSubscribe, PersistedSubscribeProperties, PersistedUnsubscribe,
    PersistedUnsubscribeProperties, SessionRestoreError, SessionStore, SessionStoreError,
};
pub use state::{MqttState, MqttStateBuilder, ProtocolViolation, StateError};
pub use transport::Transport;

/// Policy used for MQTT 5 client-side topic alias assignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TopicAliasPolicy {
    /// Do not automatically assign topic aliases.
    #[default]
    Disabled,
    /// Assign aliases monotonically until the broker's Topic Alias Maximum is reached.
    Monotonic,
    /// Recycle automatically assigned aliases using least-recently-used eviction.
    Lru,
}

/// Policy for handling broker-retained sessions when this client has no local session state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrokerSessionResumePolicy {
    /// Reject broker-only session resume.
    ///
    /// This is the strict default. It follows MQTT-3.2.2-4 by rejecting
    /// `Session Present = 1` when the client cannot reconcile local in-flight
    /// `QoS` 1/2 state. Configure a [`SessionStore`] when a newly constructed
    /// `EventLoop` or restarted process needs to restore that state and
    /// strictly continue a broker-resumed session.
    #[default]
    Strict,
    /// Accept broker-only session resume even without matching local state.
    ///
    /// This is a non-strict compatibility mode for applications that prefer
    /// receiving broker-queued messages for a stable `ClientID` over strict
    /// local session reconciliation.
    AllowBrokerOnly,
}

/// Session lifetime mode for MQTT client state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    /// Start with a clean broker session on each connection.
    Clean,
    /// Request a broker-retained session that survives network reconnects.
    Persistent,
}

/// Controls how incoming publish acknowledgements are handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AckMode {
    /// Automatically send the MQTT-required response for incoming publishes.
    ///
    /// This is the fully protocol-managed path. Incoming `QoS` 0 publishes get
    /// no response, incoming `QoS` 1 publishes get `PUBACK`, and incoming
    /// `QoS` 2 publishes get `PUBREC`.
    #[default]
    Automatic,
    /// Leave incoming publish acknowledgement completion to the application.
    ///
    /// This is an advanced, application-managed mode. The client suppresses
    /// automatic `PUBACK`/`PUBREC` for incoming `QoS` 1/`QoS` 2 publishes.
    /// Applications must acknowledge every such publish with [`Client::ack`],
    /// [`AsyncClient::ack`], or `prepare_ack(...)` plus `manual_ack(...)` to
    /// remain MQTT-compliant. The library validates manual ACK packet IDs and
    /// rejects invalid or duplicate ACKs, but it cannot guarantee eventual ACK
    /// completion by the application.
    Manual,
}

const PERSISTENT_SESSION_EXPIRY_INTERVAL: u32 = u32::MAX;

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use crate::tls::Error as TlsError;

#[cfg(feature = "proxy")]
pub use crate::proxy::{Proxy, ProxyAuth, ProxyType};

#[cfg(feature = "use-native-tls")]
pub use tokio_native_tls;
#[cfg(feature = "use-rustls-no-provider")]
pub use tokio_rustls;

pub type Incoming = Packet;

/// Current outgoing activity on the eventloop
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// Publish packet with packet identifier. 0 implies `QoS` 0
    Publish(u16),
    /// Subscribe packet with packet identifier
    Subscribe(u16),
    /// Unsubscribe packet with packet identifier
    Unsubscribe(u16),
    /// `PubAck` packet
    PubAck(u16),
    /// `PubRec` packet
    PubRec(u16),
    /// `PubRel` packet
    PubRel(u16),
    /// `PubComp` packet
    PubComp(u16),
    /// Ping request packet
    PingReq,
    /// Ping response packet
    PingResp,
    /// Disconnect packet
    Disconnect,
    /// Await for an ack for more outgoing progress
    AwaitAck(u16),
    /// Auth packet
    Auth,
}

/// Custom socket connector used to establish the underlying stream before optional proxy/TLS layers.
pub(crate) type SocketConnector = rumqttc_core::SocketConnector;

const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(100);

async fn first_success_with_stagger<T, I, F, Fut>(
    items: I,
    attempt_delay: Duration,
    connect_fn: F,
) -> io::Result<T>
where
    T: Send + 'static,
    I: IntoIterator,
    I::Item: Send + 'static,
    F: Fn(I::Item) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = io::Result<T>> + Send + 'static,
{
    let mut join_set = JoinSet::new();
    let mut item_count = 0usize;

    for (index, item) in items.into_iter().enumerate() {
        item_count += 1;
        let delay = attempt_delay.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
        let connect_fn = connect_fn.clone();
        join_set.spawn(async move {
            tokio::time::sleep(delay).await;
            connect_fn(item).await
        });
    }

    if item_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        ));
    }

    let mut last_err = None;

    while let Some(task_result) = join_set.join_next().await {
        match task_result {
            Ok(Ok(stream)) => {
                join_set.abort_all();
                return Ok(stream);
            }
            Ok(Err(err)) => {
                last_err = Some(err);
            }
            Err(err) => {
                last_err = Some(io::Error::other(format!(
                    "concurrent connect task failed: {err}"
                )));
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

async fn first_success_sequential<T, I, F, Fut>(items: I, connect_fn: F) -> io::Result<T>
where
    I: IntoIterator,
    F: Fn(I::Item) -> Fut,
    Fut: std::future::Future<Output = io::Result<T>>,
{
    let mut item_count = 0usize;
    let mut last_err = None;

    for item in items {
        item_count += 1;
        match connect_fn(item).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = Some(err),
        }
    }

    if item_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        ));
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

fn should_stagger_connect_attempts(network_options: &NetworkOptions) -> bool {
    network_options
        .bind_addr()
        .is_none_or(|bind_addr| bind_addr.port() == 0)
}

async fn connect_with_retry_mode<T, I, F, Fut>(
    items: I,
    network_options: NetworkOptions,
    connect_fn: F,
) -> io::Result<T>
where
    T: Send + 'static,
    I: IntoIterator,
    I::Item: Send + 'static,
    F: Fn(I::Item, NetworkOptions) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = io::Result<T>> + Send + 'static,
{
    connect_with_retry_mode_and_delay(items, network_options, CONNECTION_ATTEMPT_DELAY, connect_fn)
        .await
}

async fn connect_with_retry_mode_and_delay<T, I, F, Fut>(
    items: I,
    network_options: NetworkOptions,
    connection_attempt_delay: Duration,
    connect_fn: F,
) -> io::Result<T>
where
    T: Send + 'static,
    I: IntoIterator,
    I::Item: Send + 'static,
    F: Fn(I::Item, NetworkOptions) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = io::Result<T>> + Send + 'static,
{
    if should_stagger_connect_attempts(&network_options) {
        first_success_with_stagger(items, connection_attempt_delay, move |item| {
            let network_options = network_options.clone();
            let connect_fn = connect_fn.clone();
            async move { connect_fn(item, network_options).await }
        })
        .await
    } else {
        first_success_sequential(items, move |item| {
            let network_options = network_options.clone();
            let connect_fn = connect_fn.clone();
            async move { connect_fn(item, network_options).await }
        })
        .await
    }
}

async fn connect_resolved_addrs_staggered(
    addrs: Vec<SocketAddr>,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    connect_with_retry_mode(
        addrs,
        network_options,
        move |addr, network_options| async move {
            rumqttc_core::connect_socket_addr(addr, network_options).await
        },
    )
    .await
}

async fn default_socket_connect_staggered(
    host: String,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let addrs = lookup_host(host).await?.collect::<Vec<_>>();
    connect_resolved_addrs_staggered(addrs, network_options).await
}

fn default_socket_connector() -> SocketConnector {
    Arc::new(|host, network_options| {
        Box::pin(async move {
            let tcp = default_socket_connect_staggered(host, network_options).await?;
            let stream: Box<dyn crate::framed::AsyncReadWrite> = Box::new(tcp);
            Ok(stream)
        })
    })
}

const DEFAULT_BROKER_PORT: u16 = 1883;

/// Broker target used to construct [`MqttOptions`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broker {
    inner: BrokerInner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BrokerInner {
    Tcp {
        host: String,
        port: u16,
    },
    #[cfg(unix)]
    Unix {
        path: PathBuf,
    },
    #[cfg(feature = "websocket")]
    Websocket {
        url: String,
    },
}

impl Broker {
    #[must_use]
    pub fn tcp<S: Into<String>>(host: S, port: u16) -> Self {
        Self {
            inner: BrokerInner::Tcp {
                host: host.into(),
                port,
            },
        }
    }

    #[cfg(unix)]
    #[must_use]
    pub fn unix<P: Into<PathBuf>>(path: P) -> Self {
        Self {
            inner: BrokerInner::Unix { path: path.into() },
        }
    }

    #[cfg(feature = "websocket")]
    /// # Errors
    ///
    /// Returns [`OptionError::WebsocketUrl`] when `url` is not a valid websocket URL or cannot
    /// be split into broker components, [`OptionError::WssRequiresExplicitTransport`] for `wss://`
    /// URLs, and [`OptionError::Scheme`] for unsupported schemes.
    pub fn websocket<S: Into<String>>(url: S) -> Result<Self, OptionError> {
        let url = url.into();
        let uri = url
            .parse::<http::Uri>()
            .map_err(|_| OptionError::WebsocketUrl)?;

        match uri.scheme_str() {
            Some("ws") => {
                rumqttc_core::split_url(&url).map_err(|_| OptionError::WebsocketUrl)?;
                Ok(Self {
                    inner: BrokerInner::Websocket { url },
                })
            }
            Some("wss") => Err(OptionError::WssRequiresExplicitTransport),
            _ => Err(OptionError::Scheme),
        }
    }

    #[must_use]
    pub const fn tcp_address(&self) -> Option<(&str, u16)> {
        match &self.inner {
            BrokerInner::Tcp { host, port } => Some((host.as_str(), *port)),
            #[cfg(unix)]
            BrokerInner::Unix { .. } => None,
            #[cfg(feature = "websocket")]
            BrokerInner::Websocket { .. } => None,
        }
    }

    #[cfg(unix)]
    #[must_use]
    pub fn unix_path(&self) -> Option<&std::path::Path> {
        match &self.inner {
            BrokerInner::Unix { path } => Some(path.as_path()),
            BrokerInner::Tcp { .. } => None,
            #[cfg(feature = "websocket")]
            BrokerInner::Websocket { .. } => None,
        }
    }

    #[cfg(feature = "websocket")]
    #[must_use]
    pub const fn websocket_url(&self) -> Option<&str> {
        match &self.inner {
            BrokerInner::Websocket { url } => Some(url.as_str()),
            BrokerInner::Tcp { .. } => None,
            #[cfg(unix)]
            BrokerInner::Unix { .. } => None,
        }
    }

    pub(crate) const fn default_transport(&self) -> Transport {
        match &self.inner {
            BrokerInner::Tcp { .. } => Transport::tcp(),
            #[cfg(unix)]
            BrokerInner::Unix { .. } => Transport::unix(),
            #[cfg(feature = "websocket")]
            BrokerInner::Websocket { .. } => Transport::Ws,
        }
    }
}

impl From<&str> for Broker {
    fn from(host: &str) -> Self {
        Self::tcp(host, DEFAULT_BROKER_PORT)
    }
}

impl From<String> for Broker {
    fn from(host: String) -> Self {
        Self::tcp(host, DEFAULT_BROKER_PORT)
    }
}

impl<S: Into<String>> From<(S, u16)> for Broker {
    fn from((host, port): (S, u16)) -> Self {
        Self::tcp(host, port)
    }
}

/// Controls how incoming packet size limits are enforced locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IncomingPacketSizeLimit {
    /// Enforce the default incoming packet size limit.
    #[default]
    Default,
    /// Disable incoming packet size checks.
    Unlimited,
    /// Enforce a user-specified maximum size.
    Bytes(u32),
}

/// Identifies which MQTT 5 enhanced-authentication exchange is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthExchangeKind {
    InitialConnect,
    Reauthentication,
}

/// Context supplied to MQTT 5 enhanced-authentication callbacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthContext<'a> {
    pub kind: AuthExchangeKind,
    pub method: &'a str,
}

/// Action returned by an [`Authenticator`] after an AUTH Continue packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthAction {
    Send(AuthProperties),
    Complete,
    Fail(String),
}

/// Errors returned by MQTT 5 enhanced-authentication callbacks.
#[non_exhaustive]
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("authentication failed: {0}")]
    Failed(String),
}

impl From<String> for AuthError {
    fn from(value: String) -> Self {
        Self::Failed(value)
    }
}

impl From<&str> for AuthError {
    fn from(value: &str) -> Self {
        Self::Failed(value.to_owned())
    }
}

/// Structured reason emitted when an authentication exchange fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthFailureReason {
    SessionReset,
    ProtocolError,
    AuthenticationFailed(String),
    ConnectionClosed,
    OverlappingReauth,
    MissingAuthenticationMethod,
    NoticeDropped,
}

/// Structured result of a completed MQTT 5 authentication exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    Success,
}

/// Structured authentication lifecycle event yielded by the event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthEvent {
    Started {
        kind: AuthExchangeKind,
        method: String,
    },
    Continue {
        kind: AuthExchangeKind,
        method: String,
    },
    Succeeded {
        kind: AuthExchangeKind,
        method: String,
    },
    Failed {
        kind: AuthExchangeKind,
        method: String,
        reason: AuthFailureReason,
    },
}

/// MQTT 5 enhanced-authentication callback interface.
pub trait Authenticator: std::fmt::Debug + Send {
    /// Called when an enhanced-authentication exchange starts.
    ///
    /// # Errors
    ///
    /// Return an error to abort the exchange locally.
    fn start(&mut self, context: AuthContext<'_>) -> Result<Option<AuthProperties>, AuthError>;

    /// Called when the broker sends AUTH Continue.
    ///
    /// # Errors
    ///
    /// Return an error to abort the exchange locally.
    fn continue_auth(
        &mut self,
        context: AuthContext<'_>,
        incoming: Option<AuthProperties>,
    ) -> Result<AuthAction, AuthError>;

    /// Called when the broker sends AUTH Success.
    ///
    /// # Errors
    ///
    /// Return an error if the successful server response cannot be accepted by
    /// the authentication mechanism.
    fn success(
        &mut self,
        context: AuthContext<'_>,
        incoming: Option<AuthProperties>,
    ) -> Result<(), AuthError>;

    /// Called when an active exchange fails or is aborted.
    fn failure(&mut self, context: AuthContext<'_>, error: AuthError);
}

/// Requests by the client to mqtt event loop. Request are
/// handled one by one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Publish(Publish),
    PubAck(PubAck),
    PubRec(PubRec),
    PubComp(PubComp),
    PubRel(PubRel),
    PingReq,
    PingResp,
    Subscribe(Subscribe),
    SubAck(SubAck),
    Unsubscribe(Unsubscribe),
    UnsubAck(UnsubAck),
    Auth(Auth),
    Disconnect(Disconnect),
    DisconnectNow(Disconnect),
    DisconnectWithTimeout(Disconnect, Duration),
}

impl From<Subscribe> for Request {
    fn from(subscribe: Subscribe) -> Self {
        Self::Subscribe(subscribe)
    }
}

#[cfg(feature = "websocket")]
type RequestModifierFn = Arc<
    dyn Fn(http::Request<()>) -> Pin<Box<dyn Future<Output = http::Request<()>> + Send>>
        + Send
        + Sync,
>;

#[cfg(feature = "websocket")]
type RequestModifierError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(feature = "websocket")]
type FallibleRequestModifierFn = Arc<
    dyn Fn(
            http::Request<()>,
        )
            -> Pin<Box<dyn Future<Output = Result<http::Request<()>, RequestModifierError>> + Send>>
        + Send
        + Sync,
>;

/// Options to configure the behaviour of MQTT connection
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct MqttOptions {
    /// broker target that you want to connect to
    broker: Broker,
    transport: Transport,
    /// keep alive time to send pingreq to broker when the connection is idle
    keep_alive: Duration,
    /// clean (or) persistent session
    clean_start: bool,
    /// Policy for broker-retained session state when local session state is missing.
    broker_session_resume_policy: BrokerSessionResumePolicy,
    /// client identifier
    client_id: String,
    /// CONNECT authentication fields
    auth: ConnectAuth,
    /// request (publish, subscribe) channel capacity
    request_channel_capacity: usize,
    /// Max internal request batching
    max_request_batch: usize,
    /// Maximum number of packets processed in a single network read batch.
    /// `0` enables adaptive batching.
    read_batch_size: usize,
    /// Minimum delay time between consecutive outgoing packets
    /// while retransmitting pending packets
    pending_throttle: Duration,
    /// Last will that will be issued on unexpected disconnect
    last_will: Option<LastWill>,
    /// Connection timeout
    connect_timeout: Duration,
    /// Default value of for maximum incoming packet size.
    /// Used when `max_incomming_size` in `connect_properties` is NOT available.
    default_max_incoming_size: u32,
    /// Local incoming packet size policy.
    incoming_packet_size_limit: IncomingPacketSizeLimit,
    /// Connect Properties
    connect_properties: Option<ConnectProperties>,
    /// Policy used when assigning outgoing MQTT 5 topic aliases.
    topic_alias_policy: TopicAliasPolicy,
    /// Controls how incoming publish acknowledgements are handled.
    ack_mode: AckMode,
    network_options: NetworkOptions,
    #[cfg(feature = "proxy")]
    /// Proxy configuration.
    proxy: Option<Proxy>,
    /// Upper limit on maximum number of inflight requests.
    /// The server may set its own maximum inflight limit, the smaller of the two will be used.
    outgoing_inflight_upper_limit: Option<u16>,
    #[cfg(feature = "websocket")]
    request_modifier: Option<RequestModifierFn>,
    #[cfg(feature = "websocket")]
    fallible_request_modifier: Option<FallibleRequestModifierFn>,
    socket_connector: Option<SocketConnector>,

    authenticator: Option<Arc<Mutex<dyn Authenticator>>>,
    session_store: Option<Arc<dyn SessionStore>>,
}

impl MqttOptions {
    /// Create an [`MqttOptions`] object that contains default values for all settings other than
    /// - id: A string to identify the device connecting to a broker
    /// - broker: The broker target to connect to
    ///
    /// ```
    /// # use rumqttc::MqttOptions;
    /// let options = MqttOptions::new("123", "localhost");
    /// ```
    pub fn new<S: Into<String>, B: Into<Broker>>(id: S, broker: B) -> Self {
        let broker = broker.into();
        Self {
            transport: broker.default_transport(),
            broker,
            keep_alive: Duration::from_secs(60),
            clean_start: true,
            broker_session_resume_policy: BrokerSessionResumePolicy::Strict,
            client_id: id.into(),
            auth: ConnectAuth::None,
            request_channel_capacity: 10,
            max_request_batch: 0,
            read_batch_size: 0,
            pending_throttle: Duration::from_micros(0),
            last_will: None,
            connect_timeout: Duration::from_secs(5),
            default_max_incoming_size: 10 * 1024,
            incoming_packet_size_limit: IncomingPacketSizeLimit::Default,
            connect_properties: None,
            topic_alias_policy: TopicAliasPolicy::Disabled,
            ack_mode: AckMode::Automatic,
            network_options: NetworkOptions::new(),
            #[cfg(feature = "proxy")]
            proxy: None,
            outgoing_inflight_upper_limit: None,
            #[cfg(feature = "websocket")]
            request_modifier: None,
            #[cfg(feature = "websocket")]
            fallible_request_modifier: None,
            socket_connector: None,
            authenticator: None,
            session_store: None,
        }
    }

    /// Create a builder for [`MqttOptions`].
    ///
    /// ```
    /// # use rumqttc::MqttOptions;
    /// let options = MqttOptions::builder("123", "localhost")
    ///     .keep_alive(5)
    ///     .clean_start(true)
    ///     .build();
    /// ```
    #[must_use]
    pub fn builder<S: Into<String>, B: Into<Broker>>(id: S, broker: B) -> MqttOptionsBuilder {
        MqttOptionsBuilder::new(id, broker)
    }

    #[cfg(feature = "url")]
    /// Creates an [`MqttOptions`] object by parsing provided string with the [url] crate's
    /// [`Url::parse(url)`](url::Url::parse) method and is only enabled when run using the "url" feature.
    ///
    /// ```
    /// # use rumqttc::MqttOptions;
    /// let options = MqttOptions::parse_url("mqtt://example.com:1883?client_id=123").unwrap();
    /// ```
    ///
    /// **NOTE:** A url must be prefixed with one of either `tcp://`, `mqtt://` or `ws://` to
    /// denote the protocol for establishing a connection with the broker. On Unix platforms,
    /// `unix:///path/to/socket` is also supported.
    ///
    /// **NOTE:** Secure transports are configured explicitly with
    /// [`set_transport`](MqttOptions::set_transport). Secure URL schemes such as `mqtts://`,
    /// `ssl://`, and `wss://` are rejected.
    ///
    /// ```ignore
    /// # use rumqttc::{MqttOptions, Transport};
    /// # use tokio_rustls::rustls::ClientConfig;
    /// # let root_cert_store = rustls::RootCertStore::empty();
    /// # let client_config = ClientConfig::builder()
    /// #    .with_root_certificates(root_cert_store)
    /// #    .with_no_client_auth();
    /// let mut options = MqttOptions::parse_url("mqtt://example.com?client_id=123").unwrap();
    /// options.set_transport(Transport::tls_with_config(client_config.into()));
    /// ```
    ///
    /// On Unix platforms, `unix:///tmp/mqtt.sock?client_id=123` is also supported.
    ///
    /// # Errors
    ///
    /// Returns any [`OptionError`] produced while parsing the URL, validating its scheme,
    /// interpreting query parameters, or constructing the broker options from it.
    pub fn parse_url<S: Into<String>>(url: S) -> Result<Self, OptionError> {
        let url = url::Url::parse(&url.into())?;
        let options = Self::try_from(url)?;

        Ok(options)
    }

    /// Broker target
    pub const fn broker(&self) -> &Broker {
        &self.broker
    }

    pub fn set_last_will(&mut self, will: LastWill) -> &mut Self {
        self.last_will = Some(will);
        self
    }

    pub fn last_will(&self) -> Option<LastWill> {
        self.last_will.clone()
    }

    /// Sets an infallible handler for modifying the websocket HTTP request before it is sent.
    ///
    /// Calling this method replaces any previously configured fallible request modifier.
    #[cfg(feature = "websocket")]
    pub fn set_request_modifier<F, O>(&mut self, request_modifier: F) -> &mut Self
    where
        F: Fn(http::Request<()>) -> O + Send + Sync + 'static,
        O: IntoFuture<Output = http::Request<()>> + 'static,
        O::IntoFuture: Send,
    {
        self.request_modifier = Some(Arc::new(move |request| {
            let request_modifier = request_modifier(request).into_future();
            Box::pin(request_modifier)
        }));
        self.fallible_request_modifier = None;
        self
    }

    /// Sets a fallible handler for modifying the websocket HTTP request before it is sent.
    ///
    /// Calling this method replaces any previously configured infallible request modifier.
    /// If the modifier returns an error, the connection fails with
    /// [`ConnectionError::RequestModifier`].
    #[cfg(feature = "websocket")]
    pub fn set_fallible_request_modifier<F, O, E>(&mut self, request_modifier: F) -> &mut Self
    where
        F: Fn(http::Request<()>) -> O + Send + Sync + 'static,
        O: IntoFuture<Output = Result<http::Request<()>, E>> + 'static,
        O::IntoFuture: Send,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.fallible_request_modifier = Some(Arc::new(move |request| {
            let request_modifier = request_modifier(request).into_future();
            Box::pin(async move {
                request_modifier
                    .await
                    .map_err(|error| Box::new(error) as RequestModifierError)
            })
        }));
        self.request_modifier = None;
        self
    }

    #[cfg(feature = "websocket")]
    pub fn request_modifier(&self) -> Option<RequestModifierFn> {
        self.request_modifier.clone()
    }

    #[cfg(feature = "websocket")]
    pub(crate) fn fallible_request_modifier(&self) -> Option<FallibleRequestModifierFn> {
        self.fallible_request_modifier.clone()
    }

    /// Sets a custom socket connector, overriding the default TCP socket creation logic.
    ///
    /// The connector is used to create the base stream before optional proxy/TLS/WebSocket layers
    /// managed by `MqttOptions` are applied.
    ///
    /// If the connector already performs TLS/proxy work, configure `MqttOptions` transport/proxy
    /// to avoid layering those concerns twice.
    ///
    /// Custom connectors are also responsible for honoring `network_options` themselves. To keep
    /// `NetworkOptions` behavior such as `set_bind_addr(...)`, forward `network_options` into
    /// `rumqttc::default_socket_connect(...)` or apply the equivalent socket configuration before
    /// connecting.
    ///
    /// # Example
    /// ```
    /// # use rumqttc::MqttOptions;
    /// # let mut options = MqttOptions::new("test", "localhost");
    /// options.set_socket_connector(|host, network_options| async move {
    ///     rumqttc::default_socket_connect(host, network_options).await
    /// });
    /// ```
    #[cfg(not(feature = "websocket"))]
    pub fn set_socket_connector<F, Fut, S>(&mut self, f: F) -> &mut Self
    where
        F: Fn(String, NetworkOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<S, std::io::Error>> + Send + 'static,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        self.socket_connector = Some(Arc::new(move |host, network_options| {
            let stream_future = f(host, network_options);
            let future = async move {
                let stream = stream_future.await?;
                Ok(Box::new(stream) as Box<dyn crate::framed::AsyncReadWrite>)
            };
            Box::pin(future)
        }));
        self
    }

    /// Sets a custom socket connector, overriding the default TCP socket creation logic.
    ///
    /// The connector is used to create the base stream before optional proxy/TLS/WebSocket layers
    /// managed by `MqttOptions` are applied.
    ///
    /// If the connector already performs TLS/proxy work, configure `MqttOptions` transport/proxy
    /// to avoid layering those concerns twice.
    ///
    /// Custom connectors are also responsible for honoring `network_options` themselves. To keep
    /// `NetworkOptions` behavior such as `set_bind_addr(...)`, forward `network_options` into
    /// `rumqttc::default_socket_connect(...)` or apply the equivalent socket configuration before
    /// connecting.
    ///
    /// # Example
    /// ```
    /// # use rumqttc::MqttOptions;
    /// # let mut options = MqttOptions::new("test", "localhost");
    /// options.set_socket_connector(|host, network_options| async move {
    ///     rumqttc::default_socket_connect(host, network_options).await
    /// });
    /// ```
    #[cfg(feature = "websocket")]
    pub fn set_socket_connector<F, Fut, S>(&mut self, f: F) -> &mut Self
    where
        F: Fn(String, NetworkOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<S, std::io::Error>> + Send + 'static,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        self.socket_connector = Some(Arc::new(move |host, network_options| {
            let stream_future = f(host, network_options);
            let future = async move {
                let stream = stream_future.await?;
                let stream: Box<dyn crate::framed::AsyncReadWrite> = Box::new(stream);
                Ok(stream)
            };
            Box::pin(future)
        }));
        self
    }

    /// Returns whether a custom socket connector has been set.
    pub fn has_socket_connector(&self) -> bool {
        self.socket_connector.is_some()
    }

    pub fn set_client_id(&mut self, client_id: String) -> &mut Self {
        self.client_id = client_id;
        self
    }

    #[cfg(not(any(feature = "use-rustls-no-provider", feature = "use-native-tls")))]
    pub const fn set_transport(&mut self, transport: Transport) -> &mut Self {
        self.transport = transport;
        self
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    pub fn set_transport(&mut self, transport: Transport) -> &mut Self {
        self.transport = transport;
        self
    }

    /// Returns the configured transport.
    pub fn transport(&self) -> Transport {
        self.transport.clone()
    }

    /// Set number of seconds after which client should ping the broker
    /// if there is no other data exchange
    /// Set to `0` to disable automatic keep-alive pings.
    pub fn set_keep_alive(&mut self, seconds: u16) -> &mut Self {
        self.keep_alive = Duration::from_secs(u64::from(seconds));
        self
    }

    /// Keep alive time
    pub const fn keep_alive(&self) -> Duration {
        self.keep_alive
    }

    /// Client identifier
    pub fn client_id(&self) -> String {
        self.client_id.clone()
    }

    /// `clean_start = true` removes previously retained session state and starts a fresh session.
    ///
    /// When set to `false`, the broker can resume an existing session for the same `client_id`.
    /// To request that the broker retains this connection's session after disconnect, configure
    /// a non-zero session expiry interval or use [`SessionMode::Persistent`].
    pub const fn set_clean_start(&mut self, clean_start: bool) -> &mut Self {
        self.clean_start = clean_start;
        self
    }

    /// Set whether this client uses a clean or persistent session.
    pub fn set_session_mode(&mut self, mode: SessionMode) -> &mut Self {
        match mode {
            SessionMode::Clean => {
                self.clean_start = true;
                if let Some(properties) = &mut self.connect_properties {
                    properties.session_expiry_interval = None;
                }
            }
            SessionMode::Persistent => {
                self.clean_start = false;
                if let Some(properties) = &mut self.connect_properties {
                    if matches!(properties.session_expiry_interval, None | Some(0)) {
                        properties.session_expiry_interval =
                            Some(PERSISTENT_SESSION_EXPIRY_INTERVAL);
                    }
                } else {
                    let mut properties = ConnectProperties::new();
                    properties.session_expiry_interval = Some(PERSISTENT_SESSION_EXPIRY_INTERVAL);
                    self.connect_properties = Some(properties);
                }
            }
        }

        self
    }

    /// Clean session
    pub const fn clean_start(&self) -> bool {
        self.clean_start
    }

    /// Set how broker-retained session state is handled when local session state is missing.
    pub const fn set_broker_session_resume_policy(
        &mut self,
        policy: BrokerSessionResumePolicy,
    ) -> &mut Self {
        self.broker_session_resume_policy = policy;
        self
    }

    /// Returns how broker-retained session state is handled when local session state is missing.
    pub const fn broker_session_resume_policy(&self) -> BrokerSessionResumePolicy {
        self.broker_session_resume_policy
    }

    /// Set durable storage for MQTT 5 persistent client session state.
    ///
    /// Persistence is opt-in and used only when `clean_start` is `false`.
    /// rumqttc supplies the [`SessionStore`] trait and [`PersistedSession`]
    /// checkpoint model, but applications provide the actual storage backend
    /// and serialization format.
    ///
    /// Configure a store if the client needs strict restart-safe resume for
    /// sessions with a non-zero Session Expiry Interval. Without restored local
    /// Session State, strict mode rejects a broker response with
    /// `Session Present = 1` rather than continuing a session it cannot
    /// reconcile.
    ///
    /// The event loop saves checkpoints after local session state changes,
    /// loads a checkpoint before reconnecting a newly constructed event loop,
    /// and clears the store when the broker starts a fresh session or the
    /// effective session expiry is zero.
    pub fn set_session_store<S>(&mut self, store: S) -> &mut Self
    where
        S: SessionStore,
    {
        self.session_store = Some(Arc::new(store));
        self
    }

    /// Set durable storage from an existing shared store handle.
    ///
    /// See [`MqttOptions::set_session_store`] for persistence semantics.
    pub fn set_session_store_arc(&mut self, store: Arc<dyn SessionStore>) -> &mut Self {
        self.session_store = Some(store);
        self
    }

    /// Remove durable session storage from these options.
    pub fn clear_session_store(&mut self) -> &mut Self {
        self.session_store = None;
        self
    }

    /// Returns the configured durable session store, if any.
    pub fn session_store(&self) -> Option<Arc<dyn SessionStore>> {
        self.session_store.clone()
    }

    /// Replace the current CONNECT authentication state.
    ///
    /// ```
    /// use bytes::Bytes;
    /// use rumqttc::{ConnectAuth, MqttOptions};
    ///
    /// let mut options = MqttOptions::new("client", "localhost");
    /// options.set_auth(ConnectAuth::UsernamePassword {
    ///     username: "user".into(),
    ///     password: Bytes::from_static(b"pw"),
    /// });
    /// ```
    pub fn set_auth(&mut self, auth: ConnectAuth) -> &mut Self {
        self.auth = auth;
        self
    }

    /// Clear CONNECT authentication fields.
    pub fn clear_auth(&mut self) -> &mut Self {
        self.auth = ConnectAuth::None;
        self
    }

    /// Set only the MQTT username field.
    ///
    /// ```
    /// use rumqttc::{ConnectAuth, MqttOptions};
    ///
    /// let mut options = MqttOptions::new("client", "localhost");
    /// options.set_username("user");
    ///
    /// assert_eq!(
    ///     options.auth(),
    ///     &ConnectAuth::Username {
    ///         username: "user".into(),
    ///     }
    /// );
    /// ```
    pub fn set_username<U: Into<String>>(&mut self, username: U) -> &mut Self {
        self.auth = ConnectAuth::Username {
            username: username.into(),
        };
        self
    }

    /// Set only the MQTT password field.
    ///
    /// ```
    /// use bytes::Bytes;
    /// use rumqttc::{ConnectAuth, MqttOptions};
    ///
    /// let mut options = MqttOptions::new("client", "localhost");
    /// options.set_password(Bytes::from_static(b"\x00\xfftoken"));
    ///
    /// assert_eq!(
    ///     options.auth(),
    ///     &ConnectAuth::Password {
    ///         password: Bytes::from_static(b"\x00\xfftoken"),
    ///     }
    /// );
    /// ```
    pub fn set_password<P: Into<Bytes>>(&mut self, password: P) -> &mut Self {
        self.auth = ConnectAuth::Password {
            password: password.into(),
        };
        self
    }

    /// Set both MQTT username and binary password fields.
    ///
    /// ```
    /// use bytes::Bytes;
    /// use rumqttc::{ConnectAuth, MqttOptions};
    ///
    /// let mut options = MqttOptions::new("client", "localhost");
    /// options.set_credentials("user", Bytes::from_static(b"\x00\xfftoken"));
    ///
    /// assert_eq!(
    ///     options.auth(),
    ///     &ConnectAuth::UsernamePassword {
    ///         username: "user".into(),
    ///         password: Bytes::from_static(b"\x00\xfftoken"),
    ///     }
    /// );
    /// ```
    pub fn set_credentials<U: Into<String>, P: Into<Bytes>>(
        &mut self,
        username: U,
        password: P,
    ) -> &mut Self {
        self.auth = ConnectAuth::UsernamePassword {
            username: username.into(),
            password: password.into(),
        };
        self
    }

    /// CONNECT authentication fields.
    ///
    /// ```
    /// use rumqttc::{ConnectAuth, MqttOptions};
    ///
    /// let mut options = MqttOptions::new("client", "localhost");
    /// options.set_password("pw");
    ///
    /// match options.auth() {
    ///     ConnectAuth::Password { password } => assert_eq!(password.as_ref(), b"pw"),
    ///     auth => panic!("unexpected auth state: {auth:?}"),
    /// }
    /// ```
    pub const fn auth(&self) -> &ConnectAuth {
        &self.auth
    }

    /// Set request channel capacity
    pub const fn set_request_channel_capacity(&mut self, capacity: usize) -> &mut Self {
        self.request_channel_capacity = capacity;
        self
    }

    /// Request channel capacity
    pub const fn request_channel_capacity(&self) -> usize {
        self.request_channel_capacity
    }

    /// Set maximum number of requests processed in one eventloop iteration.
    ///
    /// `0` preserves legacy behavior (effectively processes one request).
    pub const fn set_max_request_batch(&mut self, max: usize) -> &mut Self {
        self.max_request_batch = max;
        self
    }

    /// Maximum number of requests processed in one eventloop iteration.
    pub const fn max_request_batch(&self) -> usize {
        self.max_request_batch
    }

    /// Set maximum number of packets processed in one network read batch.
    ///
    /// `0` enables adaptive batching.
    pub const fn set_read_batch_size(&mut self, size: usize) -> &mut Self {
        self.read_batch_size = size;
        self
    }

    /// Maximum number of packets processed in one network read batch.
    ///
    /// `0` means adaptive batching.
    pub const fn read_batch_size(&self) -> usize {
        self.read_batch_size
    }

    /// Enables throttling and sets outoing message rate to the specified 'rate'
    pub const fn set_pending_throttle(&mut self, duration: Duration) -> &mut Self {
        self.pending_throttle = duration;
        self
    }

    /// Outgoing message rate
    pub const fn pending_throttle(&self) -> Duration {
        self.pending_throttle
    }

    /// set connect timeout
    pub const fn set_connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connect_timeout = timeout;
        self
    }

    /// get connect timeout
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// set connection properties
    pub fn set_connect_properties(&mut self, properties: ConnectProperties) -> &mut Self {
        self.incoming_packet_size_limit = properties.max_packet_size.map_or(
            IncomingPacketSizeLimit::Default,
            IncomingPacketSizeLimit::Bytes,
        );
        self.connect_properties = Some(properties);
        self
    }

    /// get connection properties
    pub fn connect_properties(&self) -> Option<ConnectProperties> {
        self.connect_properties.clone()
    }

    /// set session expiry interval on connection properties
    pub fn set_session_expiry_interval(&mut self, interval: Option<u32>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.session_expiry_interval = interval;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.session_expiry_interval = interval;
            self.set_connect_properties(conn_props)
        }
    }

    /// get session expiry interval on connection properties
    pub const fn session_expiry_interval(&self) -> Option<u32> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.session_expiry_interval
        } else {
            None
        }
    }

    /// set receive maximum on connection properties
    pub fn set_receive_maximum(&mut self, recv_max: Option<u16>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.receive_maximum = recv_max;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.receive_maximum = recv_max;
            self.set_connect_properties(conn_props)
        }
    }

    /// get receive maximum from connection properties
    pub const fn receive_maximum(&self) -> Option<u16> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.receive_maximum
        } else {
            None
        }
    }

    /// set max packet size on connection properties
    pub fn set_max_packet_size(&mut self, max_size: Option<u32>) -> &mut Self {
        self.incoming_packet_size_limit = max_size.map_or(
            IncomingPacketSizeLimit::Default,
            IncomingPacketSizeLimit::Bytes,
        );

        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.max_packet_size = max_size;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.max_packet_size = max_size;
            self.set_connect_properties(conn_props)
        }
    }

    /// get max packet size from connection properties
    pub const fn max_packet_size(&self) -> Option<u32> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.max_packet_size
        } else {
            None
        }
    }

    /// set local incoming packet size policy
    ///
    /// This controls packet size enforcement in the decoder:
    /// - [`IncomingPacketSizeLimit::Default`] uses `default_max_incoming_size`
    /// - [`IncomingPacketSizeLimit::Unlimited`] disables incoming size checks
    /// - [`IncomingPacketSizeLimit::Bytes`] enforces an explicit limit
    pub fn set_incoming_packet_size_limit(&mut self, limit: IncomingPacketSizeLimit) -> &mut Self {
        self.incoming_packet_size_limit = limit;

        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.max_packet_size = match limit {
                IncomingPacketSizeLimit::Bytes(max_size) => Some(max_size),
                IncomingPacketSizeLimit::Default | IncomingPacketSizeLimit::Unlimited => None,
            };
            return self;
        }

        if let IncomingPacketSizeLimit::Bytes(max_size) = limit {
            let mut conn_props = ConnectProperties::new();
            conn_props.max_packet_size = Some(max_size);
            self.set_connect_properties(conn_props)
        } else {
            self
        }
    }

    /// disable local incoming packet size checks
    pub fn set_unlimited_incoming_packet_size(&mut self) -> &mut Self {
        self.set_incoming_packet_size_limit(IncomingPacketSizeLimit::Unlimited)
    }

    /// get local incoming packet size policy
    pub const fn incoming_packet_size_limit(&self) -> IncomingPacketSizeLimit {
        self.incoming_packet_size_limit
    }

    pub(crate) const fn max_incoming_packet_size(&self) -> Option<u32> {
        match self.incoming_packet_size_limit {
            IncomingPacketSizeLimit::Default => Some(self.default_max_incoming_size),
            IncomingPacketSizeLimit::Unlimited => None,
            IncomingPacketSizeLimit::Bytes(max_size) => Some(max_size),
        }
    }

    /// set max topic alias on connection properties
    pub fn set_topic_alias_max(&mut self, topic_alias_max: Option<u16>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.topic_alias_max = topic_alias_max;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.topic_alias_max = topic_alias_max;
            self.set_connect_properties(conn_props)
        }
    }

    /// get max topic alias from connection properties
    pub const fn topic_alias_max(&self) -> Option<u16> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.topic_alias_max
        } else {
            None
        }
    }

    /// Set the policy used for outgoing topic alias assignment.
    ///
    /// [`TopicAliasPolicy::Disabled`] turns automatic assignment off. The
    /// automatic policies assign topic aliases immediately before sending
    /// publishes on connections where the broker advertises a non-zero Topic
    /// Alias Maximum. Publishes that already contain a topic alias are left
    /// unchanged.
    pub const fn set_topic_alias_policy(
        &mut self,
        topic_alias_policy: TopicAliasPolicy,
    ) -> &mut Self {
        self.topic_alias_policy = topic_alias_policy;
        self
    }

    /// Returns the policy used for outgoing topic alias assignment.
    pub const fn topic_alias_policy(&self) -> TopicAliasPolicy {
        self.topic_alias_policy
    }

    /// set request response info on connection properties
    pub fn set_request_response_info(&mut self, request_response_info: Option<u8>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.request_response_info = request_response_info;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.request_response_info = request_response_info;
            self.set_connect_properties(conn_props)
        }
    }

    /// get request response info from connection properties
    pub const fn request_response_info(&self) -> Option<u8> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.request_response_info
        } else {
            None
        }
    }

    /// set request problem info on connection properties
    pub fn set_request_problem_info(&mut self, request_problem_info: Option<u8>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.request_problem_info = request_problem_info;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.request_problem_info = request_problem_info;
            self.set_connect_properties(conn_props)
        }
    }

    /// get request problem info from connection properties
    pub const fn request_problem_info(&self) -> Option<u8> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.request_problem_info
        } else {
            None
        }
    }

    /// set user properties on connection properties
    pub fn set_user_properties(&mut self, user_properties: Vec<(String, String)>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.user_properties = user_properties;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.user_properties = user_properties;
            self.set_connect_properties(conn_props)
        }
    }

    /// get user properties from connection properties
    pub fn user_properties(&self) -> Vec<(String, String)> {
        self.connect_properties
            .as_ref()
            .map_or_else(Vec::new, |conn_props| conn_props.user_properties.clone())
    }

    /// set authentication method on connection properties
    pub fn set_authentication_method(
        &mut self,
        authentication_method: Option<String>,
    ) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.authentication_method = authentication_method;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.authentication_method = authentication_method;
            self.set_connect_properties(conn_props)
        }
    }

    /// get authentication method from connection properties
    pub fn authentication_method(&self) -> Option<String> {
        self.connect_properties
            .as_ref()
            .and_then(|conn_props| conn_props.authentication_method.clone())
    }

    /// set authentication data on connection properties
    pub fn set_authentication_data(&mut self, authentication_data: Option<Bytes>) -> &mut Self {
        if let Some(conn_props) = &mut self.connect_properties {
            conn_props.authentication_data = authentication_data;
            self
        } else {
            let mut conn_props = ConnectProperties::new();
            conn_props.authentication_data = authentication_data;
            self.set_connect_properties(conn_props)
        }
    }

    /// get authentication data from connection properties
    pub fn authentication_data(&self) -> Option<Bytes> {
        self.connect_properties
            .as_ref()
            .map_or_else(|| None, |conn_props| conn_props.authentication_data.clone())
    }

    /// Set how incoming publish acknowledgements are handled.
    pub const fn set_ack_mode(&mut self, ack_mode: AckMode) -> &mut Self {
        self.ack_mode = ack_mode;
        self
    }

    /// Returns how incoming publish acknowledgements are handled.
    pub const fn ack_mode(&self) -> AckMode {
        self.ack_mode
    }

    pub fn network_options(&self) -> NetworkOptions {
        self.network_options.clone()
    }

    pub fn set_network_options(&mut self, network_options: NetworkOptions) -> &mut Self {
        self.network_options = network_options;
        self
    }

    #[cfg(feature = "proxy")]
    pub fn set_proxy(&mut self, proxy: Proxy) -> &mut Self {
        self.proxy = Some(proxy);
        self
    }

    #[cfg(feature = "proxy")]
    pub fn proxy(&self) -> Option<Proxy> {
        self.proxy.clone()
    }

    pub(crate) fn effective_socket_connector(&self) -> SocketConnector {
        self.socket_connector
            .clone()
            .unwrap_or_else(default_socket_connector)
    }

    pub(crate) async fn socket_connect(
        &self,
        host: String,
        network_options: NetworkOptions,
    ) -> std::io::Result<Box<dyn crate::framed::AsyncReadWrite>> {
        let connector = self.effective_socket_connector();
        connector(host, network_options).await
    }

    /// Get the upper limit on maximum number of inflight outgoing publishes.
    /// The server may set its own maximum inflight limit, the smaller of the two will be used.
    pub const fn set_outgoing_inflight_upper_limit(&mut self, limit: u16) -> &mut Self {
        self.outgoing_inflight_upper_limit = Some(limit);
        self
    }

    /// Set the upper limit on maximum number of inflight outgoing publishes.
    /// The server may set its own maximum inflight limit, the smaller of the two will be used.
    pub const fn get_outgoing_inflight_upper_limit(&self) -> Option<u16> {
        self.outgoing_inflight_upper_limit
    }

    pub fn set_authenticator(&mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> &mut Self {
        self.authenticator = Some(authenticator);
        self
    }

    pub fn authenticator(&self) -> Option<Arc<Mutex<dyn Authenticator>>> {
        self.authenticator.as_ref()?;

        self.authenticator.clone()
    }

    pub fn set_auth_manager(&mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> &mut Self {
        self.set_authenticator(authenticator)
    }

    pub fn auth_manager(&self) -> Option<Arc<Mutex<dyn Authenticator>>> {
        self.authenticator()
    }
}

/// Builder for [`MqttOptions`].
pub struct MqttOptionsBuilder {
    options: MqttOptions,
}

impl MqttOptionsBuilder {
    /// Create a new [`MqttOptions`] builder.
    #[must_use]
    pub fn new<S: Into<String>, B: Into<Broker>>(id: S, broker: B) -> Self {
        Self {
            options: MqttOptions::new(id, broker),
        }
    }

    /// Build the configured [`MqttOptions`].
    #[must_use]
    pub fn build(self) -> MqttOptions {
        self.options
    }

    /// Set the last will.
    #[must_use]
    pub fn last_will(mut self, will: LastWill) -> Self {
        self.options.set_last_will(will);
        self
    }

    /// Set an infallible handler for modifying the websocket HTTP request before it is sent.
    #[cfg(feature = "websocket")]
    #[must_use]
    pub fn request_modifier<F, O>(mut self, request_modifier: F) -> Self
    where
        F: Fn(http::Request<()>) -> O + Send + Sync + 'static,
        O: IntoFuture<Output = http::Request<()>> + 'static,
        O::IntoFuture: Send,
    {
        self.options.set_request_modifier(request_modifier);
        self
    }

    /// Set a fallible handler for modifying the websocket HTTP request before it is sent.
    #[cfg(feature = "websocket")]
    #[must_use]
    pub fn fallible_request_modifier<F, O, E>(mut self, request_modifier: F) -> Self
    where
        F: Fn(http::Request<()>) -> O + Send + Sync + 'static,
        O: IntoFuture<Output = Result<http::Request<()>, E>> + 'static,
        O::IntoFuture: Send,
        E: std::error::Error + Send + Sync + 'static,
    {
        self.options.set_fallible_request_modifier(request_modifier);
        self
    }

    /// Set a custom socket connector.
    #[cfg(not(feature = "websocket"))]
    #[must_use]
    pub fn socket_connector<F, Fut, S>(mut self, f: F) -> Self
    where
        F: Fn(String, NetworkOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<S, std::io::Error>> + Send + 'static,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin + 'static,
    {
        self.options.set_socket_connector(f);
        self
    }

    /// Set a custom socket connector.
    #[cfg(feature = "websocket")]
    #[must_use]
    pub fn socket_connector<F, Fut, S>(mut self, f: F) -> Self
    where
        F: Fn(String, NetworkOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<S, std::io::Error>> + Send + 'static,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        self.options.set_socket_connector(f);
        self
    }

    /// Set the client identifier.
    #[must_use]
    pub fn client_id(mut self, client_id: String) -> Self {
        self.options.set_client_id(client_id);
        self
    }

    /// Set the transport.
    #[cfg(not(any(feature = "use-rustls-no-provider", feature = "use-native-tls")))]
    #[must_use]
    pub const fn transport(mut self, transport: Transport) -> Self {
        self.options.set_transport(transport);
        self
    }

    /// Set the transport.
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[must_use]
    pub fn transport(mut self, transport: Transport) -> Self {
        self.options.set_transport(transport);
        self
    }

    /// Set number of seconds after which client should ping the broker if there is no other data exchange.
    #[must_use]
    pub fn keep_alive(mut self, seconds: u16) -> Self {
        self.options.set_keep_alive(seconds);
        self
    }

    /// Set whether the broker should start a clean session.
    #[must_use]
    pub const fn clean_start(mut self, clean_start: bool) -> Self {
        self.options.set_clean_start(clean_start);
        self
    }

    /// Set whether this client uses a clean or persistent session.
    #[must_use]
    pub fn session_mode(mut self, mode: SessionMode) -> Self {
        self.options.set_session_mode(mode);
        self
    }

    /// Set how broker-retained session state is handled when local session state is missing.
    #[must_use]
    pub const fn broker_session_resume_policy(mut self, policy: BrokerSessionResumePolicy) -> Self {
        self.options.set_broker_session_resume_policy(policy);
        self
    }

    /// Set durable storage for MQTT 5 persistent client session state.
    ///
    /// See [`MqttOptions::set_session_store`] for persistence semantics.
    #[must_use]
    pub fn session_store<S>(mut self, store: S) -> Self
    where
        S: SessionStore,
    {
        self.options.set_session_store(store);
        self
    }

    /// Set durable storage from an existing shared store handle.
    ///
    /// See [`MqttOptions::set_session_store`] for persistence semantics.
    #[must_use]
    pub fn session_store_arc(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.options.set_session_store_arc(store);
        self
    }

    /// Replace the current CONNECT authentication state.
    #[must_use]
    pub fn auth(mut self, auth: ConnectAuth) -> Self {
        self.options.set_auth(auth);
        self
    }

    /// Clear CONNECT authentication fields.
    #[must_use]
    pub fn clear_auth(mut self) -> Self {
        self.options.clear_auth();
        self
    }

    /// Set only the MQTT username field.
    #[must_use]
    pub fn username<U: Into<String>>(mut self, username: U) -> Self {
        self.options.set_username(username);
        self
    }

    /// Set only the MQTT password field.
    #[must_use]
    pub fn password<P: Into<Bytes>>(mut self, password: P) -> Self {
        self.options.set_password(password);
        self
    }

    /// Set both MQTT username and binary password fields.
    #[must_use]
    pub fn credentials<U: Into<String>, P: Into<Bytes>>(
        mut self,
        username: U,
        password: P,
    ) -> Self {
        self.options.set_credentials(username, password);
        self
    }

    /// Set request channel capacity.
    #[must_use]
    pub const fn request_channel_capacity(mut self, capacity: usize) -> Self {
        self.options.set_request_channel_capacity(capacity);
        self
    }

    /// Set maximum number of requests processed in one eventloop iteration.
    #[must_use]
    pub const fn max_request_batch(mut self, max: usize) -> Self {
        self.options.set_max_request_batch(max);
        self
    }

    /// Set maximum number of packets processed in one network read batch.
    #[must_use]
    pub const fn read_batch_size(mut self, size: usize) -> Self {
        self.options.set_read_batch_size(size);
        self
    }

    /// Set the minimum delay between retransmitted outgoing packets.
    #[must_use]
    pub const fn pending_throttle(mut self, duration: Duration) -> Self {
        self.options.set_pending_throttle(duration);
        self
    }

    /// Set connect timeout.
    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.options.set_connect_timeout(timeout);
        self
    }

    /// Set connection properties.
    #[must_use]
    pub fn connect_properties(mut self, properties: ConnectProperties) -> Self {
        self.options.set_connect_properties(properties);
        self
    }

    /// Set session expiry interval on connection properties.
    #[must_use]
    pub fn session_expiry_interval(mut self, interval: Option<u32>) -> Self {
        self.options.set_session_expiry_interval(interval);
        self
    }

    /// Set receive maximum on connection properties.
    #[must_use]
    pub fn receive_maximum(mut self, recv_max: Option<u16>) -> Self {
        self.options.set_receive_maximum(recv_max);
        self
    }

    /// Set max packet size on connection properties.
    #[must_use]
    pub fn max_packet_size(mut self, max_size: Option<u32>) -> Self {
        self.options.set_max_packet_size(max_size);
        self
    }

    /// Set local incoming packet size policy.
    #[must_use]
    pub fn incoming_packet_size_limit(mut self, limit: IncomingPacketSizeLimit) -> Self {
        self.options.set_incoming_packet_size_limit(limit);
        self
    }

    /// Disable local incoming packet size checks.
    #[must_use]
    pub fn unlimited_incoming_packet_size(mut self) -> Self {
        self.options.set_unlimited_incoming_packet_size();
        self
    }

    /// Set max topic alias on connection properties.
    #[must_use]
    pub fn topic_alias_max(mut self, topic_alias_max: Option<u16>) -> Self {
        self.options.set_topic_alias_max(topic_alias_max);
        self
    }

    /// Set the policy used for outgoing topic alias assignment.
    #[must_use]
    pub const fn topic_alias_policy(mut self, topic_alias_policy: TopicAliasPolicy) -> Self {
        self.options.set_topic_alias_policy(topic_alias_policy);
        self
    }

    /// Set request response info on connection properties.
    #[must_use]
    pub fn request_response_info(mut self, request_response_info: Option<u8>) -> Self {
        self.options
            .set_request_response_info(request_response_info);
        self
    }

    /// Set request problem info on connection properties.
    #[must_use]
    pub fn request_problem_info(mut self, request_problem_info: Option<u8>) -> Self {
        self.options.set_request_problem_info(request_problem_info);
        self
    }

    /// Set user properties on connection properties.
    #[must_use]
    pub fn user_properties(mut self, user_properties: Vec<(String, String)>) -> Self {
        self.options.set_user_properties(user_properties);
        self
    }

    /// Set authentication method on connection properties.
    #[must_use]
    pub fn authentication_method(mut self, authentication_method: Option<String>) -> Self {
        self.options
            .set_authentication_method(authentication_method);
        self
    }

    /// Set authentication data on connection properties.
    #[must_use]
    pub fn authentication_data(mut self, authentication_data: Option<Bytes>) -> Self {
        self.options.set_authentication_data(authentication_data);
        self
    }

    /// Set how incoming publish acknowledgements are handled.
    #[must_use]
    pub const fn ack_mode(mut self, ack_mode: AckMode) -> Self {
        self.options.set_ack_mode(ack_mode);
        self
    }

    /// Set network options.
    #[must_use]
    pub fn network_options(mut self, network_options: NetworkOptions) -> Self {
        self.options.set_network_options(network_options);
        self
    }

    /// Set proxy configuration.
    #[cfg(feature = "proxy")]
    #[must_use]
    pub fn proxy(mut self, proxy: Proxy) -> Self {
        self.options.set_proxy(proxy);
        self
    }

    /// Set the upper limit on maximum number of inflight outgoing publishes.
    #[must_use]
    pub const fn outgoing_inflight_upper_limit(mut self, limit: u16) -> Self {
        self.options.set_outgoing_inflight_upper_limit(limit);
        self
    }

    /// Set the authentication callback.
    #[must_use]
    pub fn authenticator(mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> Self {
        self.options.set_authenticator(authenticator);
        self
    }

    /// Set the authentication callback.
    #[must_use]
    pub fn auth_manager(mut self, authenticator: Arc<Mutex<dyn Authenticator>>) -> Self {
        self.options.set_authenticator(authenticator);
        self
    }
}

#[non_exhaustive]
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OptionError {
    #[error("Unsupported URL scheme.")]
    Scheme,

    #[error(
        "Secure MQTT URL schemes require explicit TLS transport configuration via MqttOptions::set_transport(...)."
    )]
    SecureUrlRequiresExplicitTransport,

    #[error("Missing client ID.")]
    ClientId,

    #[error("Invalid Unix socket path.")]
    UnixSocketPath,

    #[cfg(feature = "websocket")]
    #[error("Invalid websocket url.")]
    WebsocketUrl,

    #[cfg(feature = "websocket")]
    #[error(
        "Secure websocket URLs require Broker::websocket(\"ws://...\") plus MqttOptions::set_transport(Transport::wss_with_config(...))."
    )]
    WssRequiresExplicitTransport,

    #[error("Invalid keep-alive value.")]
    KeepAlive,

    #[error("Invalid clean-start value.")]
    CleanStart,

    #[error("Invalid max-incoming-packet-size value.")]
    MaxIncomingPacketSize,

    #[error("Invalid max-outgoing-packet-size value.")]
    MaxOutgoingPacketSize,

    #[error("Invalid request-channel-capacity value.")]
    RequestChannelCapacity,

    #[error("Invalid max-request-batch value.")]
    MaxRequestBatch,

    #[error("Invalid read-batch-size value.")]
    ReadBatchSize,

    #[error("Invalid pending-throttle value.")]
    PendingThrottle,

    #[error("Invalid inflight value.")]
    Inflight,

    #[error("Invalid conn-timeout value.")]
    ConnTimeout,

    #[error("Unknown option: {0}")]
    Unknown(String),

    #[cfg(feature = "url")]
    #[error("Couldn't parse option from url: {0}")]
    Parse(#[from] url::ParseError),
}

#[cfg(feature = "url")]
impl std::convert::TryFrom<url::Url> for MqttOptions {
    type Error = OptionError;

    fn try_from(url: url::Url) -> Result<Self, Self::Error> {
        use std::collections::HashMap;

        let broker = match url.scheme() {
            "mqtts" | "ssl" => return Err(OptionError::SecureUrlRequiresExplicitTransport),
            "mqtt" | "tcp" => Broker::tcp(
                url.host_str().unwrap_or_default(),
                url.port().unwrap_or(DEFAULT_BROKER_PORT),
            ),
            #[cfg(unix)]
            "unix" => Broker::unix(parse_unix_socket_path(&url)?),
            #[cfg(feature = "websocket")]
            "ws" => Broker::websocket(url.as_str().to_owned())?,
            #[cfg(feature = "websocket")]
            "wss" => return Err(OptionError::WssRequiresExplicitTransport),
            _ => return Err(OptionError::Scheme),
        };

        let mut queries = url.query_pairs().collect::<HashMap<_, _>>();

        let id = queries
            .remove("client_id")
            .ok_or(OptionError::ClientId)?
            .into_owned();

        let mut options = Self::new(id, broker);
        let mut connect_props = ConnectProperties::new();

        if let Some(keep_alive) = queries
            .remove("keep_alive_secs")
            .map(|v| v.parse::<u16>().map_err(|_| OptionError::KeepAlive))
            .transpose()?
        {
            options.set_keep_alive(keep_alive);
        }

        if let Some(clean_start) = queries
            .remove("clean_start")
            .map(|v| v.parse::<bool>().map_err(|_| OptionError::CleanStart))
            .transpose()?
        {
            options.set_clean_start(clean_start);
        }

        let username = url.username();
        if let Some(password) = url.password() {
            options.set_credentials(username, password.to_owned());
        } else if !username.is_empty() {
            options.set_username(username);
        }

        connect_props.max_packet_size = queries
            .remove("max_incoming_packet_size_bytes")
            .map(|v| {
                v.parse::<u32>()
                    .map_err(|_| OptionError::MaxIncomingPacketSize)
            })
            .transpose()?;

        if let Some(request_channel_capacity) = queries
            .remove("request_channel_capacity_num")
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| OptionError::RequestChannelCapacity)
            })
            .transpose()?
        {
            options.request_channel_capacity = request_channel_capacity;
        }

        if let Some(max_request_batch) = queries
            .remove("max_request_batch_num")
            .map(|v| v.parse::<usize>().map_err(|_| OptionError::MaxRequestBatch))
            .transpose()?
        {
            options.max_request_batch = max_request_batch;
        }

        if let Some(read_batch_size) = queries
            .remove("read_batch_size_num")
            .map(|v| v.parse::<usize>().map_err(|_| OptionError::ReadBatchSize))
            .transpose()?
        {
            options.read_batch_size = read_batch_size;
        }

        if let Some(pending_throttle) = queries
            .remove("pending_throttle_usecs")
            .map(|v| v.parse::<u64>().map_err(|_| OptionError::PendingThrottle))
            .transpose()?
        {
            options.set_pending_throttle(Duration::from_micros(pending_throttle));
        }

        connect_props.receive_maximum = queries
            .remove("inflight_num")
            .map(|v| v.parse::<u16>().map_err(|_| OptionError::Inflight))
            .transpose()?;

        if let Some(conn_timeout) = queries
            .remove("conn_timeout_secs")
            .map(|v| v.parse::<u64>().map_err(|_| OptionError::ConnTimeout))
            .transpose()?
        {
            options.set_connect_timeout(Duration::from_secs(conn_timeout));
        }

        if let Some((opt, _)) = queries.into_iter().next() {
            return Err(OptionError::Unknown(opt.into_owned()));
        }

        options.set_connect_properties(connect_props);
        Ok(options)
    }
}

#[cfg(all(feature = "url", unix))]
fn parse_unix_socket_path(url: &url::Url) -> Result<PathBuf, OptionError> {
    if url.host_str().is_some() {
        return Err(OptionError::UnixSocketPath);
    }

    let path = percent_decode_str(url.path()).collect::<Vec<u8>>();
    if path.is_empty() || path == b"/" {
        return Err(OptionError::UnixSocketPath);
    }

    Ok(PathBuf::from(OsString::from_vec(path)))
}

// Implement Debug manually because ClientConfig doesn't implement it, so derive(Debug) doesn't
// work.
impl Debug for MqttOptions {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("MqttOptions")
            .field("broker", &self.broker)
            .field("keep_alive", &self.keep_alive)
            .field("clean_start", &self.clean_start)
            .field(
                "broker_session_resume_policy",
                &self.broker_session_resume_policy,
            )
            .field("client_id", &self.client_id)
            .field("auth", &self.auth)
            .field("request_channel_capacity", &self.request_channel_capacity)
            .field("max_request_batch", &self.max_request_batch)
            .field("read_batch_size", &self.read_batch_size)
            .field("pending_throttle", &self.pending_throttle)
            .field("last_will", &self.last_will)
            .field("connect_timeout", &self.connect_timeout)
            .field("topic_alias_policy", &self.topic_alias_policy)
            .field("ack_mode", &self.ack_mode)
            .field("connect properties", &self.connect_properties)
            .field("session_store", &self.session_store.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::net::{TcpListener, TcpSocket};
    use tokio::runtime::Builder;
    use tokio::sync::Notify;

    fn runtime() -> tokio::runtime::Runtime {
        Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn staggered_attempts_allow_later_success_to_win() {
        runtime().block_on(async {
            let started = Arc::new(AtomicUsize::new(0));
            let started_for_connect = Arc::clone(&started);
            let begin = std::time::Instant::now();

            let result = first_success_with_stagger(
                [0_u8, 1_u8],
                std::time::Duration::from_millis(10),
                move |attempt| {
                    let started = Arc::clone(&started_for_connect);
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        if attempt == 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            Err(std::io::Error::other("slow failure"))
                        } else {
                            Ok(42_u8)
                        }
                    }
                },
            )
            .await
            .unwrap();

            assert_eq!(result, 42);
            assert_eq!(started.load(Ordering::SeqCst), 2);
            assert!(begin.elapsed() < std::time::Duration::from_millis(150));
        });
    }

    #[test]
    fn staggered_connect_returns_invalid_input_for_empty_candidates() {
        runtime().block_on(async {
            let err = connect_resolved_addrs_staggered(Vec::new(), NetworkOptions::new())
                .await
                .unwrap_err();

            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(err.to_string(), "could not resolve to any address");
        });
    }

    #[test]
    fn staggered_connect_tries_later_candidates() {
        runtime().block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let good_addr = listener.local_addr().unwrap();

            let unused_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let bad_addr = unused_listener.local_addr().unwrap();
            drop(unused_listener);

            let accept_task = tokio::spawn(async move {
                let (_stream, _) = listener.accept().await.unwrap();
            });

            let stream =
                connect_resolved_addrs_staggered(vec![bad_addr, good_addr], NetworkOptions::new())
                    .await
                    .unwrap();
            assert_eq!(stream.peer_addr().unwrap(), good_addr);

            accept_task.await.unwrap();
        });
    }

    #[test]
    fn fixed_bind_port_retry_mode_keeps_slow_first_candidate_alive() {
        runtime().block_on(async {
            let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let bind_port = reserved.local_addr().unwrap().port();
            drop(reserved);

            let mut network_options = NetworkOptions::new();
            network_options.set_bind_addr(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                bind_port,
            )));

            let first_attempt_started = Arc::new(Notify::new());
            let second_attempt_started = Arc::new(AtomicBool::new(false));

            let mut connect_task = tokio::spawn({
                let first_attempt_started = Arc::clone(&first_attempt_started);
                let second_attempt_started = Arc::clone(&second_attempt_started);
                let network_options = network_options.clone();
                async move {
                    connect_with_retry_mode_and_delay(
                        [0_u8, 1_u8],
                        network_options,
                        Duration::from_millis(10),
                        move |attempt, network_options| {
                            let first_attempt_started = Arc::clone(&first_attempt_started);
                            let second_attempt_started = Arc::clone(&second_attempt_started);
                            async move {
                                if attempt == 0 {
                                    let bind_addr = network_options.bind_addr().unwrap();
                                    let socket = match bind_addr {
                                        SocketAddr::V4(_) => TcpSocket::new_v4()?,
                                        SocketAddr::V6(_) => TcpSocket::new_v6()?,
                                    };
                                    socket.bind(bind_addr)?;
                                    first_attempt_started.notify_one();
                                    std::future::pending::<io::Result<()>>().await
                                } else {
                                    second_attempt_started.store(true, Ordering::SeqCst);
                                    let _ = network_options;
                                    Ok(())
                                }
                            }
                        },
                    )
                    .await
                }
            });

            first_attempt_started.notified().await;

            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut connect_task)
                    .await
                    .is_err(),
                "fixed-port dialing should keep the first slow candidate alive instead of capping it to the stagger delay"
            );
            assert!(
                !second_attempt_started.load(Ordering::SeqCst),
                "fixed-port dialing should not start later same-family candidates while the first is still pending"
            );
            connect_task.abort();
        });
    }

    #[test]
    fn fixed_bind_port_resolved_addrs_try_later_candidates() {
        runtime().block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let good_addr = listener.local_addr().unwrap();

            let unused_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let bad_addr = unused_listener.local_addr().unwrap();
            drop(unused_listener);

            let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let bind_port = reserved.local_addr().unwrap().port();
            drop(reserved);

            let mut network_options = NetworkOptions::new();
            network_options.set_bind_addr(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::LOCALHOST,
                bind_port,
            )));

            let accept_task = tokio::spawn(async move {
                let (stream, peer_addr) = listener.accept().await.unwrap();
                drop(stream);
                peer_addr
            });

            let stream =
                connect_resolved_addrs_staggered(vec![bad_addr, good_addr], network_options)
                    .await
                    .unwrap();
            assert_eq!(stream.peer_addr().unwrap(), good_addr);
            drop(stream);

            let peer_addr = accept_task.await.unwrap();
            assert_eq!(peer_addr.port(), bind_port);
            assert!(peer_addr.ip().is_loopback());
        });
    }

    #[test]
    fn socket_connect_uses_custom_connector_over_default() {
        runtime().block_on(async {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let good_addr = listener.local_addr().unwrap();
            let used_custom = Arc::new(AtomicUsize::new(0));
            let used_custom_for_connector = Arc::clone(&used_custom);

            let accept_task = tokio::spawn(async move {
                let (_stream, _) = listener.accept().await.unwrap();
            });

            let mut options = MqttOptions::new("test-client", "localhost");
            options.set_socket_connector(move |_host, _network_options| {
                let used_custom = Arc::clone(&used_custom_for_connector);
                async move {
                    used_custom.fetch_add(1, Ordering::SeqCst);
                    TcpStream::connect(good_addr).await
                }
            });

            assert!(options.has_socket_connector());
            options
                .socket_connect("invalid.invalid:1883".to_owned(), NetworkOptions::new())
                .await
                .unwrap();

            assert_eq!(used_custom.load(Ordering::SeqCst), 1);
            accept_task.await.unwrap();
        });
    }

    #[cfg(all(feature = "use-rustls-no-provider", feature = "websocket"))]
    mod request_modifier_tests {
        use super::{Broker, MqttOptions};

        #[derive(Debug)]
        struct TestError;

        impl std::fmt::Display for TestError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "test error")
            }
        }

        impl std::error::Error for TestError {}

        #[test]
        fn infallible_modifier_is_set() {
            let mut options = MqttOptions::new(
                "test",
                Broker::websocket("ws://localhost:8080").expect("valid websocket broker"),
            );
            options.set_request_modifier(|req| async move { req });
            assert!(options.request_modifier().is_some());
            assert!(options.fallible_request_modifier().is_none());
        }

        #[test]
        fn fallible_modifier_is_set() {
            let mut options = MqttOptions::new(
                "test",
                Broker::websocket("ws://localhost:8080").expect("valid websocket broker"),
            );
            options.set_fallible_request_modifier(|req| async move { Ok::<_, TestError>(req) });
            assert!(options.request_modifier().is_none());
            assert!(options.fallible_request_modifier().is_some());
        }

        #[test]
        fn last_setter_call_wins() {
            let mut options = MqttOptions::new(
                "test",
                Broker::websocket("ws://localhost:8080").expect("valid websocket broker"),
            );

            options
                .set_fallible_request_modifier(|req| async move { Ok::<_, TestError>(req) })
                .set_request_modifier(|req| async move { req });
            assert!(options.request_modifier().is_some());
            assert!(options.fallible_request_modifier().is_none());

            options
                .set_request_modifier(|req| async move { req })
                .set_fallible_request_modifier(|req| async move { Ok::<_, TestError>(req) });
            assert!(options.request_modifier().is_none());
            assert!(options.fallible_request_modifier().is_some());
        }
    }

    #[test]
    fn incoming_packet_size_limit_defaults_to_default_policy() {
        let mqtt_opts = MqttOptions::new("client", "127.0.0.1");
        assert_eq!(
            mqtt_opts.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Default
        );
        assert_eq!(
            mqtt_opts.max_incoming_packet_size(),
            Some(mqtt_opts.default_max_incoming_size)
        );
    }

    #[test]
    fn set_max_packet_size_remains_backward_compatible() {
        let mut mqtt_opts = MqttOptions::new("client", "127.0.0.1");

        mqtt_opts.set_max_packet_size(Some(2048));
        assert_eq!(
            mqtt_opts.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Bytes(2048)
        );
        assert_eq!(mqtt_opts.max_packet_size(), Some(2048));
        assert_eq!(mqtt_opts.max_incoming_packet_size(), Some(2048));

        mqtt_opts.set_max_packet_size(None);
        assert_eq!(
            mqtt_opts.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Default
        );
        assert_eq!(mqtt_opts.max_packet_size(), None);
        assert_eq!(
            mqtt_opts.max_incoming_packet_size(),
            Some(mqtt_opts.default_max_incoming_size)
        );
    }

    #[test]
    fn incoming_packet_size_limit_unlimited_disables_local_check() {
        let mut mqtt_opts = MqttOptions::new("client", "127.0.0.1");
        mqtt_opts.set_unlimited_incoming_packet_size();

        assert_eq!(
            mqtt_opts.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Unlimited
        );
        assert_eq!(mqtt_opts.max_incoming_packet_size(), None);
        assert_eq!(mqtt_opts.max_packet_size(), None);
        assert!(mqtt_opts.connect_properties.is_none());
    }

    #[test]
    #[cfg(all(feature = "use-rustls-no-provider", feature = "websocket"))]
    fn websocket_transport_can_be_explicitly_upgraded_to_wss() {
        use crate::{TlsConfiguration, Transport};
        let broker = Broker::websocket(
            "ws://a3f8czas.iot.eu-west-1.amazonaws.com/mqtt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=MyCreds%2F20201001%2Feu-west-1%2Fiotdevicegateway%2Faws4_request&X-Amz-Date=20201001T130812Z&X-Amz-Expires=7200&X-Amz-Signature=9ae09b49896f44270f2707551581953e6cac71a4ccf34c7c3415555be751b2d1&X-Amz-SignedHeaders=host",
        )
        .expect("valid websocket broker");
        let mut mqttoptions = MqttOptions::new("client_a", broker);

        assert!(matches!(mqttoptions.transport(), Transport::Ws));
        mqttoptions.set_transport(Transport::wss(Vec::from("Test CA"), None, None));

        if let Transport::Wss(TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        }) = mqttoptions.transport()
        {
            assert_eq!(ca.as_slice(), b"Test CA");
            assert_eq!(client_auth, None);
            assert_eq!(alpn, None);
        } else {
            panic!("Unexpected transport!");
        }

        assert_eq!(
            mqttoptions.broker().websocket_url(),
            Some(
                "ws://a3f8czas.iot.eu-west-1.amazonaws.com/mqtt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=MyCreds%2F20201001%2Feu-west-1%2Fiotdevicegateway%2Faws4_request&X-Amz-Date=20201001T130812Z&X-Amz-Expires=7200&X-Amz-Signature=9ae09b49896f44270f2707551581953e6cac71a4ccf34c7c3415555be751b2d1&X-Amz-SignedHeaders=host"
            )
        );
    }

    #[test]
    #[cfg(feature = "websocket")]
    fn wss_websocket_urls_require_explicit_transport() {
        assert_eq!(
            Broker::websocket("wss://example.com/mqtt"),
            Err(OptionError::WssRequiresExplicitTransport)
        );
    }

    #[test]
    #[cfg(all(
        feature = "url",
        feature = "use-rustls-no-provider",
        feature = "websocket"
    ))]
    fn parse_url_ws_transport_can_be_explicitly_upgraded_to_wss() {
        use crate::{TlsConfiguration, Transport};
        let mut mqttoptions =
            MqttOptions::parse_url("ws://example.com:443/mqtt?client_id=client_a")
                .expect("valid websocket options");

        assert!(matches!(mqttoptions.transport(), Transport::Ws));
        mqttoptions.set_transport(Transport::wss(Vec::from("Test CA"), None, None));

        if let Transport::Wss(TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        }) = mqttoptions.transport()
        {
            assert_eq!(ca.as_slice(), b"Test CA");
            assert_eq!(client_auth, None);
            assert_eq!(alpn, None);
        } else {
            panic!("Unexpected transport!");
        }
    }

    #[test]
    #[cfg(all(feature = "url", feature = "use-rustls-no-provider"))]
    fn parse_url_mqtt_transport_can_be_explicitly_upgraded_to_tls() {
        use crate::{TlsConfiguration, Transport};
        let mut mqttoptions = MqttOptions::parse_url("mqtt://example.com:8883?client_id=client_a")
            .expect("valid tls options");

        assert!(matches!(mqttoptions.transport(), Transport::Tcp));
        mqttoptions.set_transport(Transport::tls(Vec::from("Test CA"), None, None));

        if let Transport::Tls(TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        }) = mqttoptions.transport()
        {
            assert_eq!(ca.as_slice(), b"Test CA");
            assert_eq!(client_auth, None);
            assert_eq!(alpn, None);
        } else {
            panic!("Unexpected transport!");
        }
    }

    #[test]
    #[cfg(feature = "url")]
    fn parse_url_rejects_secure_url_schemes() {
        assert!(matches!(
            MqttOptions::parse_url("mqtts://example.com:8883?client_id=client_a"),
            Err(OptionError::SecureUrlRequiresExplicitTransport)
        ));
        assert!(matches!(
            MqttOptions::parse_url("ssl://example.com:8883?client_id=client_a"),
            Err(OptionError::SecureUrlRequiresExplicitTransport)
        ));

        #[cfg(feature = "websocket")]
        assert!(matches!(
            MqttOptions::parse_url("wss://example.com:443/mqtt?client_id=client_a"),
            Err(OptionError::WssRequiresExplicitTransport)
        ));
    }

    #[test]
    #[cfg(feature = "url")]
    fn from_url() {
        fn opt(s: &str) -> Result<MqttOptions, OptionError> {
            MqttOptions::parse_url(s)
        }
        fn ok(s: &str) -> MqttOptions {
            opt(s).expect("valid options")
        }
        fn err(s: &str) -> OptionError {
            opt(s).expect_err("invalid options")
        }

        let v = ok("mqtt://host:42?client_id=foo");
        assert_eq!(v.broker().tcp_address(), Some(("host", 42)));
        assert_eq!(v.client_id(), "foo".to_owned());

        let v = ok("mqtt://host:42?client_id=foo&keep_alive_secs=5");
        assert_eq!(v.keep_alive, Duration::from_secs(5));
        let v = ok("mqtt://host:42?client_id=foo&keep_alive_secs=0");
        assert_eq!(v.keep_alive, Duration::from_secs(0));
        let v = ok("mqtt://host:42?client_id=foo&read_batch_size_num=32");
        assert_eq!(v.read_batch_size(), 32);
        let v = ok("mqtt://host:42?client_id=foo&conn_timeout_secs=7");
        assert_eq!(v.connect_timeout(), Duration::from_secs(7));
        let v = ok("mqtt://user@host:42?client_id=foo");
        assert_eq!(
            v.auth(),
            &ConnectAuth::Username {
                username: "user".to_owned(),
            }
        );
        let v = ok("mqtt://user:pw@host:42?client_id=foo");
        assert_eq!(
            v.auth(),
            &ConnectAuth::UsernamePassword {
                username: "user".to_owned(),
                password: Bytes::from_static(b"pw"),
            }
        );
        let v = ok("mqtt://:pw@host:42?client_id=foo");
        assert_eq!(
            v.auth(),
            &ConnectAuth::UsernamePassword {
                username: String::new(),
                password: Bytes::from_static(b"pw"),
            }
        );

        assert_eq!(err("mqtt://host:42"), OptionError::ClientId);
        assert_eq!(
            err("mqtt://host:42?client_id=foo&foo=bar"),
            OptionError::Unknown("foo".to_owned())
        );
        assert_eq!(err("mqt://host:42?client_id=foo"), OptionError::Scheme);
        assert_eq!(
            err("mqtt://host:42?client_id=foo&keep_alive_secs=foo"),
            OptionError::KeepAlive
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&keep_alive_secs=65536"),
            OptionError::KeepAlive
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&clean_start=foo"),
            OptionError::CleanStart
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&max_incoming_packet_size_bytes=foo"),
            OptionError::MaxIncomingPacketSize
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&request_channel_capacity_num=foo"),
            OptionError::RequestChannelCapacity
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&max_request_batch_num=foo"),
            OptionError::MaxRequestBatch
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&read_batch_size_num=foo"),
            OptionError::ReadBatchSize
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&pending_throttle_usecs=foo"),
            OptionError::PendingThrottle
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&inflight_num=foo"),
            OptionError::Inflight
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&conn_timeout_secs=foo"),
            OptionError::ConnTimeout
        );
    }

    #[test]
    #[cfg(unix)]
    fn unix_broker_sets_unix_transport_and_preserves_defaults() {
        let options = MqttOptions::new("client", Broker::unix("/tmp/mqtt.sock"));
        let baseline = MqttOptions::new("client", "127.0.0.1");

        assert!(matches!(options.transport(), Transport::Unix));
        assert_eq!(
            options.broker().unix_path(),
            Some(std::path::Path::new("/tmp/mqtt.sock"))
        );
        assert_eq!(options.keep_alive, baseline.keep_alive);
        assert_eq!(options.clean_start, baseline.clean_start);
        assert_eq!(options.client_id, baseline.client_id);
        assert_eq!(
            options.request_channel_capacity,
            baseline.request_channel_capacity
        );
        assert_eq!(options.max_request_batch, baseline.max_request_batch);
        assert_eq!(options.read_batch_size, baseline.read_batch_size);
        assert_eq!(options.pending_throttle, baseline.pending_throttle);
        assert_eq!(options.connect_timeout, baseline.connect_timeout);
        assert_eq!(
            options.default_max_incoming_size,
            baseline.default_max_incoming_size
        );
        assert_eq!(
            options.incoming_packet_size_limit,
            baseline.incoming_packet_size_limit
        );
        assert_eq!(options.ack_mode, baseline.ack_mode);
        assert_eq!(
            options.outgoing_inflight_upper_limit,
            baseline.outgoing_inflight_upper_limit
        );
        assert!(options.authenticator.is_none());
    }

    #[test]
    #[cfg(all(feature = "url", unix))]
    fn from_url_supports_unix_socket_paths() {
        let options = MqttOptions::parse_url(
            "unix:///tmp/mqtt.sock?client_id=foo&keep_alive_secs=5&read_batch_size_num=32",
        )
        .expect("valid unix socket options");

        assert!(matches!(options.transport(), Transport::Unix));
        assert_eq!(
            options.broker().unix_path(),
            Some(std::path::Path::new("/tmp/mqtt.sock"))
        );
        assert_eq!(options.client_id(), "foo");
        assert_eq!(options.keep_alive, Duration::from_secs(5));
        assert_eq!(options.read_batch_size(), 32);
    }

    #[test]
    #[cfg(all(feature = "url", unix))]
    fn from_url_decodes_percent_escaped_unix_socket_paths() {
        let options =
            MqttOptions::parse_url("unix:///tmp/mqtt%20broker.sock?client_id=foo").unwrap();

        assert_eq!(
            options.broker().unix_path(),
            Some(std::path::Path::new("/tmp/mqtt broker.sock"))
        );
    }

    #[test]
    #[cfg(all(feature = "url", unix))]
    fn from_url_preserves_percent_decoded_unix_socket_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let options = MqttOptions::parse_url("unix:///tmp/mqtt%FF.sock?client_id=foo").unwrap();

        assert_eq!(
            options.broker().unix_path().unwrap().as_os_str().as_bytes(),
            b"/tmp/mqtt\xff.sock"
        );
    }

    #[test]
    #[cfg(all(feature = "url", unix))]
    fn from_url_rejects_invalid_unix_socket_paths() {
        fn err(s: &str) -> OptionError {
            MqttOptions::parse_url(s).expect_err("invalid unix socket url")
        }

        assert_eq!(err("unix:///tmp/mqtt.sock"), OptionError::ClientId);
        assert_eq!(
            err("unix://localhost/tmp/mqtt.sock?client_id=foo"),
            OptionError::UnixSocketPath
        );
        assert_eq!(err("unix:///?client_id=foo"), OptionError::UnixSocketPath);
    }

    #[test]
    fn allow_empty_client_id() {
        let _mqtt_opts = MqttOptions::new("", "127.0.0.1").set_clean_start(true);
    }

    #[test]
    fn allow_empty_client_id_with_clean_start_false() {
        let _mqtt_opts = MqttOptions::new("", "127.0.0.1").set_clean_start(false);
    }

    #[test]
    fn set_session_mode_updates_clean_start_and_session_expiry() {
        let mut options = MqttOptions::new("client_id", "127.0.0.1");
        options.set_session_mode(SessionMode::Persistent);
        assert!(!options.clean_start());
        assert_eq!(
            options.session_expiry_interval(),
            Some(PERSISTENT_SESSION_EXPIRY_INTERVAL)
        );

        options.set_session_mode(SessionMode::Clean);
        assert!(options.clean_start());
        assert_eq!(options.session_expiry_interval(), None);
    }

    #[test]
    fn builder_session_mode_updates_clean_start_and_session_expiry() {
        let options = MqttOptions::builder("client_id", "127.0.0.1")
            .session_mode(SessionMode::Persistent)
            .build();

        assert!(!options.clean_start());
        assert_eq!(
            options.session_expiry_interval(),
            Some(PERSISTENT_SESSION_EXPIRY_INTERVAL)
        );
    }

    #[test]
    fn persistent_session_mode_preserves_explicit_nonzero_session_expiry() {
        let mut options = MqttOptions::new("client_id", "127.0.0.1");
        options.set_session_expiry_interval(Some(120));

        options.set_session_mode(SessionMode::Persistent);

        assert!(!options.clean_start());
        assert_eq!(options.session_expiry_interval(), Some(120));
    }

    #[test]
    fn mqtt_options_builder_matches_setter_configuration() {
        let will = LastWill::new("hello/world", "good bye", QoS::AtLeastOnce, false, None);
        let mut expected = MqttOptions::new("client", ("localhost", 1884));
        expected
            .set_keep_alive(5)
            .set_last_will(will.clone())
            .set_clean_start(false)
            .set_credentials("user", Bytes::from_static(b"password"))
            .set_request_channel_capacity(16)
            .set_max_request_batch(8)
            .set_read_batch_size(32)
            .set_pending_throttle(Duration::from_micros(250))
            .set_connect_timeout(Duration::from_secs(7))
            .set_session_expiry_interval(Some(120))
            .set_receive_maximum(Some(10))
            .set_topic_alias_max(Some(4))
            .set_request_response_info(Some(1))
            .set_request_problem_info(Some(0))
            .set_user_properties(vec![("k".to_owned(), "v".to_owned())])
            .set_authentication_method(Some("SCRAM-SHA-256".to_owned()))
            .set_authentication_data(Some(Bytes::from_static(b"auth")))
            .set_topic_alias_policy(TopicAliasPolicy::Lru)
            .set_ack_mode(AckMode::Manual)
            .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly)
            .set_outgoing_inflight_upper_limit(4);

        let actual = MqttOptions::builder("client", ("localhost", 1884))
            .keep_alive(5)
            .last_will(will)
            .clean_start(false)
            .credentials("user", Bytes::from_static(b"password"))
            .request_channel_capacity(16)
            .max_request_batch(8)
            .read_batch_size(32)
            .pending_throttle(Duration::from_micros(250))
            .connect_timeout(Duration::from_secs(7))
            .session_expiry_interval(Some(120))
            .receive_maximum(Some(10))
            .topic_alias_max(Some(4))
            .request_response_info(Some(1))
            .request_problem_info(Some(0))
            .user_properties(vec![("k".to_owned(), "v".to_owned())])
            .authentication_method(Some("SCRAM-SHA-256".to_owned()))
            .authentication_data(Some(Bytes::from_static(b"auth")))
            .topic_alias_policy(TopicAliasPolicy::Lru)
            .ack_mode(AckMode::Manual)
            .broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly)
            .outgoing_inflight_upper_limit(4)
            .build();

        assert_eq!(
            actual.broker().tcp_address(),
            expected.broker().tcp_address()
        );
        assert_eq!(actual.keep_alive(), expected.keep_alive());
        assert_eq!(actual.last_will(), expected.last_will());
        assert_eq!(actual.clean_start(), expected.clean_start());
        assert_eq!(actual.auth(), expected.auth());
        assert_eq!(
            actual.request_channel_capacity(),
            expected.request_channel_capacity()
        );
        assert_eq!(actual.max_request_batch(), expected.max_request_batch());
        assert_eq!(actual.read_batch_size(), expected.read_batch_size());
        assert_eq!(actual.pending_throttle(), expected.pending_throttle());
        assert_eq!(actual.connect_timeout(), expected.connect_timeout());
        assert_eq!(actual.connect_properties(), expected.connect_properties());
        assert_eq!(actual.topic_alias_policy(), expected.topic_alias_policy());
        assert_eq!(actual.ack_mode(), expected.ack_mode());
        assert_eq!(
            actual.broker_session_resume_policy(),
            expected.broker_session_resume_policy()
        );
        assert_eq!(
            actual.get_outgoing_inflight_upper_limit(),
            expected.get_outgoing_inflight_upper_limit()
        );
    }

    #[test]
    fn broker_only_session_resume_compatibility_mode_defaults_to_strict() {
        let options = MqttOptions::new("client", "localhost");

        assert_eq!(
            options.broker_session_resume_policy(),
            BrokerSessionResumePolicy::Strict
        );
    }

    #[test]
    fn mqtt_options_builder_configures_packet_size_policies() {
        let properties = ConnectProperties {
            max_packet_size: Some(2048),
            ..ConnectProperties::new()
        };

        let from_properties = MqttOptions::builder("client", "localhost")
            .connect_properties(properties)
            .build();
        assert_eq!(
            from_properties.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Bytes(2048)
        );

        let unlimited = MqttOptions::builder("client", "localhost")
            .max_packet_size(Some(1024))
            .unlimited_incoming_packet_size()
            .build();
        assert_eq!(
            unlimited.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Unlimited
        );
        assert_eq!(unlimited.max_packet_size(), None);
    }

    #[test]
    fn mqtt_options_builder_can_replace_and_clear_auth() {
        let actual = MqttOptions::builder("client", "localhost")
            .password("password")
            .clear_auth()
            .auth(ConnectAuth::Username {
                username: "next".to_owned(),
            })
            .build();

        assert_eq!(
            actual.auth(),
            &ConnectAuth::Username {
                username: "next".to_owned(),
            }
        );
    }

    #[test]
    fn mqtt_options_builder_request_capacity_feeds_client_builder_default() {
        let mqttoptions = MqttOptions::builder("test-1", "localhost")
            .request_channel_capacity(1)
            .build();
        let (client, _eventloop) = AsyncClient::builder(mqttoptions).build();

        client
            .try_publish("hello/world", QoS::AtMostOnce, false, "one")
            .expect("first request should fit configured capacity");
        assert!(matches!(
            client.try_publish("hello/world", QoS::AtMostOnce, false, "two"),
            Err(ClientError::TryRequest(request)) if matches!(*request, Request::Publish(_))
        ));
    }

    #[test]
    fn read_batch_size_defaults_to_adaptive() {
        let options = MqttOptions::new("client", "127.0.0.1");
        assert_eq!(options.read_batch_size(), 0);
    }

    #[test]
    fn set_read_batch_size() {
        let mut options = MqttOptions::new("client", "127.0.0.1");
        options.set_read_batch_size(48);
        assert_eq!(options.read_batch_size(), 48);
    }

    #[test]
    #[cfg(feature = "url")]
    fn from_url_uses_default_incoming_limit_when_unspecified() {
        let mqtt_opts = MqttOptions::parse_url("mqtt://host:42?client_id=foo").unwrap();
        assert_eq!(
            mqtt_opts.incoming_packet_size_limit(),
            IncomingPacketSizeLimit::Default
        );
        assert_eq!(
            mqtt_opts.max_incoming_packet_size(),
            Some(mqtt_opts.default_max_incoming_size)
        );
    }
}
