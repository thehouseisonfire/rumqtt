use crate::notice::{PublishNoticeTx, RequestNoticeTx, TrackedNoticeTx};
use crate::{Incoming, MqttState, NetworkOptions, Packet, Request, StateError};
use crate::{MqttOptions, Outgoing};
use crate::{NoticeFailureReason, PublishNoticeError};
use crate::{Transport, framed::Network};

use crate::framed::AsyncReadWrite;
use crate::mqttbytes::v4::{
    ConnAck, Connect, ConnectReturnCode, PingReq, Publish, Subscribe, Unsubscribe,
};
use flume::{Receiver, Sender, TryRecvError, bounded};
use tokio::select;
use tokio::time::{self, Instant, Sleep};

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::time::Duration;

#[cfg(unix)]
use {std::path::Path, tokio::net::UnixStream};

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
use crate::tls;

#[cfg(feature = "websocket")]
use {
    crate::websockets::WsAdapter,
    crate::websockets::{UrlError, split_url, validate_response_headers},
    async_tungstenite::tungstenite::client::IntoClientRequest,
};

#[cfg(feature = "proxy")]
use crate::proxy::ProxyError;

#[derive(Debug)]
pub struct RequestEnvelope {
    request: Request,
    notice: Option<TrackedNoticeTx>,
}

impl RequestEnvelope {
    pub(crate) const fn from_parts(request: Request, notice: Option<TrackedNoticeTx>) -> Self {
        Self { request, notice }
    }

    pub(crate) const fn plain(request: Request) -> Self {
        Self {
            request,
            notice: None,
        }
    }

    pub(crate) const fn tracked_publish(publish: Publish, notice: PublishNoticeTx) -> Self {
        Self {
            request: Request::Publish(publish),
            notice: Some(TrackedNoticeTx::Publish(notice)),
        }
    }

    pub(crate) const fn tracked_subscribe(subscribe: Subscribe, notice: RequestNoticeTx) -> Self {
        Self {
            request: Request::Subscribe(subscribe),
            notice: Some(TrackedNoticeTx::Request(notice)),
        }
    }

    pub(crate) const fn tracked_unsubscribe(
        unsubscribe: Unsubscribe,
        notice: RequestNoticeTx,
    ) -> Self {
        Self {
            request: Request::Unsubscribe(unsubscribe),
            notice: Some(TrackedNoticeTx::Request(notice)),
        }
    }

    pub(crate) fn into_parts(self) -> (Request, Option<TrackedNoticeTx>) {
        (self.request, self.notice)
    }
}

