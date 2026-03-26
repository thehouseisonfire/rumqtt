#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(clippy::default_trait_access)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::explicit_iter_loop)]
#![allow(clippy::if_not_else)]
#![allow(clippy::ignored_unit_patterns)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::too_long_first_doc_paragraph)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::use_self)]
#![allow(clippy::wildcard_imports)]

#[cfg(all(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
compile_error!(
    "Features `use-rustls-ring` and `use-rustls-aws-lc` are mutually exclusive. Enable only one rustls provider feature."
);

#[macro_use]
extern crate log;

use bytes::Bytes;
use std::fmt::{self, Debug, Formatter};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(all(feature = "url", unix))]
use percent_encoding::percent_decode_str;

#[cfg(all(feature = "url", unix))]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

#[cfg(feature = "websocket")]
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
};

mod client;
mod eventloop;
mod framed;
pub mod mqttbytes;
mod notice;
mod state;
mod transport;

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
mod tls;

#[cfg(feature = "websocket")]
mod websockets;

#[cfg(feature = "proxy")]
mod proxy;

use mqttbytes::v5::*;

pub use client::{
    AsyncClient, Client, ClientError, Connection, InvalidTopic, Iter, ManualAck, PublishTopic,
    RecvError, RecvTimeoutError, TryRecvError, ValidatedTopic,
};
pub use eventloop::{ConnectionError, Event, EventLoop};
pub use mqttbytes::*;
pub use notice::{
    NoticeFailureReason, PublishNotice, PublishNoticeError, RequestNotice, RequestNoticeError,
};
pub use rumqttc_core::NetworkOptions;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::TlsConfiguration;
pub use rumqttc_core::default_socket_connect;
pub use state::{MqttState, StateError};
pub use transport::Transport;

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
    /// Publish packet with packet identifier. 0 implies QoS 0
    Publish(u16),
    /// Subscribe packet with packet identifier
    Subscribe(u16),
    /// Unsubscribe packet with packet identifier
    Unsubscribe(u16),
    /// PubAck packet
    PubAck(u16),
    /// PubRec packet
    PubRec(u16),
    /// PubRel packet
    PubRel(u16),
    /// PubComp packet
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
    pub fn tcp_address(&self) -> Option<(&str, u16)> {
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
            _ => None,
        }
    }

    #[cfg(feature = "websocket")]
    #[must_use]
    pub fn websocket_url(&self) -> Option<&str> {
        match &self.inner {
            BrokerInner::Websocket { url } => Some(url.as_str()),
            _ => None,
        }
    }

    pub(crate) fn default_transport(&self) -> Transport {
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

pub trait AuthManager: std::fmt::Debug + Send {
    /// Process authentication data received from the server and generate authentication data to be sent back.
    ///
    /// # Arguments
    ///
    /// * `auth_prop` - The authentication Properties received from the server.
    ///
    /// # Returns
    ///
    /// * `Ok(auth_prop)` - The authentication Properties to be sent back to the server.
    /// * `Err(error_message)` - An error indicating that the authentication process has failed or terminated.
    fn auth_continue(
        &mut self,
        auth_prop: Option<AuthProperties>,
    ) -> Result<Option<AuthProperties>, String>;
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
pub struct MqttOptions {
    /// broker target that you want to connect to
    broker: Broker,
    transport: Transport,
    /// keep alive time to send pingreq to broker when the connection is idle
    keep_alive: Duration,
    /// clean (or) persistent session
    clean_start: bool,
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
    /// If set to `true` MQTT acknowledgements are not sent automatically.
    /// Every incoming publish packet must be acknowledged manually with either
    /// `client.ack(...)` or the `prepare_ack(...)` + `manual_ack(...)` flow.
    manual_acks: bool,
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

    auth_manager: Option<Arc<Mutex<dyn AuthManager>>>,
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
    pub fn new<S: Into<String>, B: Into<Broker>>(id: S, broker: B) -> MqttOptions {
        let broker = broker.into();
        MqttOptions {
            transport: broker.default_transport(),
            broker,
            keep_alive: Duration::from_secs(60),
            clean_start: true,
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
            manual_acks: false,
            network_options: NetworkOptions::new(),
            #[cfg(feature = "proxy")]
            proxy: None,
            outgoing_inflight_upper_limit: None,
            #[cfg(feature = "websocket")]
            request_modifier: None,
            #[cfg(feature = "websocket")]
            fallible_request_modifier: None,
            socket_connector: None,
            auth_manager: None,
        }
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
    pub fn parse_url<S: Into<String>>(url: S) -> Result<MqttOptions, OptionError> {
        let url = url::Url::parse(&url.into())?;
        let options = MqttOptions::try_from(url)?;

        Ok(options)
    }

    /// Broker target
    pub fn broker(&self) -> &Broker {
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
                Ok(Box::new(stream) as Box<dyn crate::framed::AsyncReadWrite>)
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
    pub fn keep_alive(&self) -> Duration {
        self.keep_alive
    }

    /// Client identifier
    pub fn client_id(&self) -> String {
        self.client_id.clone()
    }

    /// `clean_start = true` removes all the state from queues & instructs the broker
    /// to clean all the client state when client disconnects.
    ///
    /// When set `false`, broker will hold the client state and performs pending
    /// operations on the client when reconnection with same `client_id`
    /// happens. Local queue state is also held to retransmit packets after reconnection.
    pub fn set_clean_start(&mut self, clean_start: bool) -> &mut Self {
        self.clean_start = clean_start;
        self
    }

    /// Clean session
    pub fn clean_start(&self) -> bool {
        self.clean_start
    }

    /// Replace the current CONNECT authentication state.
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
    pub fn set_username<U: Into<String>>(&mut self, username: U) -> &mut Self {
        self.auth = ConnectAuth::Username {
            username: username.into(),
        };
        self
    }

    /// Set only the MQTT password field.
    pub fn set_password<P: Into<Bytes>>(&mut self, password: P) -> &mut Self {
        self.auth = ConnectAuth::Password {
            password: password.into(),
        };
        self
    }

    /// Set both MQTT username and binary password fields.
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
    pub fn auth(&self) -> &ConnectAuth {
        &self.auth
    }

    /// Set request channel capacity
    pub fn set_request_channel_capacity(&mut self, capacity: usize) -> &mut Self {
        self.request_channel_capacity = capacity;
        self
    }

    /// Request channel capacity
    pub fn request_channel_capacity(&self) -> usize {
        self.request_channel_capacity
    }

    /// Set maximum number of requests processed in one eventloop iteration.
    ///
    /// `0` preserves legacy behavior (effectively processes one request).
    pub fn set_max_request_batch(&mut self, max: usize) -> &mut Self {
        self.max_request_batch = max;
        self
    }

    /// Maximum number of requests processed in one eventloop iteration.
    pub fn max_request_batch(&self) -> usize {
        self.max_request_batch
    }

    /// Set maximum number of packets processed in one network read batch.
    ///
    /// `0` enables adaptive batching.
    pub fn set_read_batch_size(&mut self, size: usize) -> &mut Self {
        self.read_batch_size = size;
        self
    }

    /// Maximum number of packets processed in one network read batch.
    ///
    /// `0` means adaptive batching.
    pub fn read_batch_size(&self) -> usize {
        self.read_batch_size
    }

    /// Enables throttling and sets outoing message rate to the specified 'rate'
    pub fn set_pending_throttle(&mut self, duration: Duration) -> &mut Self {
        self.pending_throttle = duration;
        self
    }

    /// Outgoing message rate
    pub fn pending_throttle(&self) -> Duration {
        self.pending_throttle
    }

    /// set connect timeout
    pub fn set_connect_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.connect_timeout = timeout;
        self
    }

    /// get connect timeout
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// set connection properties
    pub fn set_connect_properties(&mut self, properties: ConnectProperties) -> &mut Self {
        self.incoming_packet_size_limit = match properties.max_packet_size {
            Some(max_size) => IncomingPacketSizeLimit::Bytes(max_size),
            None => IncomingPacketSizeLimit::Default,
        };
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
    pub fn session_expiry_interval(&self) -> Option<u32> {
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
    pub fn receive_maximum(&self) -> Option<u16> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.receive_maximum
        } else {
            None
        }
    }

    /// set max packet size on connection properties
    pub fn set_max_packet_size(&mut self, max_size: Option<u32>) -> &mut Self {
        self.incoming_packet_size_limit = match max_size {
            Some(max_size) => IncomingPacketSizeLimit::Bytes(max_size),
            None => IncomingPacketSizeLimit::Default,
        };

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
    pub fn max_packet_size(&self) -> Option<u32> {
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
    pub fn incoming_packet_size_limit(&self) -> IncomingPacketSizeLimit {
        self.incoming_packet_size_limit
    }

    pub(crate) fn max_incoming_packet_size(&self) -> Option<u32> {
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
    pub fn topic_alias_max(&self) -> Option<u16> {
        if let Some(conn_props) = &self.connect_properties {
            conn_props.topic_alias_max
        } else {
            None
        }
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
    pub fn request_response_info(&self) -> Option<u8> {
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
    pub fn request_problem_info(&self) -> Option<u8> {
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
        if let Some(conn_props) = &self.connect_properties {
            conn_props.user_properties.clone()
        } else {
            Vec::new()
        }
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
        if let Some(conn_props) = &self.connect_properties {
            conn_props.authentication_method.clone()
        } else {
            None
        }
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
        if let Some(conn_props) = &self.connect_properties {
            conn_props.authentication_data.clone()
        } else {
            None
        }
    }

    /// set manual acknowledgements
    pub fn set_manual_acks(&mut self, manual_acks: bool) -> &mut Self {
        self.manual_acks = manual_acks;
        self
    }

    /// get manual acknowledgements
    pub fn manual_acks(&self) -> bool {
        self.manual_acks
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

    #[cfg(feature = "proxy")]
    pub(crate) fn socket_connector(&self) -> Option<SocketConnector> {
        self.socket_connector.clone()
    }

    pub(crate) async fn socket_connect(
        &self,
        host: String,
        network_options: NetworkOptions,
    ) -> std::io::Result<Box<dyn crate::framed::AsyncReadWrite>> {
        if let Some(connector) = &self.socket_connector {
            connector(host, network_options).await
        } else {
            let tcp = default_socket_connect(host, network_options).await?;
            Ok(Box::new(tcp))
        }
    }

    /// Get the upper limit on maximum number of inflight outgoing publishes.
    /// The server may set its own maximum inflight limit, the smaller of the two will be used.
    pub fn set_outgoing_inflight_upper_limit(&mut self, limit: u16) -> &mut Self {
        self.outgoing_inflight_upper_limit = Some(limit);
        self
    }

    /// Set the upper limit on maximum number of inflight outgoing publishes.
    /// The server may set its own maximum inflight limit, the smaller of the two will be used.
    pub fn get_outgoing_inflight_upper_limit(&self) -> Option<u16> {
        self.outgoing_inflight_upper_limit
    }

    pub fn set_auth_manager(&mut self, auth_manager: Arc<Mutex<dyn AuthManager>>) -> &mut Self {
        self.auth_manager = Some(auth_manager);
        self
    }

    pub fn auth_manager(&self) -> Option<Arc<Mutex<dyn AuthManager>>> {
        self.auth_manager.as_ref()?;

        self.auth_manager.clone()
    }
}

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
        "Secure websocket URLs require Broker::websocket(\"ws://...\") plus MqttOptions::set_transport(Transport::wss(...))."
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

        let mut options = MqttOptions::new(id, broker);
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
        let password = url.password();
        if password.is_some() {
            options.set_credentials(username, password.unwrap_or_default().to_owned());
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
            .field("client_id", &self.client_id)
            .field("auth", &self.auth)
            .field("request_channel_capacity", &self.request_channel_capacity)
            .field("max_request_batch", &self.max_request_batch)
            .field("read_batch_size", &self.read_batch_size)
            .field("pending_throttle", &self.pending_throttle)
            .field("last_will", &self.last_will)
            .field("connect_timeout", &self.connect_timeout)
            .field("manual_acks", &self.manual_acks)
            .field("connect properties", &self.connect_properties)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod test {
    use super::*;

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
        assert_eq!(options.manual_acks, baseline.manual_acks);
        assert_eq!(
            options.outgoing_inflight_upper_limit,
            baseline.outgoing_inflight_upper_limit
        );
        assert!(options.auth_manager.is_none());
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
