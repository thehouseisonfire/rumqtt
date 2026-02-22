use super::framed::Network;
use super::mqttbytes::v5::*;
use super::{Incoming, MqttOptions, MqttState, Outgoing, Request, StateError, Transport};
use crate::eventloop::socket_connect;
use crate::framed::AsyncReadWrite;

use flume::{Receiver, Sender, TryRecvError, bounded};
use tokio::select;
use tokio::time::{self, Instant, Sleep, error::Elapsed};

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use super::mqttbytes::v5::ConnectReturnCode;

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
use crate::tls;

#[cfg(unix)]
use {std::path::Path, tokio::net::UnixStream};

#[cfg(feature = "websocket")]
use {
    crate::websockets::WsAdapter,
    crate::websockets::{UrlError, split_url, validate_response_headers},
    async_tungstenite::tungstenite::client::IntoClientRequest,
};

#[cfg(feature = "proxy")]
use crate::proxy::ProxyError;

/// Critical errors during eventloop polling
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("Mqtt state: {0}")]
    MqttState(#[from] StateError),
    #[error("Timeout")]
    Timeout(#[from] Elapsed),
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
    /// All request senders have been dropped. Use `AsyncClient::disconnect` for MQTT-level
    /// graceful shutdown with a DISCONNECT packet.
    #[error("Requests done")]
    RequestsDone,
    #[error("Auth processing error")]
    AuthProcessingError,
    #[cfg(feature = "websocket")]
    #[error("Invalid Url: {0}")]
    InvalidUrl(#[from] UrlError),
    #[cfg(feature = "proxy")]
    #[error("Proxy Connect: {0}")]
    Proxy(#[from] ProxyError),
    #[cfg(feature = "websocket")]
    #[error("Websocket response validation error: ")]
    ResponseValidation(#[from] crate::websockets::ValidationError),
}

/// Eventloop with all the state of a connection
pub struct EventLoop {
    /// Options of the current mqtt connection
    pub options: MqttOptions,
    /// Current state of the connection
    pub state: MqttState,
    /// Request stream
    requests_rx: Receiver<Request>,
    /// Internal request sender retained for compatibility in `EventLoop::new`.
    /// This is intentionally dropped in the `AsyncClient::new` constructor path.
    _requests_tx: Option<Sender<Request>>,
    /// Pending packets from last session
    pub pending: VecDeque<Request>,
    /// Network connection to the broker
    network: Option<Network>,
    /// Keep alive time
    keepalive_timeout: Option<Pin<Box<Sleep>>>,
}

/// Events which can be yielded by the event loop
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Incoming(Incoming),
    Outgoing(Outgoing),
}

impl EventLoop {
    /// New MQTT `EventLoop`
    ///
    /// When connection encounters critical errors (like auth failure), user has a choice to
    /// access and update `options`, `state` and `requests`.
    pub fn new(options: MqttOptions, cap: usize) -> EventLoop {
        let (requests_tx, requests_rx) = bounded(cap);
        Self::with_channel(options, requests_rx, Some(requests_tx))
    }

    /// Internal constructor used by `AsyncClient::new`.
    ///
    /// Unlike `EventLoop::new`, this does not keep an internal sender handle, so dropping all
    /// `AsyncClient` handles can terminate polling with `ConnectionError::RequestsDone`.
    pub(crate) fn new_for_async_client(
        options: MqttOptions,
        cap: usize,
    ) -> (EventLoop, Sender<Request>) {
        let (requests_tx, requests_rx) = bounded(cap);
        let eventloop = Self::with_channel(options, requests_rx, None);

        (eventloop, requests_tx)
    }

    fn with_channel(
        options: MqttOptions,
        requests_rx: Receiver<Request>,
        requests_tx: Option<Sender<Request>>,
    ) -> EventLoop {
        let pending = VecDeque::new();
        let inflight_limit = options.outgoing_inflight_upper_limit.unwrap_or(u16::MAX);
        let manual_acks = options.manual_acks;

        let auth_manager = options.auth_manager();

        EventLoop {
            options,
            state: MqttState::new(inflight_limit, manual_acks, auth_manager),
            requests_rx,
            _requests_tx: requests_tx,
            pending,
            network: None,
            keepalive_timeout: None,
        }
    }

    /// Last session might contain packets which aren't acked. MQTT says these packets should be
    /// republished in the next session. Move pending messages from state to eventloop, drops the
    /// underlying network connection and clears the keepalive timeout if any.
    ///
    /// > NOTE: Use only when EventLoop is blocked on network and unable to immediately handle disconnect.
    /// > Also, while this helps prevent data loss, the pending list length should be managed properly.
    /// > For this reason we recommend setting [`AsycClient`](super::AsyncClient)'s channel capacity to `0`.
    pub fn clean(&mut self) {
        self.network = None;
        self.keepalive_timeout = None;
        self.pending.extend(self.state.clean());

        // drain requests from channel which weren't yet received
        for request in self.requests_rx.drain() {
            // Wait for publish retransmission, else the broker could be confused by an unexpected ack
            if !matches!(request, Request::PubAck(_)) {
                self.pending.push_back(request);
            }
        }
    }

    /// Yields Next notification or outgoing request and periodically pings
    /// the broker. Continuing to poll will reconnect to the broker if there is
    /// a disconnection.
    /// **NOTE** Don't block this while iterating
    pub async fn poll(&mut self) -> Result<Event, ConnectionError> {
        if self.network.is_none() {
            let (network, connack) = time::timeout(
                self.options.connect_timeout(),
                connect(&mut self.options, &mut self.state),
            )
            .await??;
            // Last session might contain packets which aren't acked. If it's a new session, clear the pending packets and any existing collision state.
            if !connack.session_present {
                self.pending.clear();
                self.state.clear_collision();
            }
            self.network = Some(network);

            if self.keepalive_timeout.is_none() && !self.options.keep_alive.is_zero() {
                self.keepalive_timeout = Some(Box::pin(time::sleep(self.options.keep_alive)));
            }

            self.state
                .handle_incoming_packet(Incoming::ConnAck(connack))?;
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
    async fn select(&mut self) -> Result<Event, ConnectionError> {
        let network = self.network.as_mut().unwrap();
        // let await_acks = self.state.await_acks;

        let inflight_full = self.state.inflight >= self.state.max_outgoing_inflight;
        let collision = self.state.collision.is_some();

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
                self.options.pending_throttle
            ), if !self.pending.is_empty() || (!inflight_full && !collision) => match o {
                Ok(request) => {
                    let max_request_batch = self.options.max_request_batch.max(1);
                    let mut should_flush = false;

                    if let Some(outgoing) = self.state.handle_outgoing_packet(request)? {
                        network.write(outgoing).await?;
                        should_flush = true;
                    }

                    for _ in 1..max_request_batch {
                        let inflight_full = self.state.inflight >= self.state.max_outgoing_inflight;
                        let collision = self.state.collision.is_some();

                        if self.pending.is_empty() && (inflight_full || collision) {
                            break;
                        }

                        let next_request = if let Some(request) = self.pending.pop_front() {
                            if !self.options.pending_throttle.is_zero() {
                                time::sleep(self.options.pending_throttle).await;
                            }
                            request
                        } else {
                            match self.requests_rx.try_recv() {
                                Ok(request) => request,
                                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                            }
                        };

                        if let Some(outgoing) = self.state.handle_outgoing_packet(next_request)? {
                            network.write(outgoing).await?;
                            should_flush = true;
                        }
                    }

                    if should_flush {
                        network.flush().await?;
                    }
                    Ok(self.state.events.pop_front().unwrap())
                }
                Err(_) => Err(ConnectionError::RequestsDone),
            },
            // Pull a bunch of packets from network, reply in bunch and yield the first item
            o = network.readb(&mut self.state) => {
                o?;
                // flush all the acks and return first incoming packet
                network.flush().await?;
                Ok(self.state.events.pop_front().unwrap())
            },
            // We generate pings irrespective of network activity. This keeps the ping logic
            // simple. We can change this behavior in future if necessary (to prevent extra pings)
            _ = self.keepalive_timeout.as_mut().unwrap_or(&mut no_sleep),
                if self.keepalive_timeout.is_some() && !self.options.keep_alive.is_zero() => {
                    let timeout = self.keepalive_timeout.as_mut().unwrap();
                    timeout.as_mut().reset(Instant::now() + self.options.keep_alive);

                    if let Some(outgoing) = self.state.handle_outgoing_packet(Request::PingReq)? {
                        network.write(outgoing).await?;
                    }
                    network.flush().await?;
                    Ok(self.state.events.pop_front().unwrap())
            }
        }
    }

    async fn next_request(
        pending: &mut VecDeque<Request>,
        rx: &Receiver<Request>,
        pending_throttle: Duration,
    ) -> Result<Request, ConnectionError> {
        if !pending.is_empty() {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .next() AFTER sleep() otherwise .next() would
            // advance the iterator but the future might be canceled before return
            Ok(pending.pop_front().unwrap())
        } else {
            match rx.recv_async().await {
                Ok(r) => Ok(r),
                Err(_) => Err(ConnectionError::RequestsDone),
            }
        }
    }
}

/// This stream internally processes requests from the request stream provided to the eventloop
/// while also consuming byte stream from the network and yielding mqtt packets as the output of
/// the stream.
/// This function (for convenience) includes internal delays for users to perform internal sleeps
/// between re-connections so that cancel semantics can be used during this sleep
async fn connect(
    options: &mut MqttOptions,
    state: &mut MqttState,
) -> Result<(Network, ConnAck), ConnectionError> {
    // connect to the broker
    let mut network = network_connect(options).await?;

    // make MQTT connection request (which internally awaits for ack)
    let connack = mqtt_connect(options, &mut network, state).await?;

    Ok((network, connack))
}

async fn network_connect(options: &MqttOptions) -> Result<Network, ConnectionError> {
    let max_incoming_pkt_size = options.max_incoming_packet_size();

    // Process Unix files early, as proxy is not supported for them.
    #[cfg(unix)]
    if matches!(options.transport(), Transport::Unix) {
        let file = options.broker_addr.as_str();
        let socket = UnixStream::connect(Path::new(file)).await?;
        let network = Network::new(socket, max_incoming_pkt_size);
        return Ok(network);
    }

    // For websockets domain and port are taken directly from `broker_addr` (which is a url).
    let (domain, port) = match options.transport() {
        #[cfg(feature = "websocket")]
        Transport::Ws => split_url(&options.broker_addr)?,
        #[cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        ))]
        Transport::Wss(_) => split_url(&options.broker_addr)?,
        _ => options.broker_address(),
    };

    let tcp_stream: Box<dyn AsyncReadWrite> = {
        #[cfg(feature = "proxy")]
        match options.proxy() {
            Some(proxy) => {
                proxy
                    .connect(&domain, port, options.network_options())
                    .await?
            }
            None => {
                let addr = format!("{domain}:{port}");
                let tcp = socket_connect(addr, options.network_options()).await?;
                Box::new(tcp)
            }
        }
        #[cfg(not(feature = "proxy"))]
        {
            let addr = format!("{domain}:{port}");
            let tcp = socket_connect(addr, options.network_options()).await?;
            Box::new(tcp)
        }
    };

    let network = match options.transport() {
        Transport::Tcp => Network::new(tcp_stream, max_incoming_pkt_size),
        #[cfg(any(feature = "use-native-tls", feature = "use-rustls-no-provider"))]
        Transport::Tls(tls_config) => {
            let socket =
                tls::tls_connect(&options.broker_addr, options.port, &tls_config, tcp_stream)
                    .await?;
            Network::new(socket, max_incoming_pkt_size)
        }
        #[cfg(unix)]
        Transport::Unix => unreachable!(),
        #[cfg(feature = "websocket")]
        Transport::Ws => {
            let mut request = options.broker_addr.as_str().into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());

            if let Some(request_modifier) = options.request_modifier() {
                request = request_modifier(request).await;
            }

            let (socket, response) =
                async_tungstenite::tokio::client_async(request, tcp_stream).await?;
            validate_response_headers(response)?;

            Network::new(WsAdapter::new(socket), max_incoming_pkt_size)
        }
        #[cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        ))]
        Transport::Wss(tls_config) => {
            let mut request = options.broker_addr.as_str().into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());

            if let Some(request_modifier) = options.request_modifier() {
                request = request_modifier(request).await;
            }

            let connector = tls::websocket_tls_connector(&tls_config)?;

            let (socket, response) = async_tungstenite::tokio::client_async_tls_with_connector(
                request,
                tcp_stream,
                Some(connector),
            )
            .await?;
            validate_response_headers(response)?;

            Network::new(WsAdapter::new(socket), max_incoming_pkt_size)
        }
    };

    Ok(network)
}

async fn mqtt_connect(
    options: &mut MqttOptions,
    network: &mut Network,
    state: &mut MqttState,
) -> Result<ConnAck, ConnectionError> {
    let packet = Packet::Connect(
        Connect {
            client_id: options.client_id(),
            keep_alive: u16::try_from(options.keep_alive().as_secs()).unwrap_or(u16::MAX),
            clean_start: options.clean_start(),
            properties: options.connect_properties(),
        },
        options.last_will(),
        options.credentials(),
    );

    // send mqtt connect packet
    network.write(packet).await?;
    network.flush().await?;

    // validate connack
    loop {
        match network.read().await? {
            Incoming::ConnAck(connack) if connack.code == ConnectReturnCode::Success => {
                if let Some(props) = &connack.properties {
                    if let Some(keep_alive) = props.server_keep_alive {
                        options.keep_alive = Duration::from_secs(u64::from(keep_alive));
                    }
                    network.set_max_outgoing_size(props.max_packet_size);

                    // Override local session_expiry_interval value if set by server.
                    if props.session_expiry_interval.is_some() {
                        options.set_session_expiry_interval(props.session_expiry_interval);
                    }
                }
                return Ok(connack);
            }
            Incoming::ConnAck(connack) => {
                return Err(ConnectionError::ConnectionRefused(connack.code));
            }
            Incoming::Auth(auth) => {
                if let Some(outgoing) = state.handle_incoming_packet(Incoming::Auth(auth))? {
                    network.write(outgoing).await?;
                    network.flush().await?;
                } else {
                    return Err(ConnectionError::AuthProcessingError);
                }
            }
            packet => return Err(ConnectionError::NotConnAck(Box::new(packet))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flume::TryRecvError;

    #[test]
    fn eventloop_new_keeps_internal_sender_alive() {
        let options = MqttOptions::new("test-client", "localhost", 1883);
        let eventloop = EventLoop::new(options, 1);

        assert!(matches!(
            eventloop.requests_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn async_client_constructor_path_allows_channel_shutdown() {
        let options = MqttOptions::new("test-client", "localhost", 1883);
        let (eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        drop(request_tx);

        assert!(matches!(
            eventloop.requests_rx.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn async_client_path_reports_requests_done_after_pending_drain() {
        let options = MqttOptions::new("test-client", "localhost", 1883);
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.pending.push_back(Request::PingReq);
        drop(request_tx);

        let request = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(request, Request::PingReq));

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
        let options = MqttOptions::new("test-client", "localhost", 1883);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.pending.push_back(Request::PingReq);

        let delayed = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::from_millis(50),
        );
        let timed_out = time::timeout(Duration::from_millis(5), delayed).await;

        assert!(timed_out.is_err());
        assert!(matches!(eventloop.pending.front(), Some(Request::PingReq)));
    }

    #[tokio::test]
    async fn next_request_prioritizes_pending_over_channel_messages() {
        let options = MqttOptions::new("test-client", "localhost", 1883);
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);
        eventloop.pending.push_back(Request::PingReq);
        request_tx.send_async(Request::PingReq).await.unwrap();

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, Request::PingReq));
        assert!(eventloop.pending.is_empty());

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(second, Request::PingReq));
    }
}
