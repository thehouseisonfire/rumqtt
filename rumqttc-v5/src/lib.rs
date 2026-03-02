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
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpSocket, TcpStream, lookup_host};

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
pub use state::{MqttState, StateError};

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use crate::tls::Error as TlsError;

#[cfg(feature = "proxy")]
pub use crate::proxy::{Proxy, ProxyAuth, ProxyType};

#[cfg(feature = "use-rustls-no-provider")]
use rustls_native_certs::load_native_certs;
#[cfg(feature = "use-native-tls")]
pub use tokio_native_tls;
#[cfg(feature = "use-native-tls")]
use tokio_native_tls::native_tls::TlsConnector;
#[cfg(feature = "use-rustls-no-provider")]
pub use tokio_rustls;
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

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
pub(crate) type SocketConnector = Arc<
    dyn Fn(
            String,
            NetworkOptions,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<Box<dyn crate::framed::AsyncReadWrite>, std::io::Error>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

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

/// Transport methods. Defaults to TCP.
#[derive(Clone)]
pub enum Transport {
    Tcp,
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    Tls(TlsConfiguration),
    #[cfg(unix)]
    Unix,
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    Ws,
    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    Wss(TlsConfiguration),
}

impl Default for Transport {
    fn default() -> Self {
        Self::tcp()
    }
}

impl Transport {
    /// Use regular tcp as transport (default)
    #[must_use]
    pub fn tcp() -> Self {
        Self::Tcp
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[must_use]
    pub fn tls_with_default_config() -> Self {
        Self::tls_with_config(Default::default())
    }

    /// Use secure tcp with tls as transport
    #[cfg(feature = "use-rustls-no-provider")]
    #[must_use]
    pub fn tls(
        ca: Vec<u8>,
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
        alpn: Option<Vec<Vec<u8>>>,
    ) -> Self {
        let config = TlsConfiguration::Simple {
            ca,
            alpn,
            client_auth,
        };

        Self::tls_with_config(config)
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[must_use]
    pub fn tls_with_config(tls_config: TlsConfiguration) -> Self {
        Self::Tls(tls_config)
    }

    #[cfg(unix)]
    #[must_use]
    pub fn unix() -> Self {
        Self::Unix
    }

    /// Use websockets as transport
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    pub fn ws() -> Self {
        Self::Ws
    }

    /// Use secure websockets with tls as transport
    #[cfg(all(feature = "use-rustls-no-provider", feature = "websocket"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "use-rustls-no-provider", feature = "websocket")))
    )]
    pub fn wss(
        ca: Vec<u8>,
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
        alpn: Option<Vec<Vec<u8>>>,
    ) -> Self {
        let config = TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        };

        Self::wss_with_config(config)
    }

    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    pub fn wss_with_config(tls_config: TlsConfiguration) -> Self {
        Self::Wss(tls_config)
    }

    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    pub fn wss_with_default_config() -> Self {
        Self::Wss(Default::default())
    }
}

/// TLS configuration method
#[derive(Clone, Debug)]
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub enum TlsConfiguration {
    #[cfg(feature = "use-rustls-no-provider")]
    Simple {
        /// ca certificate
        ca: Vec<u8>,
        /// alpn settings
        alpn: Option<Vec<Vec<u8>>>,
        /// tls client_authentication
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
    },
    #[cfg(feature = "use-native-tls")]
    SimpleNative {
        /// ca certificate
        ca: Vec<u8>,
        /// pkcs12 binary der and
        /// password for use with der
        client_auth: Option<(Vec<u8>, String)>,
    },
    #[cfg(feature = "use-rustls-no-provider")]
    /// Injected rustls ClientConfig for TLS, to allow more customisation.
    Rustls(Arc<ClientConfig>),
    #[cfg(feature = "use-native-tls")]
    /// Use default native-tls configuration
    Native,
    #[cfg(feature = "use-native-tls")]
    /// Injected native-tls TlsConnector for TLS, to allow more customisation.
    NativeConnector(TlsConnector),
}

#[cfg(all(feature = "use-rustls-no-provider", not(feature = "use-native-tls")))]
#[allow(clippy::derivable_impls)]
impl Default for TlsConfiguration {
    fn default() -> Self {
        let mut root_cert_store = RootCertStore::empty();
        for cert in load_native_certs().expect("could not load platform certs") {
            root_cert_store.add(cert).unwrap();
        }
        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        Self::Rustls(Arc::new(tls_config))
    }
}

