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

use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;
use std::time::Duration;

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

#[cfg(feature = "websocket")]
use std::{
    future::{Future, IntoFuture},
    pin::Pin,
};

#[cfg(feature = "websocket")]
type RequestModifierError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(feature = "websocket")]
type RequestModifierFn = Arc<
    dyn Fn(http::Request<()>) -> Pin<Box<dyn Future<Output = http::Request<()>> + Send>>
        + Send
        + Sync,
>;

#[cfg(feature = "websocket")]
type FallibleRequestModifierFn = Arc<
    dyn Fn(
            http::Request<()>,
        )
            -> Pin<Box<dyn Future<Output = Result<http::Request<()>, RequestModifierError>> + Send>>
        + Send
        + Sync,
>;

#[cfg(feature = "proxy")]
mod proxy;

pub use client::{
    AsyncClient, Client, ClientError, Connection, InvalidTopic, Iter, ManualAck, PublishTopic,
    RecvError, RecvTimeoutError, TryRecvError, ValidatedTopic,
};
pub use eventloop::{ConnectionError, Event, EventLoop};
pub use mqttbytes::v4::*;
pub use mqttbytes::*;
pub use notice::{
    NoticeFailureReason, PublishNotice, PublishNoticeError, RequestNotice, RequestNoticeError,
};
pub use rumqttc_core::NetworkOptions;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::TlsConfiguration;
pub use rumqttc_core::default_socket_connect;
pub use state::{MqttState, StateError};
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use tls::Error as TlsError;
#[cfg(feature = "use-native-tls")]
pub use tokio_native_tls;
#[cfg(feature = "use-rustls-no-provider")]
pub use tokio_rustls;
pub use transport::Transport;

#[cfg(feature = "proxy")]
pub use proxy::{Proxy, ProxyAuth, ProxyType};

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
    PingReq(PingReq),
    PingResp(PingResp),
    Subscribe(Subscribe),
    SubAck(SubAck),
    Unsubscribe(Unsubscribe),
    UnsubAck(UnsubAck),
    Disconnect(Disconnect),
}

impl From<Publish> for Request {
    fn from(publish: Publish) -> Request {
        Request::Publish(publish)
    }
}

impl From<Subscribe> for Request {
    fn from(subscribe: Subscribe) -> Request {
        Request::Subscribe(subscribe)
    }
}

impl From<Unsubscribe> for Request {
    fn from(unsubscribe: Unsubscribe) -> Request {
        Request::Unsubscribe(unsubscribe)
    }
}

