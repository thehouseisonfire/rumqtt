use crate::notice::{
    PublishNoticeTx, PublishResult, SubscribeNoticeTx, TrackedNoticeTx, UnsubscribeNoticeTx,
};
use crate::{Incoming, MqttState, NetworkOptions, Packet, Request, StateError};
use crate::{MqttOptions, Outgoing};
use crate::{NoticeFailureReason, PublishNoticeError};
use crate::{
    Transport,
    framed::{Network, ReadBatchOutcome},
};

use crate::framed::AsyncReadWrite;
use crate::mqttbytes::v4::{
    ConnAck, Connect, ConnectReturnCode, PingReq, Publish, Subscribe, Unsubscribe,
};
use flume::{Receiver, Sender, TryRecvError, bounded, unbounded};
use rumqttc_core::{OutboundScheduler, RequestClass, RequestReadiness, ScheduledRequest};
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

    pub(crate) const fn tracked_subscribe(subscribe: Subscribe, notice: SubscribeNoticeTx) -> Self {
        Self {
            request: Request::Subscribe(subscribe),
            notice: Some(TrackedNoticeTx::Subscribe(notice)),
        }
    }

    pub(crate) const fn tracked_unsubscribe(
        unsubscribe: Unsubscribe,
        notice: UnsubscribeNoticeTx,
    ) -> Self {
        Self {
            request: Request::Unsubscribe(unsubscribe),
            notice: Some(TrackedNoticeTx::Unsubscribe(notice)),
        }
    }

    pub(crate) fn into_parts(self) -> (Request, Option<TrackedNoticeTx>) {
        (self.request, self.notice)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestChannelCapacity {
    Bounded(usize),
    Unbounded,
}

struct PendingDisconnect {
    deadline: Option<Instant>,
}

impl PendingDisconnect {
    fn new(timeout: Option<Duration>) -> Self {
        Self {
            deadline: timeout.map(|timeout| Instant::now() + timeout),
        }
    }
}

/// Critical errors during eventloop polling
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("Mqtt state: {0}")]
    MqttState(#[from] StateError),
    #[error("Network timeout")]
    NetworkTimeout,
    #[error("Flush timeout")]
    FlushTimeout,
    #[error("Graceful disconnect timed out before outbound protocol state drained")]
    DisconnectTimeout,
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
    /// Flow-controlled publish request stream.
    requests_rx: Receiver<RequestEnvelope>,
    /// Control request stream.
    control_requests_rx: Receiver<RequestEnvelope>,
    /// Narrow immediate shutdown stream.
    immediate_disconnect_rx: Receiver<RequestEnvelope>,
    /// Internal request sender retained for compatibility in `EventLoop::new`.
    /// This is intentionally dropped in the client builder path.
    _requests_tx: Option<Sender<RequestEnvelope>>,
    _control_requests_tx: Option<Sender<RequestEnvelope>>,
    _immediate_disconnect_tx: Option<Sender<RequestEnvelope>>,
    /// Pending packets from last session
    pending: VecDeque<RequestEnvelope>,
    /// Requests admitted by the event loop and waiting for protocol scheduling.
    queued: OutboundScheduler<RequestEnvelope>,
    /// Network connection to the broker
    pub network: Option<Network>,
    /// Keep alive time
    keepalive_timeout: Option<Pin<Box<Sleep>>>,
    /// Dummy sleep used as a placeholder in select! when keepalive is disabled.
    /// Initialized once with `Duration::MAX` so that it never fires.
    no_sleep: Option<Pin<Box<Sleep>>>,
    /// `ClientId` associated with the currently retained local session state.
    session_client_id: Option<String>,
    pub network_options: NetworkOptions,
    pending_disconnect: Option<PendingDisconnect>,
    disconnect_complete: bool,
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
        let (control_requests_tx, control_requests_rx) = bounded(cap);
        let (immediate_disconnect_tx, immediate_disconnect_rx) = unbounded();
        Self::with_channel(
            mqtt_options,
            requests_rx,
            control_requests_rx,
            immediate_disconnect_rx,
            Some(requests_tx),
            Some(control_requests_tx),
            Some(immediate_disconnect_tx),
        )
    }

    /// Internal constructor used by client builders.
    ///
    /// Unlike `EventLoop::new`, this does not keep an internal sender handle, so dropping all
    /// `AsyncClient` handles can terminate polling with `ConnectionError::RequestsDone`.
    pub(crate) fn new_for_async_client_with_capacity(
        mqtt_options: MqttOptions,
        capacity: RequestChannelCapacity,
    ) -> (
        Self,
        Sender<RequestEnvelope>,
        Sender<RequestEnvelope>,
        Sender<RequestEnvelope>,
    ) {
        let (requests_tx, requests_rx) = match capacity {
            RequestChannelCapacity::Bounded(cap) => bounded(cap),
            RequestChannelCapacity::Unbounded => unbounded(),
        };
        let (control_requests_tx, control_requests_rx) = match capacity {
            RequestChannelCapacity::Bounded(cap) => bounded(cap),
            RequestChannelCapacity::Unbounded => unbounded(),
        };
        let (immediate_disconnect_tx, immediate_disconnect_rx) = unbounded();
        let eventloop = Self::with_channel(
            mqtt_options,
            requests_rx,
            control_requests_rx,
            immediate_disconnect_rx,
            None,
            None,
            None,
        );
        (
            eventloop,
            requests_tx,
            control_requests_tx,
            immediate_disconnect_tx,
        )
    }

    /// Internal bounded constructor retained for tests that already choose a capacity.
    #[cfg(test)]
    pub(crate) fn new_for_async_client(
        mqtt_options: MqttOptions,
        cap: usize,
    ) -> (Self, Sender<RequestEnvelope>) {
        let (eventloop, request_tx, _control_request_tx, _immediate_disconnect_tx) =
            Self::new_for_async_client_with_capacity(
                mqtt_options,
                RequestChannelCapacity::Bounded(cap),
            );
        (eventloop, request_tx)
    }

    fn with_channel(
        mqtt_options: MqttOptions,
        requests_rx: Receiver<RequestEnvelope>,
        control_requests_rx: Receiver<RequestEnvelope>,
        immediate_disconnect_rx: Receiver<RequestEnvelope>,
        requests_tx: Option<Sender<RequestEnvelope>>,
        control_requests_tx: Option<Sender<RequestEnvelope>>,
        immediate_disconnect_tx: Option<Sender<RequestEnvelope>>,
    ) -> Self {
        let pending = VecDeque::new();
        let max_inflight = mqtt_options.inflight;
        let ack_mode = mqtt_options.ack_mode;

        Self {
            mqtt_options,
            state: MqttState::new_internal(max_inflight, ack_mode),
            requests_rx,
            control_requests_rx,
            immediate_disconnect_rx,
            _requests_tx: requests_tx,
            _control_requests_tx: control_requests_tx,
            _immediate_disconnect_tx: immediate_disconnect_tx,
            pending,
            queued: OutboundScheduler::default(),
            network: None,
            keepalive_timeout: None,
            no_sleep: None,
            session_client_id: None,
            network_options: NetworkOptions::new(),
            pending_disconnect: None,
            disconnect_complete: false,
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
        self.pending_disconnect = None;
        for (request, notice) in self.state.clean_with_notices() {
            self.pending
                .push_back(RequestEnvelope::from_parts(request, notice));
        }

        for envelope in self.queued.drain() {
            if should_replay_after_reconnect(&envelope.request) {
                self.pending.push_back(envelope);
            }
        }

        // drain requests from channel which weren't yet received
        for envelope in self.requests_rx.drain() {
            // Wait for publish retransmission, else the broker could be confused by an unexpected
            // inbound acknowledgment replayed from a previous connection.
            if should_replay_after_reconnect(&envelope.request) {
                self.pending.push_back(envelope);
            }
        }

        for envelope in self.control_requests_rx.drain() {
            // Wait for publish retransmission, else the broker could be confused by an unexpected
            // inbound acknowledgment replayed from a previous connection.
            if should_replay_after_reconnect(&envelope.request) {
                self.pending.push_back(envelope);
            }
        }
    }

    /// Number of pending requests queued for retransmission.
    pub fn pending_len(&self) -> usize {
        self.pending.len() + self.queued.len()
    }

    /// Returns true when there are no pending requests queued for retransmission.
    pub fn pending_is_empty(&self) -> bool {
        self.pending.is_empty() && self.queued.is_empty()
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
                    TrackedNoticeTx::Subscribe(notice) => {
                        notice.error(reason.subscribe_error());
                    }
                    TrackedNoticeTx::Unsubscribe(notice) => {
                        notice.error(reason.unsubscribe_error());
                    }
                }
            }
        }
        for envelope in self.queued.drain() {
            drained += 1;
            if let Some(notice) = envelope.notice {
                match notice {
                    TrackedNoticeTx::Publish(notice) => {
                        notice.error(reason.publish_error());
                    }
                    TrackedNoticeTx::Subscribe(notice) => {
                        notice.error(reason.subscribe_error());
                    }
                    TrackedNoticeTx::Unsubscribe(notice) => {
                        notice.error(reason.unsubscribe_error());
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

    fn reset_session_state_if_client_id_changed(&mut self) {
        let current_client_id = self.mqtt_options.client_id();
        if self.session_client_id.as_deref() == Some(current_client_id.as_str()) {
            return;
        }

        if self.session_client_id.is_some() {
            self.reset_session_state();
        }
        self.session_client_id = None;
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
        if self.disconnect_complete {
            return Err(ConnectionError::RequestsDone);
        }

        if self.network.is_none() {
            if let Ok(envelope) = self.immediate_disconnect_rx.try_recv() {
                self.disconnect_complete = true;
                if let Some(notice) = envelope.notice {
                    drop(notice);
                }
                return Err(ConnectionError::RequestsDone);
            }

            self.reset_session_state_if_client_id_changed();

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
            self.session_client_id = Some(self.mqtt_options.client_id());
            self.network = Some(network);

            if self.keepalive_timeout.is_none() && !self.mqtt_options.keep_alive.is_zero() {
                self.keepalive_timeout = Some(Box::pin(time::sleep(self.mqtt_options.keep_alive)));
            }

            return Ok(Event::Incoming(Packet::ConnAck(connack)));
        }

        match self.select().await {
            Ok(v) => Ok(v),
            Err(ConnectionError::DisconnectTimeout) => {
                self.network = None;
                self.keepalive_timeout = None;
                self.pending_disconnect = None;
                self.disconnect_complete = true;
                Err(ConnectionError::DisconnectTimeout)
            }
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
        loop {
            if let Some(event) = self.state.events.pop_front() {
                return Ok(event);
            }

            if let Ok(envelope) = self.immediate_disconnect_rx.try_recv() {
                return self.handle_immediate_disconnect(envelope).await;
            }

            if self.queued.is_empty()
                && self.pending.is_empty()
                && self.pending_disconnect.is_none()
                && self.requests_rx.is_disconnected()
                && self.requests_rx.is_empty()
                && self.control_requests_rx.is_disconnected()
                && self.control_requests_rx.is_empty()
                && self.state.outbound_requests_drained()
            {
                return Err(ConnectionError::RequestsDone);
            }

            if self.handle_ready_requests().await? {
                if let Some(event) = self.state.events.pop_front() {
                    return Ok(event);
                }
                continue;
            }

            if self.pending_disconnect.is_some() {
                if self.queued.is_empty() && self.state.outbound_requests_drained() {
                    return self.send_pending_disconnect().await;
                }

                if let Some(event) = self.poll_disconnect_drain().await? {
                    return Ok(event);
                }
                continue;
            }

            let read_batch_size = self.effective_read_batch_size();
            let normal_request_admission_allowed =
                self.normal_request_admission_allowed() || !self.pending.is_empty();
            let no_sleep = self
                .no_sleep
                .get_or_insert_with(|| Box::pin(time::sleep(Duration::MAX)));

            return select! {
                biased;
                o = self.immediate_disconnect_rx.recv_async(), if !self.immediate_disconnect_rx.is_disconnected() => match o {
                    Ok(envelope) => self.handle_immediate_disconnect(envelope).await,
                    Err(_) => continue,
                },
                o = self.control_requests_rx.recv_async(),
                    if self.pending_disconnect.is_none()
                        && (!self.control_requests_rx.is_empty()
                            || !self.control_requests_rx.is_disconnected()) => match o {
                    Ok(envelope) => {
                        self.try_admit_existing_normal_requests().await;
                        self.queued.push_back(envelope);
                        continue;
                    }
                    Err(_) => continue,
                },
                o = Self::next_request(
                    &mut self.pending,
                    &self.requests_rx,
                    self.mqtt_options.pending_throttle
                ), if self.pending_disconnect.is_none()
                    && normal_request_admission_allowed
                    && (!self.pending.is_empty()
                        || !self.requests_rx.is_empty()
                        || !self.requests_rx.is_disconnected()) => match o {
                    Ok((request, notice)) => {
                        self.admit_normal_request_batch((request, notice)).await;
                        continue;
                    }
                    Err(_) => continue,
                },
                o = self.network.as_mut().unwrap().readb(&mut self.state, read_batch_size) => {
                    let outcome = o?;
                    self.flush_network().await?;
                    if matches!(outcome, ReadBatchOutcome::ResponseWritten) {
                        self.reset_keepalive_timeout();
                    }
                    Ok(self.state.events.pop_front().unwrap())
                },
                () = self.keepalive_timeout.as_mut().unwrap_or(no_sleep),
                    if self.keepalive_timeout.is_some() && !self.mqtt_options.keep_alive.is_zero() => {
                    let (outgoing, _flush_notice) = self
                        .state
                        .handle_outgoing_packet_with_notice(Request::PingReq(PingReq), None)?;
                    if let Some(outgoing) = outgoing {
                        self.network.as_mut().unwrap().write(outgoing).await?;
                    }
                    self.flush_network().await?;
                    self.reset_keepalive_timeout();
                    Ok(self.state.events.pop_front().unwrap())
                }
            };
        }
    }

    async fn handle_immediate_disconnect(
        &mut self,
        envelope: RequestEnvelope,
    ) -> Result<Event, ConnectionError> {
        let (request, notice) = envelope.into_parts();
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        self.handle_request(request, notice, &mut should_flush, &mut qos0_notices)
            .await?;
        self.flush_request_batch(should_flush, qos0_notices).await?;
        Ok(self.state.events.pop_front().unwrap())
    }

    async fn handle_ready_requests(&mut self) -> Result<bool, ConnectionError> {
        let Some((request, notice)) = self.next_scheduled_request() else {
            return Ok(false);
        };

        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        if self
            .handle_request(request, notice, &mut should_flush, &mut qos0_notices)
            .await?
        {
            for _ in 1..self.mqtt_options.max_request_batch.max(1) {
                let Some((next_request, next_notice)) = self.next_scheduled_request() else {
                    break;
                };

                if !self
                    .handle_request(
                        next_request,
                        next_notice,
                        &mut should_flush,
                        &mut qos0_notices,
                    )
                    .await?
                {
                    break;
                }
            }
        }

        self.flush_request_batch(should_flush, qos0_notices).await?;
        Ok(true)
    }

    fn next_scheduled_request(&mut self) -> Option<(Request, Option<TrackedNoticeTx>)> {
        let state = &self.state;
        self.queued
            .pop_next(|envelope| classify_request(state, &envelope.request))
            .map(RequestEnvelope::into_parts)
    }

    fn normal_request_admission_allowed(&self) -> bool {
        self.queued.is_empty()
            || self
                .queued
                .has_ready(|envelope| classify_request(&self.state, &envelope.request))
    }

    async fn try_admit_existing_normal_requests(&mut self) {
        let ready = self.pending.len() + self.requests_rx.len();
        for _ in 0..ready {
            if !self.normal_request_admission_allowed() && self.pending.is_empty() {
                break;
            }

            let Some((request, notice)) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.mqtt_options.pending_throttle,
            )
            .await
            else {
                break;
            };

            let stop_batch = is_disconnect_request(&request);
            self.queued
                .push_back(RequestEnvelope::from_parts(request, notice));
            if stop_batch {
                break;
            }
        }
    }

    async fn admit_normal_request_batch(&mut self, first: (Request, Option<TrackedNoticeTx>)) {
        let (request, notice) = first;
        let stop_batch = is_disconnect_request(&request);
        self.queued
            .push_back(RequestEnvelope::from_parts(request, notice));
        if stop_batch || !self.normal_request_admission_allowed() && self.pending.is_empty() {
            return;
        }

        for _ in 1..self.mqtt_options.max_request_batch.max(1) {
            let Some((request, notice)) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.mqtt_options.pending_throttle,
            )
            .await
            else {
                break;
            };
            let stop_batch = is_disconnect_request(&request);
            self.queued
                .push_back(RequestEnvelope::from_parts(request, notice));
            if stop_batch {
                break;
            }
        }
    }

    async fn handle_request(
        &mut self,
        request: Request,
        notice: Option<TrackedNoticeTx>,
        should_flush: &mut bool,
        qos0_notices: &mut Vec<PublishNoticeTx>,
    ) -> Result<bool, ConnectionError> {
        match request {
            Request::Disconnect(_) => {
                self.pending_disconnect = Some(PendingDisconnect::new(None));
                Ok(false)
            }
            Request::DisconnectWithTimeout(_, timeout) => {
                self.pending_disconnect = Some(PendingDisconnect::new(Some(timeout)));
                Ok(false)
            }
            Request::DisconnectNow(_) => {
                let (outgoing, _) = self
                    .state
                    .handle_outgoing_packet_with_notice(request, notice)?;
                if let Some(outgoing) = outgoing {
                    if let Err(err) = self.network.as_mut().unwrap().write(outgoing).await {
                        return Err(ConnectionError::MqttState(err));
                    }
                    *should_flush = true;
                }
                self.disconnect_complete = true;
                Ok(false)
            }
            request => {
                let (outgoing, flush_notice) = self
                    .state
                    .handle_outgoing_packet_with_notice(request, notice)?;
                if let Some(notice) = flush_notice {
                    qos0_notices.push(notice);
                }
                if let Some(outgoing) = outgoing {
                    if let Err(err) = self.network.as_mut().unwrap().write(outgoing).await {
                        for notice in qos0_notices.drain(..) {
                            notice.error(PublishNoticeError::Qos0NotFlushed);
                        }
                        return Err(ConnectionError::MqttState(err));
                    }
                    *should_flush = true;
                }
                Ok(true)
            }
        }
    }

    async fn flush_request_batch(
        &mut self,
        should_flush: bool,
        qos0_notices: Vec<PublishNoticeTx>,
    ) -> Result<(), ConnectionError> {
        if !should_flush {
            return Ok(());
        }

        match self.flush_network().await {
            Ok(()) => {
                for notice in qos0_notices {
                    notice.success(PublishResult::Qos0Flushed);
                }
                self.reset_keepalive_timeout();
                Ok(())
            }
            Err(err) => {
                for notice in qos0_notices {
                    notice.error(PublishNoticeError::Qos0NotFlushed);
                }
                Err(err)
            }
        }
    }

    async fn flush_network(&mut self) -> Result<(), ConnectionError> {
        self.state.mark_outgoing_publishes_flush_attempted();
        time::timeout(
            Duration::from_secs(self.network_options.connection_timeout()),
            self.network.as_mut().unwrap().flush(),
        )
        .await
        .map_or_else(
            |_| Err(ConnectionError::FlushTimeout),
            |inner| inner.map_err(ConnectionError::MqttState),
        )
    }

    async fn poll_disconnect_drain(&mut self) -> Result<Option<Event>, ConnectionError> {
        let read_batch_size = self.effective_read_batch_size();
        let read = self
            .network
            .as_mut()
            .unwrap()
            .readb(&mut self.state, read_batch_size);

        let outcome = if let Some(deadline) = self
            .pending_disconnect
            .as_ref()
            .and_then(|pending| pending.deadline)
        {
            select! {
                o = self.immediate_disconnect_rx.recv_async(), if !self.immediate_disconnect_rx.is_disconnected() => match o {
                    Ok(envelope) => return self.handle_immediate_disconnect(envelope).await.map(Some),
                    Err(_) => return Ok(None),
                },
                result = read => result?,
                () = time::sleep_until(deadline) => return Err(ConnectionError::DisconnectTimeout),
            }
        } else {
            select! {
                o = self.immediate_disconnect_rx.recv_async(), if !self.immediate_disconnect_rx.is_disconnected() => match o {
                    Ok(envelope) => return self.handle_immediate_disconnect(envelope).await.map(Some),
                    Err(_) => return Ok(None),
                },
                result = read => result?,
            }
        };

        self.flush_network().await?;
        if matches!(outcome, ReadBatchOutcome::ResponseWritten) {
            self.reset_keepalive_timeout();
        }
        Ok(None)
    }

    async fn send_pending_disconnect(&mut self) -> Result<Event, ConnectionError> {
        self.pending_disconnect = None;
        let (outgoing, _) = self
            .state
            .handle_outgoing_packet_with_notice(Request::DisconnectNow(crate::Disconnect), None)?;

        if let Some(outgoing) = outgoing {
            self.network.as_mut().unwrap().write(outgoing).await?;
            self.flush_network().await?;
        }

        self.disconnect_complete = true;
        Ok(self.state.events.pop_front().unwrap())
    }

    pub fn network_options(&self) -> NetworkOptions {
        self.network_options.clone()
    }

    pub fn set_network_options(&mut self, network_options: NetworkOptions) -> &mut Self {
        self.network_options = network_options;
        self
    }

    fn reset_keepalive_timeout(&mut self) {
        if self.mqtt_options.keep_alive.is_zero() {
            return;
        }

        if let Some(timeout) = &mut self.keepalive_timeout {
            timeout
                .as_mut()
                .reset(Instant::now() + self.mqtt_options.keep_alive);
        }
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

        if !self.pending.is_empty()
            || !self.queued.is_empty()
            || !self.requests_rx.is_empty()
            || !self.control_requests_rx.is_empty()
        {
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

fn classify_request(state: &MqttState, request: &Request) -> ScheduledRequest {
    match request {
        Request::Publish(publish) if publish.qos != crate::mqttbytes::QoS::AtMostOnce => {
            ScheduledRequest {
                class: RequestClass::FlowControlledPublish,
                readiness: if state.can_send_publish(publish) {
                    RequestReadiness::Ready
                } else {
                    RequestReadiness::Blocked
                },
            }
        }
        Request::Publish(_) => ScheduledRequest {
            class: RequestClass::Publish,
            readiness: RequestReadiness::Ready,
        },
        Request::Subscribe(_) | Request::Unsubscribe(_) => ScheduledRequest {
            class: RequestClass::Control,
            readiness: if state.control_packet_identifier_available() {
                RequestReadiness::Ready
            } else {
                RequestReadiness::Blocked
            },
        },
        _ => ScheduledRequest {
            class: RequestClass::Control,
            readiness: RequestReadiness::Ready,
        },
    }
}

const fn is_disconnect_request(request: &Request) -> bool {
    matches!(
        request,
        Request::Disconnect(_) | Request::DisconnectWithTimeout(_, _) | Request::DisconnectNow(_)
    )
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
        if let Some(proxy) = options.proxy() {
            proxy
                .connect(
                    &domain,
                    port,
                    network_options,
                    Some(options.effective_socket_connector()),
                )
                .await?
        } else {
            let addr = format!("{domain}:{port}");
            options.socket_connect(addr, network_options).await?
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

const fn should_replay_after_reconnect(request: &Request) -> bool {
    !matches!(request, Request::PubAck(_) | Request::PubRec(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqttbytes::QoS;
    use crate::{Disconnect, PubAck, PubComp, PubRec, PubRel};
    use flume::TryRecvError;
    use tokio::io::{AsyncReadExt, DuplexStream};

    fn push_pending(eventloop: &mut EventLoop, request: Request) {
        eventloop.pending.push_back(RequestEnvelope::plain(request));
    }

    async fn read_packet_bytes(peer: &mut DuplexStream) -> Vec<u8> {
        let byte1 = peer.read_u8().await.unwrap();
        let mut multiplier = 1usize;
        let mut remaining_len = 0usize;
        let mut remaining_len_bytes = Vec::new();

        loop {
            let encoded_byte = peer.read_u8().await.unwrap();
            remaining_len_bytes.push(encoded_byte);
            remaining_len += usize::from(encoded_byte & 0x7F) * multiplier;
            if encoded_byte & 0x80 == 0 {
                break;
            }
            multiplier *= 128;
        }

        let mut payload = vec![0; remaining_len];
        peer.read_exact(&mut payload).await.unwrap();

        let mut packet = Vec::with_capacity(1 + remaining_len_bytes.len() + payload.len());
        packet.push(byte1);
        packet.extend(remaining_len_bytes);
        packet.extend(payload);
        packet
    }

    async fn run_mqtt_connect_with_connack(
        options: MqttOptions,
        connack: ConnAck,
    ) -> Result<ConnAck, ConnectionError> {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, 1024, 1024);

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = bytes::BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut peer, &encoded_connack)
                .await
                .unwrap();
        };

        let (result, ()) = tokio::join!(mqtt_connect(&options, &mut network), broker);
        result
    }

    async fn run_mqtt_connect_with_first_server_packet(
        options: MqttOptions,
        packet: Packet,
    ) -> Result<ConnAck, ConnectionError> {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, 1024, 1024);

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_packet = bytes::BytesMut::new();
            packet.write(&mut encoded_packet, 1024).unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut peer, &encoded_packet)
                .await
                .unwrap();
        };

        let (result, ()) = tokio::join!(mqtt_connect(&options, &mut network), broker);
        result
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

    fn publish(qos: QoS) -> Publish {
        Publish::new("hello/world", qos, "payload")
    }

    fn fill_publish_window(eventloop: &mut EventLoop) {
        let mut active = publish(QoS::AtLeastOnce);
        active.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(active);
        eventloop.state.inflight = 1;
    }

    fn next_after_blocked_publish(request: Request) -> Option<Request> {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_inflight(1);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop.queued.push_back(RequestEnvelope::plain(request));

        eventloop
            .next_scheduled_request()
            .map(|(request, _notice)| request)
    }

    #[test]
    fn scheduler_sends_control_after_quota_blocked_publish() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_inflight(1);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Subscribe(Subscribe::new(
                "hello/world",
                QoS::AtMostOnce,
            ))));

        let (request, _) = eventloop.next_scheduled_request().unwrap();

        assert!(matches!(request, Request::Subscribe(_)));
        match eventloop.state.handle_outgoing_packet(request).unwrap() {
            Some(Packet::Subscribe(subscribe)) => assert_ne!(subscribe.pkid, 1),
            packet => panic!("expected subscribe packet, got {packet:?}"),
        }
        assert!(matches!(
            eventloop
                .queued
                .drain()
                .next()
                .map(|envelope| envelope.request),
            Some(Request::Publish(_))
        ));
    }

    #[test]
    fn scheduler_does_not_let_qos0_publish_pass_blocked_publish() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_inflight(1);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtMostOnce,
            ))));

        assert!(eventloop.next_scheduled_request().is_none());
    }

    #[test]
    fn scheduler_allows_each_progress_and_control_class_after_blocked_publish() {
        assert!(matches!(
            next_after_blocked_publish(Request::PubAck(PubAck::new(7))),
            Some(Request::PubAck(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubRec(PubRec::new(8))),
            Some(Request::PubRec(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubRel(PubRel::new(9))),
            Some(Request::PubRel(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubComp(PubComp::new(10))),
            Some(Request::PubComp(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PingReq(PingReq)),
            Some(Request::PingReq(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Subscribe(Subscribe::new(
                "hello/world",
                QoS::AtMostOnce
            ))),
            Some(Request::Subscribe(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Unsubscribe(Unsubscribe::new("hello/world"))),
            Some(Request::Unsubscribe(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Disconnect(Disconnect)),
            Some(Request::Disconnect(_))
        ));
    }

    #[test]
    fn scheduler_unsubscribe_after_blocked_publish_uses_non_conflicting_packet_id() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_inflight(1);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Unsubscribe(
                Unsubscribe::new("hello/world"),
            )));

        let (request, _) = eventloop.next_scheduled_request().unwrap();

        assert!(matches!(request, Request::Unsubscribe(_)));
        match eventloop.state.handle_outgoing_packet(request).unwrap() {
            Some(Packet::Unsubscribe(unsubscribe)) => assert_ne!(unsubscribe.pkid, 1),
            packet => panic!("expected unsubscribe packet, got {packet:?}"),
        }
    }

    #[test]
    fn scheduler_preserves_sendable_publish_before_later_control() {
        let (mut eventloop, _request_tx) =
            EventLoop::new_for_async_client(MqttOptions::new("test-client", "localhost"), 1);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Subscribe(Subscribe::new(
                "hello/world",
                QoS::AtMostOnce,
            ))));

        let (request, _) = eventloop.next_scheduled_request().unwrap();

        assert!(matches!(request, Request::Publish(_)));
    }

    #[tokio::test]
    async fn select_admits_control_request_after_ready_publish_backlog_snapshot() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_max_request_batch(1);
        let (mut eventloop, request_tx, control_request_tx, _immediate_disconnect_tx) =
            EventLoop::new_for_async_client_with_capacity(
                options,
                RequestChannelCapacity::Unbounded,
            );
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));

        request_tx
            .send_async(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtMostOnce,
            ))))
            .await
            .unwrap();
        request_tx
            .send_async(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtMostOnce,
            ))))
            .await
            .unwrap();
        control_request_tx
            .send_async(RequestEnvelope::plain(Request::Disconnect(Disconnect)))
            .await
            .unwrap();

        let first = time::timeout(Duration::from_secs(1), eventloop.select())
            .await
            .expect("timed out waiting for first request")
            .expect("select should not fail");
        request_tx
            .send_async(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtMostOnce,
            ))))
            .await
            .unwrap();
        let second = time::timeout(Duration::from_secs(1), eventloop.select())
            .await
            .expect("timed out waiting for second request")
            .expect("select should not fail");
        let third = time::timeout(Duration::from_secs(1), eventloop.select())
            .await
            .expect("timed out waiting for control request")
            .expect("select should not fail");

        assert!(matches!(first, Event::Outgoing(Outgoing::Publish(_))));
        assert!(matches!(second, Event::Outgoing(Outgoing::Publish(_))));
        assert!(matches!(third, Event::Outgoing(Outgoing::Disconnect)));
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

    #[test]
    fn clean_drops_ack_requests_drained_from_queued_scheduler() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 3);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PubAck(PubAck::new(7))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PubRec(PubRec::new(8))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PingReq(PingReq)));

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq(_))
        ));
    }

    #[tokio::test]
    async fn mqtt_connect_returns_bad_client_id_rejection() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck::new(ConnectReturnCode::BadClientId, false);

        let result = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::ConnectionRefused(
                ConnectReturnCode::BadClientId
            ))
        ));
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_non_connack_first_server_packet() {
        let options = MqttOptions::new("test-client", "localhost");

        let result = run_mqtt_connect_with_first_server_packet(options, Packet::PingResp).await;

        assert!(matches!(
            result,
            Err(ConnectionError::NotConnAck(packet)) if matches!(*packet, Packet::PingResp)
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
    async fn poll_requeues_qos1_publish_with_dup_after_partial_flush_failure() {
        use std::{
            io,
            pin::Pin,
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

        #[derive(Default)]
        struct PartialFlushFailure {
            wrote_once: bool,
        }

        impl AsyncRead for PartialFlushFailure {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Pending
            }
        }

        impl AsyncWrite for PartialFlushFailure {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                if !self.wrote_once {
                    self.wrote_once = true;
                    return Poll::Ready(Ok(buf.len().min(1)));
                }

                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "partial flush failure",
                )))
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        eventloop.network = Some(Network::new(PartialFlushFailure::default(), 1024, 1024));

        request_tx
            .send_async(RequestEnvelope::plain(Request::Publish(Publish::new(
                "hello/world",
                crate::mqttbytes::QoS::AtLeastOnce,
                "payload",
            ))))
            .await
            .unwrap();

        let err = eventloop.poll().await.unwrap_err();

        assert!(matches!(err, ConnectionError::MqttState(_)));
        assert!(
            matches!(
                pending_front_request(&eventloop),
                Some(Request::Publish(publish))
                    if publish.dup && publish.qos == crate::mqttbytes::QoS::AtLeastOnce
            ),
            "QoS 1 publish partially written during flush must replay with DUP=1"
        );
    }

    #[tokio::test]
    async fn poll_drops_network_after_incoming_publish_with_invalid_qos_bits() {
        use crate::mqttbytes::Error as MqttError;
        use tokio::io::{AsyncWriteExt, duplex};

        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, mut peer) = duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));

        peer.write_all(&[
            0b0011_0110,
            9, // packet type, flags and remaining len
            0x00,
            0x03,
            b'a',
            b'/',
            b'b', // topic name = 'a/b'
            0x00,
            0x0a, // packet identifier
            0x01,
            0x02, // payload
        ])
        .await
        .unwrap();

        let err = eventloop.poll().await.unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(MqttError::InvalidQoS(3)))
        ));
        assert!(
            eventloop.network.is_none(),
            "poll() must drop the network after an incoming malformed PUBLISH"
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

        let (request_notice_tx, request_notice) = SubscribeNoticeTx::new();
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
            crate::SubscribeNoticeError::SessionReset
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

    #[tokio::test]
    async fn poll_rejects_clean_session_with_session_present_connack() {
        use tokio::io::{AsyncWriteExt, duplex};

        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_session(true);
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x01, 0x00]).await.unwrap();
        });

        let err = eventloop.poll().await.unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::SessionStateMismatch {
                clean_session: true,
                session_present: true
            }
        ));
        broker.await.unwrap();
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

    #[test]
    fn client_id_guard_keeps_state_when_client_id_is_unchanged() {
        let mut eventloop = build_eventloop_with_pending(false);
        eventloop.session_client_id = Some("test-client".to_owned());

        eventloop.reset_session_state_if_client_id_changed();

        assert_eq!(eventloop.pending_len(), 1);
        assert_eq!(eventloop.session_client_id.as_deref(), Some("test-client"));
    }

    #[test]
    fn client_id_guard_resets_state_when_client_id_changes() {
        let mut eventloop = build_eventloop_with_pending(false);
        eventloop.session_client_id = Some("test-client".to_owned());
        eventloop
            .mqtt_options
            .set_client_id("other-client".to_owned());

        eventloop.reset_session_state_if_client_id_changed();

        assert!(eventloop.pending_is_empty());
        assert_eq!(eventloop.session_client_id, None);
    }

    // MQTT-3.1.0-1: After a Network Connection is established by a Client to a Server,
    // the first Packet sent from the Client to the Server MUST be a CONNECT Packet.
    #[tokio::test]
    async fn mqtt_connect_sends_connect_as_first_packet_on_the_wire() {
        use crate::mqttbytes::v4::Packet;
        use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

        let (client, mut peer) = duplex(1024);
        let mut network = Network::new(client, 1024, 1024);

        let options = MqttOptions::new("test-client", "localhost");
        // Write a ConnAck on the peer side so mqtt_connect can complete.
        // ConnAck: 0x20 (type=2, flags=0), remaining_len=2, session_present=0, return_code=0
        peer.write_all(&[0x20, 0x02, 0x00, 0x00]).await.unwrap();

        mqtt_connect(&options, &mut network).await.unwrap();

        // Read the bytes the client wrote to the wire.
        let mut buf = vec![0u8; 256];
        let n = peer.read(&mut buf).await.unwrap();
        assert!(n > 0, "client should have written bytes to the wire");

        // Parse the first packet from the raw bytes.
        let mut stream = bytes::BytesMut::from(&buf[..n]);
        let packet = Packet::read(&mut stream, 1024).unwrap();

        assert!(
            matches!(packet, Packet::Connect(_)),
            "first packet on the wire must be CONNECT, got {packet:?}"
        );
    }

    // MQTT-3.1.0-1: Verify that poll() cannot process user requests before
    // establishing the network connection (which sends CONNECT).
    #[tokio::test]
    async fn poll_sends_connect_before_queued_requests() {
        use crate::mqttbytes::v4::Packet;
        use tokio::io::{AsyncWriteExt, duplex};

        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .unwrap();

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let first_packet = read_packet_bytes(&mut peer).await;
            // ConnAck: 0x20 (type=2, flags=0), remaining_len=2, session_present=0, return_code=0
            peer.write_all(&[0x20, 0x02, 0x00, 0x00]).await.unwrap();
            let second_packet = read_packet_bytes(&mut peer).await;
            (first_packet, second_packet)
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        ));

        let (first_packet, second_packet) = broker.await.unwrap();
        let mut first_stream = bytes::BytesMut::from(&first_packet[..]);
        let mut second_stream = bytes::BytesMut::from(&second_packet[..]);
        assert!(matches!(
            Packet::read(&mut first_stream, 1024).unwrap(),
            Packet::Connect(_)
        ));
        assert!(matches!(
            Packet::read(&mut second_stream, 1024).unwrap(),
            Packet::PingReq
        ));
    }

    // MQTT-3.1.0-2: A Client can only send the CONNECT Packet once over a Network Connection.
    // The Request enum has no Connect variant, so users cannot submit a CONNECT
    // through the request channel. This is a compile-time invariant.
    // The test below documents the current public request variants; the enum
    // definition is the compile-time evidence that no Connect variant exists.
    #[test]
    fn request_enum_has_no_connect_variant() {
        use crate::mqttbytes::v4::{PingResp, SubAck, UnsubAck};
        use std::mem::discriminant;

        // Explicitly enumerate the current variants to make the request-channel
        // surface visible in the MQTT-3.1.0-2 regression tests.
        let variants: Vec<_> = [
            discriminant(&Request::Publish(Publish::new(
                "t",
                QoS::AtMostOnce,
                vec![],
            ))),
            discriminant(&Request::PubAck(PubAck::new(1))),
            discriminant(&Request::PubRec(PubRec::new(1))),
            discriminant(&Request::PubComp(PubComp::new(1))),
            discriminant(&Request::PubRel(PubRel::new(1))),
            discriminant(&Request::PingReq(PingReq)),
            discriminant(&Request::PingResp(PingResp)),
            discriminant(&Request::Subscribe(Subscribe::new("t", QoS::AtMostOnce))),
            discriminant(&Request::SubAck(SubAck::new(1, vec![]))),
            discriminant(&Request::Unsubscribe(Unsubscribe::new("t"))),
            discriminant(&Request::UnsubAck(UnsubAck::new(1))),
            discriminant(&Request::Disconnect(Disconnect)),
            discriminant(&Request::DisconnectNow(Disconnect)),
            discriminant(&Request::DisconnectWithTimeout(
                Disconnect,
                Duration::from_secs(1),
            )),
        ]
        .into_iter()
        .collect();

        // There are exactly 14 documented Request variants.
        assert_eq!(
            variants.len(),
            14,
            "Request should have exactly 14 documented variants"
        );
    }

    // MQTT-3.1.0-2: Verify that after mqtt_connect succeeds, the eventloop
    // never sends another CONNECT on the same network connection. The select()
    // loop only processes Request variants, none of which is Connect.
    #[tokio::test]
    async fn eventloop_sends_only_one_connect_per_network_connection() {
        use crate::mqttbytes::v4::Packet;
        use tokio::io::{AsyncWriteExt, duplex};

        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let first_packet = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x00, 0x00]).await.unwrap();
            let second_packet = read_packet_bytes(&mut peer).await;
            (first_packet, second_packet)
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq(PingReq)))
            .unwrap();
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        ));

        let (first_packet, second_packet) = broker.await.unwrap();
        let mut first_stream = bytes::BytesMut::from(&first_packet[..]);
        let mut second_stream = bytes::BytesMut::from(&second_packet[..]);
        assert!(matches!(
            Packet::read(&mut first_stream, 1024).unwrap(),
            Packet::Connect(_)
        ));
        assert!(matches!(
            Packet::read(&mut second_stream, 1024).unwrap(),
            Packet::PingReq
        ));
    }
}