#[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
#[allow(clippy::derivable_impls)]
impl Default for TlsConfiguration {
    fn default() -> Self {
        Self::Native
    }
}

#[cfg(all(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
#[allow(clippy::derivable_impls)]
impl Default for TlsConfiguration {
    fn default() -> Self {
        // When both are enabled, prefer rustls
        let mut root_cert_store = RootCertStore::empty();
        for cert in load_native_certs().expect("could not load platform certs") {
            root_cert_store.add(cert).unwrap();
        }
        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        Self::Rustls(Arc::new(tls_config))
    }
}

#[cfg(feature = "use-rustls-no-provider")]
impl From<ClientConfig> for TlsConfiguration {
    fn from(config: ClientConfig) -> Self {
        TlsConfiguration::Rustls(Arc::new(config))
    }
}

#[cfg(feature = "use-native-tls")]
impl From<TlsConnector> for TlsConfiguration {
    fn from(connector: TlsConnector) -> Self {
        TlsConfiguration::NativeConnector(connector)
    }
}

/// Provides a way to configure low level network connection configurations
#[derive(Clone, Debug, Default)]
pub struct NetworkOptions {
    tcp_send_buffer_size: Option<u32>,
    tcp_recv_buffer_size: Option<u32>,
    tcp_nodelay: bool,
    conn_timeout: u64,
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    bind_device: Option<String>,
}

impl NetworkOptions {
    #[must_use]
    pub fn new() -> Self {
        NetworkOptions {
            tcp_send_buffer_size: None,
            tcp_recv_buffer_size: None,
            tcp_nodelay: false,
            conn_timeout: 5,
            #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
            bind_device: None,
        }
    }

    pub fn set_tcp_nodelay(&mut self, nodelay: bool) {
        self.tcp_nodelay = nodelay;
    }

    pub fn set_tcp_send_buffer_size(&mut self, size: u32) {
        self.tcp_send_buffer_size = Some(size);
    }

    pub fn set_tcp_recv_buffer_size(&mut self, size: u32) {
        self.tcp_recv_buffer_size = Some(size);
    }

    /// set connection timeout in secs
    pub fn set_connection_timeout(&mut self, timeout: u64) -> &mut Self {
        self.conn_timeout = timeout;
        self
    }

    /// get timeout in secs
    #[must_use]
    pub fn connection_timeout(&self) -> u64 {
        self.conn_timeout
    }

    /// bind connection to a specific network device by name
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))
    )]
    pub fn set_bind_device(&mut self, bind_device: &str) -> &mut Self {
        self.bind_device = Some(bind_device.to_string());
        self
    }
}