/// Critical errors during eventloop polling
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("Mqtt state: {0}")]
    MqttState(#[from] StateError),
    #[error("Network timeout")]
    NetworkTimeout,
    #[error("Flush timeout")]
    FlushTimeout,
    #[cfg(feature = "websocket")]
    #[error("Websocket: {0}")]
    Websocket(#[from] async_tungstenite::tungstenite::error::Error),
    #[cfg(feature = "websocket")]
    #[error("Websocket Connect: {0}")]
    WsConnect(#[from] http::Error),
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[error("TLS: {0}")]
    Tls(#[from] tls::Error),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("Connection refused, return code: `{0:?}`")]
    ConnectionRefused(ConnectReturnCode),
    #[error("Expected ConnAck packet, received: {0:?}")]
    NotConnAck(Box<Packet>),
    #[error(
        "Broker replied with session_present={session_present} for clean_session={clean_session}"
    )]
    SessionStateMismatch {
        clean_session: bool,
        session_present: bool,
    },
    #[error("Broker target is incompatible with the selected transport")]
    BrokerTransportMismatch,
    /// All request senders have been dropped. Use `AsyncClient::disconnect` for MQTT-level
    /// graceful shutdown with a DISCONNECT packet.
    #[error("Requests done")]
    RequestsDone,
    #[cfg(feature = "websocket")]
    #[error("Invalid Url: {0}")]
    InvalidUrl(#[from] UrlError),
    #[cfg(feature = "proxy")]
    #[error("Proxy Connect: {0}")]
    Proxy(#[from] ProxyError),
    #[cfg(feature = "websocket")]
    #[error("Websocket response validation error: ")]
    ResponseValidation(#[from] crate::websockets::ValidationError),
    #[cfg(feature = "websocket")]
    #[error("Websocket request modifier failed: {0}")]
    RequestModifier(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Eventloop with all the state of a connection
pub struct EventLoop {
    /// Options of the current mqtt connection
    pub mqtt_options: MqttOptions,
    /// Current state of the connection
    pub state: MqttState,
    /// Request stream
    requests_rx: Receiver<RequestEnvelope>,
    /// Internal request sender retained for compatibility in `EventLoop::new`.
    /// This is intentionally dropped in the `AsyncClient::new` constructor path.
    _requests_tx: Option<Sender<RequestEnvelope>>,
    /// Pending packets from last session
    pending: VecDeque<RequestEnvelope>,
    /// Network connection to the broker
    pub network: Option<Network>,
    /// Keep alive time
    keepalive_timeout: Option<Pin<Box<Sleep>>>,
    pub network_options: NetworkOptions,
}

/// Events which can be yielded by the event loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Incoming(Incoming),
    Outgoing(Outgoing),
}

impl EventLoop {
    fn reconcile_connack_session(&mut self, session_present: bool) -> Result<(), ConnectionError> {
        let clean_session = self.mqtt_options.clean_session();
        if clean_session && session_present {
            return Err(ConnectionError::SessionStateMismatch {
                clean_session,
                session_present,
            });
        }

        if !session_present {
            self.reset_session_state();
        }

        Ok(())
    }

    /// New MQTT `EventLoop`
    ///
    /// When connection encounters critical errors (like auth failure), user has a choice to
    /// access and update `options`, `state` and `requests`.
    pub fn new(mqtt_options: MqttOptions, cap: usize) -> Self {
        let (requests_tx, requests_rx) = bounded(cap);
        Self::with_channel(mqtt_options, requests_rx, Some(requests_tx))
    }

    /// Internal constructor used by `AsyncClient::new`.
    ///
    /// Unlike `EventLoop::new`, this does not keep an internal sender handle, so dropping all
    /// `AsyncClient` handles can terminate polling with `ConnectionError::RequestsDone`.
    pub(crate) fn new_for_async_client(
        mqtt_options: MqttOptions,
        cap: usize,
    ) -> (Self, Sender<RequestEnvelope>) {
        let (requests_tx, requests_rx) = bounded(cap);
        let eventloop = Self::with_channel(mqtt_options, requests_rx, None);
        (eventloop, requests_tx)
    }

    fn with_channel(
        mqtt_options: MqttOptions,
        requests_rx: Receiver<RequestEnvelope>,
        requests_tx: Option<Sender<RequestEnvelope>>,
    ) -> Self {
        let pending = VecDeque::new();
        let max_inflight = mqtt_options.inflight;
        let manual_acks = mqtt_options.manual_acks;

        Self {
            mqtt_options,
            state: MqttState::new(max_inflight, manual_acks),
            requests_rx,
            _requests_tx: requests_tx,
            pending,
            network: None,
            keepalive_timeout: None,
            network_options: NetworkOptions::new(),
        }
    }

    /// Last session might contain packets which aren't acked. MQTT says these packets should be
    /// republished in the next session. Move pending messages from state to eventloop, drops the
    /// underlying network connection and clears the keepalive timeout if any.
    ///
    /// > NOTE: Use only when EventLoop is blocked on network and unable to immediately handle disconnect.
    /// > Pending requests are managed internally by the event loop.
    /// > Use [`pending_len`](Self::pending_len) or [`pending_is_empty`](Self::pending_is_empty)
    /// > for observation-only checks.
    pub fn clean(&mut self) {
        self.network = None;
        self.keepalive_timeout = None;
        for (request, notice) in self.state.clean_with_notices() {
            self.pending
                .push_back(RequestEnvelope::from_parts(request, notice));
        }

        // drain requests from channel which weren't yet received
        for envelope in self.requests_rx.drain() {
            // Wait for publish retransmission, else the broker could be confused by an unexpected
            // inbound acknowledgment replayed from a previous connection.
            if !matches!(&envelope.request, Request::PubAck(_) | Request::PubRec(_)) {
                self.pending.push_back(envelope);
            }
        }
    }

    /// Number of pending requests queued for retransmission.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns true when there are no pending requests queued for retransmission.
    pub fn pending_is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Drains pending retransmission queue and fails tracked notices with the given reason.
    ///
    /// Returns the number of pending requests removed from the queue.
    pub fn drain_pending_as_failed(&mut self, reason: NoticeFailureReason) -> usize {
        let mut drained = 0;
        for envelope in self.pending.drain(..) {
            drained += 1;
            if let Some(notice) = envelope.notice {
                match notice {
                    TrackedNoticeTx::Publish(notice) => {
                        notice.error(reason.publish_error());
                    }
                    TrackedNoticeTx::Request(notice) => {
                        notice.error(reason.request_error());
                    }
                }
            }
        }

        drained
    }

    /// Clears eventloop and state tracking bound to a previous session.
    pub fn reset_session_state(&mut self) {
        self.drain_pending_as_failed(NoticeFailureReason::SessionReset);
        self.state.fail_pending_notices();
    }

    /// Yields Next notification or outgoing request and periodically pings
    /// the broker. Continuing to poll will reconnect to the broker if there is
    /// a disconnection.
    /// **NOTE** Don't block this while iterating
    ///
    /// # Errors
    ///
    /// Returns a [`ConnectionError`] if connecting, reading, writing, or
    /// protocol handling fails.
    pub async fn poll(&mut self) -> Result<Event, ConnectionError> {
        if self.network.is_none() {
            let (network, connack) = match time::timeout(
                Duration::from_secs(self.network_options.connection_timeout()),
                connect(&self.mqtt_options, self.network_options.clone()),
            )
            .await
            {
                Ok(inner) => inner?,
                Err(_) => return Err(ConnectionError::NetworkTimeout),
            };
            self.reconcile_connack_session(connack.session_present)?;
            self.network = Some(network);

            if self.keepalive_timeout.is_none() && !self.mqtt_options.keep_alive.is_zero() {
                self.keepalive_timeout = Some(Box::pin(time::sleep(self.mqtt_options.keep_alive)));
            }

            return Ok(Event::Incoming(Packet::ConnAck(connack)));
        }

        match self.select().await {
            Ok(v) => Ok(v),
            Err(e) => {
                // MQTT requires that packets pending acknowledgement should be republished on session resume.
                // Move pending messages from state to eventloop.
                self.clean();
                Err(e)
            }
        }
    }

    /// Select on network and requests and generate keepalive pings when necessary
    #[allow(clippy::too_many_lines)]
    async fn select(&mut self) -> Result<Event, ConnectionError> {
        let read_batch_size = self.effective_read_batch_size();
        let network = self.network.as_mut().unwrap();
        // let await_acks = self.state.await_acks;
        let inflight_full = self.state.inflight >= self.mqtt_options.inflight;
        let collision = self.state.collision.is_some();
        let network_timeout = Duration::from_secs(self.network_options.connection_timeout());

        // Read buffered events from previous polls before calling a new poll
        if let Some(event) = self.state.events.pop_front() {
            return Ok(event);
        }

        let mut no_sleep = Box::pin(time::sleep(Duration::ZERO));
        // this loop is necessary since self.incoming.pop_front() might return None. In that case,
        // instead of returning a None event, we try again.
        select! {
            // Handles pending and new requests.
            // If available, prioritises pending requests from previous session.
            // Else, pulls next request from user requests channel.
            // If conditions in the below branch are for flow control.
            // The branch is disabled if there's no pending messages and new user requests
            // cannot be serviced due flow control.
            // We read next user user request only when inflight messages are < configured inflight
            // and there are no collisions while handling previous outgoing requests.
            //
            // Flow control is based on ack count. If inflight packet count in the buffer is
            // less than max_inflight setting, next outgoing request will progress. For this
            // to work correctly, broker should ack in sequence (a lot of brokers won't)
            //
            // E.g If max inflight = 5, user requests will be blocked when inflight queue
            // looks like this                 -> [1, 2, 3, 4, 5].
            // If broker acking 2 instead of 1 -> [1, x, 3, 4, 5].
            // This pulls next user request. But because max packet id = max_inflight, next
            // user request's packet id will roll to 1. This replaces existing packet id 1.
            // Resulting in a collision
            //
            // Eventloop can stop receiving outgoing user requests when previous outgoing
            // request collided. I.e collision state. Collision state will be cleared only
            // when correct ack is received
            // Full inflight queue will look like -> [1a, 2, 3, 4, 5].
            // If 3 is acked instead of 1 first   -> [1a, 2, x, 4, 5].
            // After collision with pkid 1        -> [1b ,2, x, 4, 5].
            // 1a is saved to state and event loop is set to collision mode stopping new
            // outgoing requests (along with 1b).
            o = Self::next_request(
                &mut self.pending,
                &self.requests_rx,
                self.mqtt_options.pending_throttle
            ), if !self.pending.is_empty() || (!inflight_full && !collision) => match o {
                Ok((request, notice)) => {
                    let max_request_batch = self.mqtt_options.max_request_batch.max(1);
                    let mut should_flush = false;
                    let mut qos0_notices = Vec::new();

                    let (outgoing, flush_notice) =
                        self.state.handle_outgoing_packet_with_notice(request, notice)?;
                    if let Some(notice) = flush_notice {
                        qos0_notices.push(notice);
                    }
                    if let Some(outgoing) = outgoing {
                        if let Err(err) = network.write(outgoing).await {
                            for notice in qos0_notices {
                                notice.error(PublishNoticeError::Qos0NotFlushed);
                            }
                            return Err(ConnectionError::MqttState(err));
                        }
                        should_flush = true;
                    }

                    for _ in 1..max_request_batch {
                        let inflight_full = self.state.inflight >= self.mqtt_options.inflight;
                        let collision = self.state.collision.is_some();

                        if self.pending.is_empty() && (inflight_full || collision) {
                            break;
                        }

                        let Some((next_request, next_notice)) = Self::try_next_request(
                            &mut self.pending,
                            &self.requests_rx,
                            self.mqtt_options.pending_throttle,
                        ).await else {
                            break;
                        };

                        let (outgoing, flush_notice) =
                            self.state.handle_outgoing_packet_with_notice(next_request, next_notice)?;
                        if let Some(notice) = flush_notice {
                            qos0_notices.push(notice);
                        }
                        if let Some(outgoing) = outgoing {
                            if let Err(err) = network.write(outgoing).await {
                                for notice in qos0_notices {
                                    notice.error(PublishNoticeError::Qos0NotFlushed);
                                }
                                return Err(ConnectionError::MqttState(err));
                            }
                            should_flush = true;
                        }
                    }

                    if should_flush {
                        match time::timeout(network_timeout, network.flush()).await {
                            Ok(Ok(())) => {
                                for notice in qos0_notices {
                                    notice.success();
                                }
                            }
                            Ok(Err(err)) => {
                                for notice in qos0_notices {
                                    notice.error(PublishNoticeError::Qos0NotFlushed);
                                }
                                return Err(ConnectionError::MqttState(err));
                            }
                            Err(_) => {
                                for notice in qos0_notices {
                                    notice.error(PublishNoticeError::Qos0NotFlushed);
                                }
                                return Err(ConnectionError::FlushTimeout);
                            }
                        }
                    }
                    Ok(self.state.events.pop_front().unwrap())
                }
                Err(_) => Err(ConnectionError::RequestsDone),
            },
            // Pull a bunch of packets from network, reply in bunch and yield the first item
            o = network.readb(&mut self.state, read_batch_size) => {
                o?;
                // flush all the acks and return first incoming packet
                match time::timeout(network_timeout, network.flush()).await {
                    Ok(inner) => inner?,
                    Err(_)=> return Err(ConnectionError::FlushTimeout),
                }
                Ok(self.state.events.pop_front().unwrap())
            },
            // We generate pings irrespective of network activity. This keeps the ping logic
            // simple. We can change this behavior in future if necessary (to prevent extra pings)
            () = self.keepalive_timeout.as_mut().unwrap_or(&mut no_sleep),
                if self.keepalive_timeout.is_some() && !self.mqtt_options.keep_alive.is_zero() => {
                let timeout = self.keepalive_timeout.as_mut().unwrap();
                timeout.as_mut().reset(Instant::now() + self.mqtt_options.keep_alive);

                let (outgoing, _flush_notice) = self
                    .state
                    .handle_outgoing_packet_with_notice(Request::PingReq(PingReq), None)?;
                if let Some(outgoing) = outgoing {
                    network.write(outgoing).await?;
                }
                match time::timeout(network_timeout, network.flush()).await {
                    Ok(inner) => inner?,
                    Err(_)=> return Err(ConnectionError::FlushTimeout),
                }
                Ok(self.state.events.pop_front().unwrap())
            }
        }
    }

    pub fn network_options(&self) -> NetworkOptions {
        self.network_options.clone()
    }

    pub fn set_network_options(&mut self, network_options: NetworkOptions) -> &mut Self {
        self.network_options = network_options;
        self
    }

    fn effective_read_batch_size(&self) -> usize {
        const MAX_READ_BATCH_SIZE: usize = 128;
        const PENDING_FAIRNESS_CAP: usize = 16;

        let configured = self.mqtt_options.read_batch_size();
        if configured > 0 {
            return configured.clamp(1, MAX_READ_BATCH_SIZE);
        }

        let request_batch = self.mqtt_options.max_request_batch().max(1);
        let inflight = usize::from(self.mqtt_options.inflight());
        let mut adaptive = request_batch.max(inflight / 2).max(8);

        if !self.pending.is_empty() || !self.requests_rx.is_empty() {
            adaptive = adaptive.min(PENDING_FAIRNESS_CAP);
        }

        adaptive.clamp(1, MAX_READ_BATCH_SIZE)
    }

    async fn try_next_request(
        pending: &mut VecDeque<RequestEnvelope>,
        rx: &Receiver<RequestEnvelope>,
        pending_throttle: Duration,
    ) -> Option<(Request, Option<TrackedNoticeTx>)> {
        if !pending.is_empty() {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .pop_front() AFTER sleep() otherwise we would have
            // advanced the iterator but the future might be canceled before return
            return pending.pop_front().map(RequestEnvelope::into_parts);
        }

        match rx.try_recv() {
            Ok(envelope) => return Some(envelope.into_parts()),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }

        None
    }

    async fn next_request(
        pending: &mut VecDeque<RequestEnvelope>,
        rx: &Receiver<RequestEnvelope>,
        pending_throttle: Duration,
    ) -> Result<(Request, Option<TrackedNoticeTx>), ConnectionError> {
        if pending.is_empty() {
            rx.recv_async()
                .await
                .map(RequestEnvelope::into_parts)
                .map_err(|_| ConnectionError::RequestsDone)
        } else {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .pop_front() AFTER sleep() otherwise we would have
            // advanced the iterator but the future might be canceled before return
            Ok(pending.pop_front().unwrap().into_parts())
        }
    }
}

/// This stream internally processes requests from the request stream provided to the eventloop
/// while also consuming byte stream from the network and yielding mqtt packets as the output of
/// the stream.
/// This function (for convenience) includes internal delays for users to perform internal sleeps
/// between re-connections so that cancel semantics can be used during this sleep
async fn connect(
    mqtt_options: &MqttOptions,
    network_options: NetworkOptions,
) -> Result<(Network, ConnAck), ConnectionError> {
    // connect to the broker
    let mut network = network_connect(mqtt_options, network_options).await?;

    // make MQTT connection request (which internally awaits for ack)
    let connack = mqtt_connect(mqtt_options, &mut network).await?;

    Ok((network, connack))
}

#[allow(clippy::too_many_lines)]
async fn network_connect(
    options: &MqttOptions,
    network_options: NetworkOptions,
) -> Result<Network, ConnectionError> {
    let transport = options.transport();

    // Process Unix files early, as proxy is not supported for them.
    #[cfg(unix)]
    if matches!(&transport, Transport::Unix) {
        let file = options
            .broker()
            .unix_path()
            .ok_or(ConnectionError::BrokerTransportMismatch)?;
        let socket = UnixStream::connect(Path::new(file)).await?;
        let network = Network::new(
            socket,
            options.max_incoming_packet_size,
            options.max_outgoing_packet_size,
        );
        return Ok(network);
    }

    // For websockets domain and port are taken directly from the broker URL.
    let (domain, port) = match &transport {
        #[cfg(feature = "websocket")]
        Transport::Ws => split_url(
            options
                .broker()
                .websocket_url()
                .ok_or(ConnectionError::BrokerTransportMismatch)?,
        )?,
        #[cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        ))]
        Transport::Wss(_) => split_url(
            options
                .broker()
                .websocket_url()
                .ok_or(ConnectionError::BrokerTransportMismatch)?,
        )?,
        _ => options
            .broker()
            .tcp_address()
            .map(|(host, port)| (host.to_owned(), port))
            .ok_or(ConnectionError::BrokerTransportMismatch)?,
    };

    let tcp_stream: Box<dyn AsyncReadWrite> = {
        #[cfg(feature = "proxy")]
        match options.proxy() {
            Some(proxy) => {
                proxy
                    .connect(
                        &domain,
                        port,
                        network_options,
                        Some(options.effective_socket_connector()),
                    )
                    .await?
            }
            None => {
                let addr = format!("{domain}:{port}");
                options.socket_connect(addr, network_options).await?
            }
        }
        #[cfg(not(feature = "proxy"))]
        {
            let addr = format!("{domain}:{port}");
            options.socket_connect(addr, network_options).await?
        }
    };

    let network = match transport {
        Transport::Tcp => Network::new(
            tcp_stream,
            options.max_incoming_packet_size,
            options.max_outgoing_packet_size,
        ),
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        Transport::Tls(tls_config) => {
            let (host, port) = options
                .broker()
                .tcp_address()
                .expect("tls transport requires a tcp broker");
            let socket = tls::tls_connect(host, port, &tls_config, tcp_stream).await?;
            Network::new(
                socket,
                options.max_incoming_packet_size,
                options.max_outgoing_packet_size,
            )
        }
        #[cfg(unix)]
        Transport::Unix => unreachable!(),
        #[cfg(feature = "websocket")]
        Transport::Ws => {
            let mut request = options
                .broker()
                .websocket_url()
                .expect("ws transport requires a websocket broker")
                .into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());

            if let Some(request_modifier) = options.fallible_request_modifier() {
                request = request_modifier(request)
                    .await
                    .map_err(ConnectionError::RequestModifier)?;
            } else if let Some(request_modifier) = options.request_modifier() {
                request = request_modifier(request).await;
            }

            let (socket, response) =
                async_tungstenite::tokio::client_async(request, tcp_stream).await?;
            validate_response_headers(response)?;

            Network::new(
                WsAdapter::new(socket),
                options.max_incoming_packet_size,
                options.max_outgoing_packet_size,
            )
        }
        #[cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        ))]
        Transport::Wss(tls_config) => {
            let mut request = options
                .broker()
                .websocket_url()
                .expect("wss transport requires a websocket broker")
                .into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());

            if let Some(request_modifier) = options.fallible_request_modifier() {
                request = request_modifier(request)
                    .await
                    .map_err(ConnectionError::RequestModifier)?;
            } else if let Some(request_modifier) = options.request_modifier() {
                request = request_modifier(request).await;
            }

            let tls_stream = tls::tls_connect(&domain, port, &tls_config, tcp_stream).await?;
            let (socket, response) =
                async_tungstenite::tokio::client_async(request, tls_stream).await?;
            validate_response_headers(response)?;

            Network::new(
                WsAdapter::new(socket),
                options.max_incoming_packet_size,
                options.max_outgoing_packet_size,
            )
        }
    };

    Ok(network)
}

async fn mqtt_connect(
    options: &MqttOptions,
    network: &mut Network,
) -> Result<ConnAck, ConnectionError> {
    let mut connect = Connect::new(options.client_id());
    connect.keep_alive = u16::try_from(options.keep_alive().as_secs()).unwrap_or(u16::MAX);
    connect.clean_session = options.clean_session();
    connect.last_will = options.last_will();
    connect.auth = options.auth().clone();

    // send mqtt connect packet
    network.write(Packet::Connect(connect)).await?;
    network.flush().await?;

    // validate connack
    match network.read().await? {
        Incoming::ConnAck(connack) if connack.code == ConnectReturnCode::Success => Ok(connack),
        Incoming::ConnAck(connack) => Err(ConnectionError::ConnectionRefused(connack.code)),
        packet => Err(ConnectionError::NotConnAck(Box::new(packet))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Disconnect, PubAck, PubRec};
    use flume::TryRecvError;

    fn push_pending(eventloop: &mut EventLoop, request: Request) {
        eventloop.pending.push_back(RequestEnvelope::plain(request));
    }

    fn pending_front_request(eventloop: &EventLoop) -> Option<&Request> {
        eventloop.pending.front().map(|envelope| &envelope.request)
    }

    fn build_eventloop_with_pending(clean_session: bool) -> EventLoop {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_session(clean_session);

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        push_pending(&mut eventloop, Request::PingReq(PingReq));
        eventloop
    }

    #[test]
    fn eventloop_new_keeps_internal_sender_alive() {
        let options = MqttOptions::new("test-client", "localhost");
        let eventloop = EventLoop::new(options, 1);

        assert!(matches!(
            eventloop.requests_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn async_client_constructor_path_allows_channel_shutdown() {
        let options = MqttOptions::new("test-client", "localhost");
        let (eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        drop(request_tx);

        assert!(matches!(
            eventloop.requests_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn clean_drops_ack_requests_drained_from_channel() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 3);
        request_tx
            .send(RequestEnvelope::plain(Request::PubAck(PubAck::new(7))))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::PubRec(PubRec::new(8))))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .unwrap();

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq(_))
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn network_connect_rejects_unix_broker_with_tcp_transport() {
        let mut options = MqttOptions::new("test-client", crate::Broker::unix("/tmp/mqtt.sock"));
        options.set_transport(Transport::tcp());

        match network_connect(&options, NetworkOptions::new()).await {
            Err(ConnectionError::BrokerTransportMismatch) => {}
            Err(err) => panic!("unexpected error: {err:?}"),
            Ok(_) => panic!("mismatched broker and transport should fail"),
        }
    }

    #[tokio::test]
    #[cfg(feature = "websocket")]
    async fn network_connect_rejects_tcp_broker_with_websocket_transport() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_transport(Transport::Ws);

        match network_connect(&options, NetworkOptions::new()).await {
            Err(ConnectionError::BrokerTransportMismatch) => {}
            Err(err) => panic!("unexpected error: {err:?}"),
            Ok(_) => panic!("mismatched broker and transport should fail"),
        }
    }

    #[tokio::test]
    #[cfg(feature = "websocket")]
    async fn network_connect_rejects_websocket_broker_with_tcp_transport() {
        let broker = crate::Broker::websocket("ws://localhost:9001/mqtt").unwrap();
        let mut options = MqttOptions::new("test-client", broker);
        options.set_transport(Transport::tcp());

        match network_connect(&options, NetworkOptions::new()).await {
            Err(ConnectionError::BrokerTransportMismatch) => {}
            Err(err) => panic!("unexpected error: {err:?}"),
            Ok(_) => panic!("mismatched broker and transport should fail"),
        }
    }

    #[tokio::test]
    async fn async_client_path_reports_requests_done_after_pending_drain() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        push_pending(&mut eventloop, Request::PingReq(PingReq));
        drop(request_tx);

        let request = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(request, (Request::PingReq(_), None)));

        let err = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ConnectionError::RequestsDone));
    }

    #[tokio::test]
    async fn next_request_is_cancellation_safe_for_pending_queue() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        push_pending(&mut eventloop, Request::PingReq(PingReq));

        let delayed = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::from_millis(50),
        );
        let timed_out = time::timeout(Duration::from_millis(5), delayed).await;

        assert!(timed_out.is_err());
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq(_))
        ));
    }

    #[tokio::test]
    async fn try_next_request_applies_pending_throttle_for_followup_pending_item() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 2);
        push_pending(&mut eventloop, Request::PingReq(PingReq));
        push_pending(&mut eventloop, Request::Disconnect(Disconnect));

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq(_), None)));

        let delayed = EventLoop::try_next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::from_millis(50),
        );
        let timed_out = time::timeout(Duration::from_millis(5), delayed).await;

        assert!(timed_out.is_err());
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Disconnect(_))
        ));
    }

    #[tokio::test]
    async fn try_next_request_does_not_throttle_when_pending_queue_is_empty() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .await
            .unwrap();

        let received = time::timeout(
            Duration::from_millis(20),
            EventLoop::try_next_request(
                &mut eventloop.pending,
                &eventloop.requests_rx,
                Duration::from_secs(1),
            ),
        )
        .await
        .unwrap();

        assert!(matches!(received, Some((Request::PingReq(_), None))));
    }

    #[tokio::test]
    async fn next_request_prioritizes_pending_over_channel_messages() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);
        push_pending(&mut eventloop, Request::PingReq(PingReq));
        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .await
            .unwrap();

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq(_), None)));
        assert!(eventloop.pending.is_empty());

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(second, (Request::PingReq(_), None)));
    }

    #[tokio::test]
    async fn next_request_preserves_fifo_order_for_plain_and_tracked_requests() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (notice_tx, _notice) = PublishNoticeTx::new();
        let tracked_publish =
            Publish::new("hello/world", crate::mqttbytes::QoS::AtLeastOnce, "payload");

        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .await
            .unwrap();
        request_tx
            .send_async(RequestEnvelope::tracked_publish(
                tracked_publish.clone(),
                notice_tx,
            ))
            .await
            .unwrap();
        request_tx
            .send_async(RequestEnvelope::plain(Request::Disconnect(Disconnect)))
            .await
            .unwrap();

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq(_), None)));

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(
            second,
            (Request::Publish(publish), Some(_)) if publish == tracked_publish
        ));

        let third = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(third, (Request::Disconnect(_), None)));
    }

    #[tokio::test]
    async fn tracked_qos0_notice_reports_not_flushed_on_first_write_failure() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 16));

        let (notice_tx, notice) = PublishNoticeTx::new();
        let publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtMostOnce,
            vec![1; 128],
        );
        request_tx
            .send_async(RequestEnvelope::tracked_publish(publish, notice_tx))
            .await
            .unwrap();

        let err = eventloop.select().await.unwrap_err();
        assert!(matches!(err, ConnectionError::MqttState(_)));
        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            PublishNoticeError::Qos0NotFlushed
        );
    }

    #[tokio::test]
    async fn tracked_qos0_notices_report_not_flushed_on_batched_write_failure() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_max_request_batch(2);
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 80));

        let small_publish = Publish::new("hello/world", crate::mqttbytes::QoS::AtMostOnce, vec![1]);
        let large_publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtMostOnce,
            vec![2; 256],
        );

        let (first_notice_tx, first_notice) = PublishNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_publish(
                small_publish,
                first_notice_tx,
            ))
            .await
            .unwrap();

        let (second_notice_tx, second_notice) = PublishNoticeTx::new();
        request_tx
            .send_async(RequestEnvelope::tracked_publish(
                large_publish,
                second_notice_tx,
            ))
            .await
            .unwrap();

        let err = eventloop.select().await.unwrap_err();
        assert!(matches!(err, ConnectionError::MqttState(_)));
        assert_eq!(
            first_notice.wait_async().await.unwrap_err(),
            PublishNoticeError::Qos0NotFlushed
        );
        assert_eq!(
            second_notice.wait_async().await.unwrap_err(),
            PublishNoticeError::Qos0NotFlushed
        );
    }

    #[tokio::test]
    async fn drain_pending_as_failed_drains_all_and_returns_count() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (notice_tx, notice) = PublishNoticeTx::new();
        let publish = Publish::new("hello/world", crate::mqttbytes::QoS::AtLeastOnce, "payload");
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_publish(publish, notice_tx));
        eventloop
            .pending
            .push_back(RequestEnvelope::plain(Request::PingReq(PingReq)));

        let drained = eventloop.drain_pending_as_failed(NoticeFailureReason::SessionReset);

        assert_eq!(drained, 2);
        assert!(eventloop.pending.is_empty());
        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            PublishNoticeError::SessionReset
        );
    }

    #[tokio::test]
    async fn drain_pending_as_failed_reports_session_reset_for_tracked_notices() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (publish_notice_tx, publish_notice) = PublishNoticeTx::new();
        let publish = Publish::new("hello/world", crate::mqttbytes::QoS::AtLeastOnce, "payload");
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_publish(publish, publish_notice_tx));

        let (request_notice_tx, request_notice) = RequestNoticeTx::new();
        let subscribe = Subscribe::new("hello/world", crate::mqttbytes::QoS::AtMostOnce);
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_subscribe(
                subscribe,
                request_notice_tx,
            ));

        eventloop.drain_pending_as_failed(NoticeFailureReason::SessionReset);

        assert_eq!(
            publish_notice.wait_async().await.unwrap_err(),
            PublishNoticeError::SessionReset
        );
        assert_eq!(
            request_notice.wait_async().await.unwrap_err(),
            crate::RequestNoticeError::SessionReset
        );
    }

    #[tokio::test]
    async fn reset_session_state_reports_session_reset_for_pending_tracked_notice() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (notice_tx, notice) = PublishNoticeTx::new();
        let publish = Publish::new("hello/world", crate::mqttbytes::QoS::AtLeastOnce, "payload");
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_publish(publish, notice_tx));

        eventloop.reset_session_state();

        assert!(eventloop.pending.is_empty());
        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            PublishNoticeError::SessionReset
        );
    }

    #[test]
    fn connack_reconcile_rejects_clean_session_with_session_present() {
        let mut eventloop = build_eventloop_with_pending(true);

        let err = eventloop.reconcile_connack_session(true).unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::SessionStateMismatch {
                clean_session: true,
                session_present: true
            }
        ));
        assert_eq!(eventloop.pending_len(), 1);
    }

    #[test]
    fn connack_reconcile_resets_pending_when_clean_session_gets_new_session() {
        let mut eventloop = build_eventloop_with_pending(true);

        eventloop.reconcile_connack_session(false).unwrap();

        assert!(eventloop.pending_is_empty());
    }

    #[test]
    fn connack_reconcile_resets_pending_when_resumed_session_is_missing() {
        let mut eventloop = build_eventloop_with_pending(false);

        eventloop.reconcile_connack_session(false).unwrap();

        assert!(eventloop.pending_is_empty());
    }

    #[test]
    fn connack_reconcile_keeps_pending_when_resumed_session_exists() {
        let mut eventloop = build_eventloop_with_pending(false);

        eventloop.reconcile_connack_session(true).unwrap();

        assert_eq!(eventloop.pending_len(), 1);
    }
}