/// Custom socket connector used to establish the underlying stream before optional proxy/TLS layers.
pub(crate) type SocketConnector = rumqttc_core::SocketConnector;

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
    clean_session: bool,
    /// client identifier
    client_id: String,
    /// username and password
    credentials: Option<Login>,
    /// maximum incoming packet size (verifies remaining length of the packet)
    max_incoming_packet_size: usize,
    /// Maximum outgoing packet size (only verifies publish payload size)
    max_outgoing_packet_size: usize,
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
    /// maximum number of outgoing inflight messages
    inflight: u16,
    /// Last will that will be issued on unexpected disconnect
    last_will: Option<LastWill>,
    /// If set to `true` MQTT acknowledgements are not sent automatically.
    /// Every incoming publish packet must be acknowledged manually with either
    /// `client.ack(...)` or the `prepare_ack(...)` + `manual_ack(...)` flow.
    manual_acks: bool,
    #[cfg(feature = "proxy")]
    /// Proxy configuration.
    proxy: Option<Proxy>,
    #[cfg(feature = "websocket")]
    request_modifier: Option<RequestModifierFn>,
    #[cfg(feature = "websocket")]
    fallible_request_modifier: Option<FallibleRequestModifierFn>,
    socket_connector: Option<SocketConnector>,
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
            clean_session: true,
            client_id: id.into(),
            credentials: None,
            max_incoming_packet_size: 10 * 1024,
            max_outgoing_packet_size: 10 * 1024,
            request_channel_capacity: 10,
            max_request_batch: 0,
            read_batch_size: 0,
            pending_throttle: Duration::from_micros(0),
            inflight: 100,
            last_will: None,
            manual_acks: false,
            #[cfg(feature = "proxy")]
            proxy: None,
            #[cfg(feature = "websocket")]
            request_modifier: None,
            #[cfg(feature = "websocket")]
            fallible_request_modifier: None,
            socket_connector: None,
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

    /// Set packet size limit for outgoing and incoming packets
    pub fn set_max_packet_size(&mut self, incoming: usize, outgoing: usize) -> &mut Self {
        self.max_incoming_packet_size = incoming;
        self.max_outgoing_packet_size = outgoing;
        self
    }

    /// Maximum packet size
    pub fn max_packet_size(&self) -> usize {
        self.max_incoming_packet_size
    }

    /// `clean_session = true` removes all the state from queues & instructs the broker
    /// to clean all the client state when client disconnects.
    ///
    /// When set `false`, broker will hold the client state and performs pending
    /// operations on the client when reconnection with same `client_id`
    /// happens. Local queue state is also held to retransmit packets after reconnection.
    ///
    /// # Panic
    ///
    /// Panics if `clean_session` is false when `client_id` is empty.
    ///
    /// ```should_panic
    /// # use rumqttc::MqttOptions;
    /// let mut options = MqttOptions::new("", "localhost", 1883);
    /// options.set_clean_session(false);
    /// ```
    pub fn set_clean_session(&mut self, clean_session: bool) -> &mut Self {
        assert!(
            !self.client_id.is_empty() || clean_session,
            "Cannot unset clean session when client id is empty"
        );
        self.clean_session = clean_session;
        self
    }

    /// Clean session
    pub fn clean_session(&self) -> bool {
        self.clean_session
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

    /// Set number of concurrent in flight messages
    pub fn set_inflight(&mut self, inflight: u16) -> &mut Self {
        assert!(inflight != 0, "zero in flight is not allowed");

        self.inflight = inflight;
        self
    }

    /// Number of concurrent in flight messages
    pub fn inflight(&self) -> u16 {
        self.inflight
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

    #[cfg(feature = "proxy")]
    pub fn set_proxy(&mut self, proxy: Proxy) -> &mut Self {
        self.proxy = Some(proxy);
        self
    }

    #[cfg(feature = "proxy")]
    pub fn proxy(&self) -> Option<Proxy> {
        self.proxy.clone()
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
    pub fn set_socket_connector<F, Fut, S>(&mut self, f: F) -> &mut Self
    where
        F: Fn(String, NetworkOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<S, std::io::Error>> + Send + 'static,
        S: crate::framed::AsyncReadWrite + 'static,
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

    #[error("Invalid clean-session value.")]
    CleanSession,

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
        options.set_transport(transport);

        if let Some(keep_alive) = queries
            .remove("keep_alive_secs")
            .map(|v| v.parse::<u16>().map_err(|_| OptionError::KeepAlive))
            .transpose()?
        {
            options.set_keep_alive(keep_alive);
        }

        if let Some(clean_session) = queries
            .remove("clean_session")
            .map(|v| v.parse::<bool>().map_err(|_| OptionError::CleanSession))
            .transpose()?
        {
            options.set_clean_session(clean_session);
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

        if let (Some(incoming), Some(outgoing)) = (
            queries
                .remove("max_incoming_packet_size_bytes")
                .map(|v| {
                    v.parse::<usize>()
                        .map_err(|_| OptionError::MaxIncomingPacketSize)
                })
                .transpose()?,
            queries
                .remove("max_outgoing_packet_size_bytes")
                .map(|v| {
                    v.parse::<usize>()
                        .map_err(|_| OptionError::MaxOutgoingPacketSize)
                })
                .transpose()?,
        ) {
            options.set_max_packet_size(incoming, outgoing);
        }

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

        if let Some(inflight) = queries
            .remove("inflight_num")
            .map(|v| v.parse::<u16>().map_err(|_| OptionError::Inflight))
            .transpose()?
        {
            options.set_inflight(inflight);
        }

        if let Some((opt, _)) = queries.into_iter().next() {
            return Err(OptionError::Unknown(opt.into_owned()));
        }

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
            .field("clean_session", &self.clean_session)
            .field("client_id", &self.client_id)
            .field("credentials", &self.credentials)
            .field("max_packet_size", &self.max_incoming_packet_size)
            .field("request_channel_capacity", &self.request_channel_capacity)
            .field("max_request_batch", &self.max_request_batch)
            .field("read_batch_size", &self.read_batch_size)
            .field("pending_throttle", &self.pending_throttle)
            .field("inflight", &self.inflight)
            .field("last_will", &self.last_will)
            .field("manual_acks", &self.manual_acks)
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
    #[cfg(all(feature = "use-rustls-no-provider", feature = "websocket"))]
    fn no_scheme() {
        let mut mqttoptions = MqttOptions::new(
            "client_a",
            "a3f8czas.iot.eu-west-1.amazonaws.com/mqtt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=MyCreds%2F20201001%2Feu-west-1%2Fiotdevicegateway%2Faws4_request&X-Amz-Date=20201001T130812Z&X-Amz-Expires=7200&X-Amz-Signature=9ae09b49896f44270f2707551581953e6cac71a4ccf34c7c3415555be751b2d1&X-Amz-SignedHeaders=host",
            443,
        );

        mqttoptions.set_transport(crate::Transport::wss(Vec::from("Test CA"), None, None));

        if let crate::Transport::Wss(TlsConfiguration::Simple {
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
            err("mqtt://host:42?client_id=foo&clean_session=foo"),
            OptionError::CleanSession
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&max_incoming_packet_size_bytes=foo"),
            OptionError::MaxIncomingPacketSize
        );
        assert_eq!(
            err("mqtt://host:42?client_id=foo&max_outgoing_packet_size_bytes=foo"),
            OptionError::MaxOutgoingPacketSize
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
    }

    #[test]
    fn accept_empty_client_id() {
        let _mqtt_opts = MqttOptions::new("", "127.0.0.1", 1883).set_clean_session(true);
    }

    #[test]
    fn set_clean_session_when_client_id_present() {
        let mut options = MqttOptions::new("client_id", "127.0.0.1", 1883);
        options.set_clean_session(false);
        options.set_clean_session(true);
    }

    #[test]
    fn read_batch_size_defaults_to_adaptive() {
        let options = MqttOptions::new("client_id", "127.0.0.1", 1883);
        assert_eq!(options.read_batch_size(), 0);
    }

    #[test]
    fn set_read_batch_size() {
        let mut options = MqttOptions::new("client_id", "127.0.0.1", 1883);
        options.set_read_batch_size(48);
        assert_eq!(options.read_batch_size(), 48);
    }
}