/// Options to configure the behaviour of MQTT connection
#[derive(Clone)]
pub struct MqttOptions {
    /// broker address that you want to connect to
    broker_addr: String,
    /// broker port
    port: u16,
    // What transport protocol to use
    transport: Transport,
    /// keep alive time to send pingreq to broker when the connection is idle
    keep_alive: Duration,
    /// clean (or) persistent session
    clean_start: bool,
    /// client identifier
    client_id: String,
    /// username and password
    credentials: Option<Login>,
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
    /// - host: The broker's domain name or IP address
    /// - port: The port number on which broker must be listening for incoming connections
    ///
    /// ```
    /// # use rumqttc::MqttOptions;
    /// let options = MqttOptions::new("123", "localhost", 1883);
    /// ```
    pub fn new<S: Into<String>, T: Into<String>>(id: S, host: T, port: u16) -> MqttOptions {
        MqttOptions {
            broker_addr: host.into(),
            port,
            transport: Transport::tcp(),
            keep_alive: Duration::from_secs(60),
            clean_start: true,
            client_id: id.into(),
            credentials: None,
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
    /// **NOTE:** A url must be prefixed with one of either `tcp://`, `mqtt://`, `ssl://`,`mqtts://`,
    /// `ws://` or `wss://` to denote the protocol for establishing a connection with the broker.
    ///
    /// **NOTE:** Encrypted connections(i.e. `mqtts://`, `ssl://`, `wss://`) by default use the
    /// system's root certificates. To configure with custom certificates, one may use the
    /// [`set_transport`](MqttOptions::set_transport) method.
    ///
    /// ```ignore
    /// # use rumqttc::{MqttOptions, Transport};
    /// # use tokio_rustls::rustls::ClientConfig;
    /// # let root_cert_store = rustls::RootCertStore::empty();
    /// # let client_config = ClientConfig::builder()
    /// #    .with_root_certificates(root_cert_store)
    /// #    .with_no_client_auth();
    /// let mut options = MqttOptions::parse_url("mqtts://example.com?client_id=123").unwrap();
    /// options.set_transport(Transport::tls_with_config(client_config.into()));
    /// ```
    pub fn parse_url<S: Into<String>>(url: S) -> Result<MqttOptions, OptionError> {
        let url = url::Url::parse(&url.into())?;
        let options = MqttOptions::try_from(url)?;

        Ok(options)
    }

    /// Broker address
    pub fn broker_address(&self) -> (String, u16) {
        (self.broker_addr.clone(), self.port)
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
    /// # let mut options = MqttOptions::new("test", "localhost", 1883);
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
    /// # let mut options = MqttOptions::new("test", "localhost", 1883);
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

    /// Username and password
    pub fn set_credentials<U: Into<String>, P: Into<String>>(
        &mut self,
        username: U,
        password: P,
    ) -> &mut Self {
        self.credentials = Some(Login::new(username, password));
        self
    }

    /// Security options
    pub fn credentials(&self) -> Option<Login> {
        self.credentials.clone()
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

/// Default TCP socket connection logic used by the MQTT event loop.
///
/// This resolves the host, applies [`NetworkOptions`] on each candidate socket,
/// and returns the first successful connection.
pub async fn default_socket_connect(
    host: String,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let addrs = lookup_host(host).await?;
    let mut last_err = None;

    for addr in addrs {
        let socket = match addr {
            SocketAddr::V4(_) => TcpSocket::new_v4()?,
            SocketAddr::V6(_) => TcpSocket::new_v6()?,
        };

        socket.set_nodelay(network_options.tcp_nodelay)?;

        if let Some(send_buff_size) = network_options.tcp_send_buffer_size {
            socket.set_send_buffer_size(send_buff_size)?;
        }
        if let Some(recv_buffer_size) = network_options.tcp_recv_buffer_size {
            socket.set_recv_buffer_size(recv_buffer_size)?;
        }

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        {
            if let Some(bind_device) = &network_options.bind_device {
                socket.bind_device(Some(bind_device.as_bytes()))?;
            }
        }

        match socket.connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                last_err = Some(e);
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

#[cfg(feature = "url")]
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OptionError {
    #[error("Unsupported URL scheme.")]
    Scheme,

    #[error("Missing client ID.")]
    ClientId,

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

    #[error("Couldn't parse option from url: {0}")]
    Parse(#[from] url::ParseError),
}

#[cfg(feature = "url")]
impl std::convert::TryFrom<url::Url> for MqttOptions {
    type Error = OptionError;

    fn try_from(url: url::Url) -> Result<Self, Self::Error> {
        use std::collections::HashMap;

        let host = url.host_str().unwrap_or_default().to_owned();

        let (transport, default_port) = match url.scheme() {
            // Encrypted connections are supported, but require explicit TLS configuration. We fall
            // back to the unencrypted transport layer, so that `set_transport` can be used to
            // configure the encrypted transport layer with the provided TLS configuration.
            #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
            "mqtts" | "ssl" => (Transport::tls_with_default_config(), 8883),
            "mqtt" | "tcp" => (Transport::Tcp, 1883),
            #[cfg(feature = "websocket")]
            "ws" => (Transport::Ws, 8000),
            #[cfg(all(
                any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
                feature = "websocket"
            ))]
            "wss" => (Transport::wss_with_default_config(), 8000),
            _ => return Err(OptionError::Scheme),
        };

        let port = url.port().unwrap_or(default_port);

        let mut queries = url.query_pairs().collect::<HashMap<_, _>>();

        let id = queries
            .remove("client_id")
            .ok_or(OptionError::ClientId)?
            .into_owned();

        let mut options = MqttOptions::new(id, host, port);
        let mut connect_props = ConnectProperties::new();
        options.set_transport(transport);

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

        if let Some((username, password)) = {
            match url.username() {
                "" => None,
                username => Some((
                    username.to_owned(),
                    url.password().unwrap_or_default().to_owned(),
                )),
            }
        } {
            options.set_credentials(username, password);
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

// Implement Debug manually because ClientConfig doesn't implement it, so derive(Debug) doesn't
// work.
impl Debug for MqttOptions {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("MqttOptions")
            .field("broker_addr", &self.broker_addr)
            .field("port", &self.port)
            .field("keep_alive", &self.keep_alive)
            .field("clean_start", &self.clean_start)
            .field("client_id", &self.client_id)
            .field("credentials", &self.credentials)
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
        use super::MqttOptions;

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
            let mut options = MqttOptions::new("test", "ws://localhost", 8080);
            options.set_request_modifier(|req| async move { req });
            assert!(options.request_modifier().is_some());
            assert!(options.fallible_request_modifier().is_none());
        }

        #[test]
        fn fallible_modifier_is_set() {
            let mut options = MqttOptions::new("test", "ws://localhost", 8080);
            options.set_fallible_request_modifier(|req| async move { Ok::<_, TestError>(req) });
            assert!(options.request_modifier().is_none());
            assert!(options.fallible_request_modifier().is_some());
        }

        #[test]
        fn last_setter_call_wins() {
            let mut options = MqttOptions::new("test", "ws://localhost", 8080);

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
        let mqtt_opts = MqttOptions::new("client", "127.0.0.1", 1883);
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
        let mut mqtt_opts = MqttOptions::new("client", "127.0.0.1", 1883);

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
        let mut mqtt_opts = MqttOptions::new("client", "127.0.0.1", 1883);
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
    fn no_scheme() {
        use crate::{TlsConfiguration, Transport};
        let mut mqttoptions = MqttOptions::new(
            "client_a",
            "a3f8czas.iot.eu-west-1.amazonaws.com/mqtt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=MyCreds%2F20201001%2Feu-west-1%2Fiotdevicegateway%2Faws4_request&X-Amz-Date=20201001T130812Z&X-Amz-Expires=7200&X-Amz-Signature=9ae09b49896f44270f2707551581953e6cac71a4ccf34c7c3415555be751b2d1&X-Amz-SignedHeaders=host",
            443,
        );

        mqttoptions.set_transport(Transport::wss(Vec::from("Test CA"), None, None));

        if let Transport::Wss(TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        }) = mqttoptions.transport
        {
            assert_eq!(ca, Vec::from("Test CA"));
            assert_eq!(client_auth, None);
            assert_eq!(alpn, None);
        } else {
            panic!("Unexpected transport!");
        }

        assert_eq!(
            mqttoptions.broker_addr,
            "a3f8czas.iot.eu-west-1.amazonaws.com/mqtt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=MyCreds%2F20201001%2Feu-west-1%2Fiotdevicegateway%2Faws4_request&X-Amz-Date=20201001T130812Z&X-Amz-Expires=7200&X-Amz-Signature=9ae09b49896f44270f2707551581953e6cac71a4ccf34c7c3415555be751b2d1&X-Amz-SignedHeaders=host"
        );
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
        assert_eq!(v.broker_address(), ("host".to_owned(), 42));
        assert_eq!(v.client_id(), "foo".to_owned());

        let v = ok("mqtt://host:42?client_id=foo&keep_alive_secs=5");
        assert_eq!(v.keep_alive, Duration::from_secs(5));
        let v = ok("mqtt://host:42?client_id=foo&keep_alive_secs=0");
        assert_eq!(v.keep_alive, Duration::from_secs(0));
        let v = ok("mqtt://host:42?client_id=foo&read_batch_size_num=32");
        assert_eq!(v.read_batch_size(), 32);
        let v = ok("mqtt://host:42?client_id=foo&conn_timeout_secs=7");
        assert_eq!(v.connect_timeout(), Duration::from_secs(7));

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
    fn allow_empty_client_id() {
        let _mqtt_opts = MqttOptions::new("", "127.0.0.1", 1883).set_clean_start(true);
    }

    #[test]
    fn read_batch_size_defaults_to_adaptive() {
        let options = MqttOptions::new("client", "127.0.0.1", 1883);
        assert_eq!(options.read_batch_size(), 0);
    }

    #[test]
    fn set_read_batch_size() {
        let mut options = MqttOptions::new("client", "127.0.0.1", 1883);
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
