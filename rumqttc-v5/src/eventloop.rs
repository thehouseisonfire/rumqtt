use super::framed::Network;
use super::mqttbytes::v5::{
    ConnAck, Connect, ConnectProperties, Disconnect, DisconnectReasonCode, Packet, Publish,
    Subscribe, Unsubscribe,
};
use super::{Incoming, MqttOptions, MqttState, Outgoing, Request, StateError, Transport};
use crate::framed::AsyncReadWrite;
use crate::notice::{
    AuthNoticeTx, PublishNoticeTx, PublishResult, SubscribeNoticeTx, TrackedNoticeTx,
    UnsubscribeNoticeError, UnsubscribeNoticeTx,
};
use crate::{AuthEvent, NoticeFailureReason, PublishNoticeError, SubscribeNoticeError};

use flume::{Receiver, Sender, TryRecvError, bounded, unbounded};
use rumqttc_core::{OutboundScheduler, RequestClass, RequestReadiness, ScheduledRequest};
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

    pub(crate) const fn tracked_auth(
        auth: super::mqttbytes::v5::Auth,
        notice: AuthNoticeTx,
    ) -> Self {
        Self {
            request: Request::Auth(auth),
            notice: Some(TrackedNoticeTx::Auth(notice)),
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
    disconnect: super::mqttbytes::v5::Disconnect,
    deadline: Option<Instant>,
}

impl PendingDisconnect {
    fn new(disconnect: super::mqttbytes::v5::Disconnect, timeout: Option<Duration>) -> Self {
        Self {
            disconnect,
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
    #[error("Timeout")]
    Timeout(#[from] Elapsed),
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
    #[error("Broker replied with session_present={session_present} for clean_start={clean_start}")]
    SessionStateMismatch {
        clean_start: bool,
        session_present: bool,
    },
    #[error("Broker target is incompatible with the selected transport")]
    BrokerTransportMismatch,
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
    #[cfg(feature = "websocket")]
    #[error("Websocket request modifier failed: {0}")]
    RequestModifier(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Eventloop with all the state of a connection
pub struct EventLoop {
    /// Options of the current mqtt connection
    pub options: MqttOptions,
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
    network: Option<Network>,
    /// Keep alive time
    keepalive_timeout: Option<Pin<Box<Sleep>>>,
    /// Dummy sleep used as a placeholder in select! when keepalive is disabled.
    /// Initialized once with `Duration::MAX` so that it never fires.
    no_sleep: Option<Pin<Box<Sleep>>>,
    /// `ClientId` associated with the currently retained local session state.
    session_client_id: Option<String>,
    /// Current connection resumed broker state without matching local packet-id ownership.
    broker_only_session_resume: bool,
    pending_disconnect: Option<PendingDisconnect>,
    disconnect_complete: bool,
}

/// Events which can be yielded by the event loop
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Incoming(Incoming),
    Outgoing(Outgoing),
    Auth(AuthEvent),
}

impl EventLoop {
    fn has_local_session_state(&self) -> bool {
        self.session_client_id.as_deref() == Some(self.options.client_id().as_str())
    }

    fn reconcile_connack_session(&mut self, session_present: bool) -> Result<(), ConnectionError> {
        let clean_start = self.options.clean_start();
        let has_local_state = self.has_local_session_state();
        let missing_local_state = !has_local_state;
        let allow_broker_only_resume = self
            .options
            .allow_broker_session_resume_without_local_state();
        if session_present && (clean_start || (missing_local_state && !allow_broker_only_resume)) {
            return Err(ConnectionError::SessionStateMismatch {
                clean_start,
                session_present,
            });
        }

        if !session_present {
            self.reset_session_state();
        }

        self.broker_only_session_resume =
            session_present && missing_local_state && allow_broker_only_resume;

        Ok(())
    }

    /// New MQTT `EventLoop`
    ///
    /// When connection encounters critical errors (like auth failure), user has a choice to
    /// access and update `options`, `state` and `requests`.
    pub fn new(options: MqttOptions, cap: usize) -> Self {
        let (requests_tx, requests_rx) = bounded(cap);
        let (control_requests_tx, control_requests_rx) = bounded(cap);
        let (immediate_disconnect_tx, immediate_disconnect_rx) = unbounded();
        Self::with_channel(
            options,
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
        options: MqttOptions,
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
            options,
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
        options: MqttOptions,
        cap: usize,
    ) -> (Self, Sender<RequestEnvelope>) {
        let (eventloop, request_tx, _control_request_tx, _immediate_disconnect_tx) =
            Self::new_for_async_client_with_capacity(options, RequestChannelCapacity::Bounded(cap));
        (eventloop, request_tx)
    }

    fn with_channel(
        options: MqttOptions,
        requests_rx: Receiver<RequestEnvelope>,
        control_requests_rx: Receiver<RequestEnvelope>,
        immediate_disconnect_rx: Receiver<RequestEnvelope>,
        requests_tx: Option<Sender<RequestEnvelope>>,
        control_requests_tx: Option<Sender<RequestEnvelope>>,
        immediate_disconnect_tx: Option<Sender<RequestEnvelope>>,
    ) -> Self {
        let pending = VecDeque::new();
        let inflight_limit = options.outgoing_inflight_upper_limit.unwrap_or(u16::MAX);
        let ack_mode = options.ack_mode;
        let auto_topic_aliases = options.auto_topic_aliases();
        let topic_alias_policy = options.topic_alias_policy();
        let client_topic_alias_max = options.topic_alias_max().unwrap_or(0);

        let authenticator = options.authenticator();
        let authentication_method = options.authentication_method();

        Self {
            options,
            state: MqttState::new_internal(
                inflight_limit,
                ack_mode,
                auto_topic_aliases,
                topic_alias_policy,
                client_topic_alias_max,
                authentication_method,
                authenticator,
            ),
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
            broker_only_session_resume: false,
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
        let mut replay_topic_aliases = self.state.replay_topic_aliases();

        for (request, notice) in self.state.clean_with_notices() {
            self.push_replay_envelope(
                RequestEnvelope::from_parts(request, notice),
                &mut replay_topic_aliases,
            );
        }

        let queued: Vec<_> = self.queued.drain().collect();
        for envelope in queued {
            if should_replay_after_reconnect(&envelope.request) {
                self.push_replay_envelope(envelope, &mut replay_topic_aliases);
            }
        }

        // drain requests from channel which weren't yet received
        let drained_requests: Vec<_> = self.requests_rx.drain().collect();
        for envelope in drained_requests {
            // Wait for publish retransmission, else the broker could be confused by an unexpected
            // inbound acknowledgment replayed from a previous connection.
            if should_replay_after_reconnect(&envelope.request) {
                self.push_replay_envelope(envelope, &mut replay_topic_aliases);
            }
        }

        let drained_control_requests: Vec<_> = self.control_requests_rx.drain().collect();
        for envelope in drained_control_requests {
            // Wait for publish retransmission, else the broker could be confused by an unexpected
            // inbound acknowledgment replayed from a previous connection.
            if should_replay_after_reconnect(&envelope.request) {
                self.push_replay_envelope(envelope, &mut replay_topic_aliases);
            }
        }

        self.state.fail_auth_exchange_due_to_session_reset();
        self.state.reset_connection_scoped_state();
        self.state.events.clear();
    }

    fn push_replay_envelope(
        &mut self,
        mut envelope: RequestEnvelope,
        replay_topic_aliases: &mut std::collections::HashMap<u16, bytes::Bytes>,
    ) {
        if let Err(err) = MqttState::prepare_request_for_replay_with_aliases(
            &mut envelope.request,
            replay_topic_aliases,
        ) {
            if let Some(TrackedNoticeTx::Publish(notice)) = envelope.notice {
                notice.error(err);
            }
            return;
        }

        self.pending.push_back(envelope);
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
                    TrackedNoticeTx::Auth(notice) => {
                        notice.error(reason.auth_error());
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
                    TrackedNoticeTx::Auth(notice) => {
                        notice.error(reason.auth_error());
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
        self.state.fail_reauth_exchange_due_to_session_reset();
        self.state.reset_connection_scoped_state();
        self.session_client_id = None;
        self.broker_only_session_resume = false;
    }

    fn reset_session_state_if_client_id_changed(&mut self) {
        let current_client_id = self.options.client_id();
        if self.session_client_id.as_deref() == Some(current_client_id.as_str()) {
            return;
        }

        if self.session_client_id.is_some() {
            self.reset_session_state();
        }
        self.session_client_id = None;
    }

    fn reconcile_outgoing_tracking_after_connack(&mut self) {
        self.state
            .reconcile_outgoing_tracking_capacity(self.pending.is_empty());
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

            let (network, connack) = time::timeout(
                self.options.connect_timeout(),
                connect(&mut self.options, &mut self.state),
            )
            .await??;
            self.reconcile_connack_session(connack.session_present)?;
            if !self.broker_only_session_resume {
                self.session_client_id = Some(self.options.client_id());
            }
            self.network = Some(network);

            if self.keepalive_timeout.is_none() && !self.options.keep_alive.is_zero() {
                self.keepalive_timeout = Some(Box::pin(time::sleep(self.options.keep_alive)));
            }

            self.state
                .handle_incoming_packet(Incoming::ConnAck(connack))?;
            self.reconcile_outgoing_tracking_after_connack();
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
                    self.options.pending_throttle
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
                    o?;
                    self.flush_network().await?;
                    Ok(self.state.events.pop_front().unwrap())
                },
                () = self.keepalive_timeout.as_mut().unwrap_or(no_sleep),
                    if self.keepalive_timeout.is_some() && !self.options.keep_alive.is_zero() => {
                    let timeout = self.keepalive_timeout.as_mut().unwrap();
                    timeout.as_mut().reset(Instant::now() + self.options.keep_alive);

                    let (outgoing, _flush_notice) = self
                        .state
                        .handle_outgoing_packet_with_notice(Request::PingReq, None)?;
                    if let Some(outgoing) = outgoing {
                        self.network.as_mut().unwrap().write(outgoing).await?;
                    }
                    self.flush_network().await?;
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
            for _ in 1..self.options.max_request_batch.max(1) {
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
                self.options.pending_throttle,
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

        for _ in 1..self.options.max_request_batch.max(1) {
            let Some((request, notice)) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.options.pending_throttle,
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
        if self.broker_only_session_resume && request_allocates_client_packet_id(&request) {
            reject_broker_only_session_request(&request, notice);
            return Ok(true);
        }

        match request {
            Request::Disconnect(disconnect) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
                self.pending_disconnect = Some(PendingDisconnect::new(disconnect, None));
                Ok(false)
            }
            Request::DisconnectWithTimeout(disconnect, timeout) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
                self.pending_disconnect = Some(PendingDisconnect::new(disconnect, Some(timeout)));
                Ok(false)
            }
            Request::DisconnectNow(_) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
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
        self.network.as_mut().unwrap().flush().await?;
        Ok(())
    }

    async fn poll_disconnect_drain(&mut self) -> Result<Option<Event>, ConnectionError> {
        let read_batch_size = self.effective_read_batch_size();
        let read = self
            .network
            .as_mut()
            .unwrap()
            .readb(&mut self.state, read_batch_size);

        if let Some(deadline) = self
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
        }

        self.flush_network().await?;
        Ok(None)
    }

    async fn send_pending_disconnect(&mut self) -> Result<Event, ConnectionError> {
        let disconnect = self
            .pending_disconnect
            .take()
            .expect("pending disconnect checked by caller")
            .disconnect;
        let (outgoing, _) = self
            .state
            .handle_outgoing_packet_with_notice(Request::DisconnectNow(disconnect), None)?;

        if let Some(outgoing) = outgoing {
            self.network.as_mut().unwrap().write(outgoing).await?;
            self.flush_network().await?;
        }

        self.disconnect_complete = true;
        Ok(self.state.events.pop_front().unwrap())
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
            // We must call .next() AFTER sleep() otherwise .next() would
            // advance the iterator but the future might be canceled before return
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
            // We must call .next() AFTER sleep() otherwise .next() would
            // advance the iterator but the future might be canceled before return
            Ok(pending.pop_front().unwrap().into_parts())
        }
    }

    fn effective_read_batch_size(&self) -> usize {
        const MAX_READ_BATCH_SIZE: usize = 128;
        const PENDING_FAIRNESS_CAP: usize = 16;

        let configured = self.options.read_batch_size();
        if configured > 0 {
            return configured.clamp(1, MAX_READ_BATCH_SIZE);
        }

        let request_batch = self.options.max_request_batch().max(1);
        let inflight = usize::from(self.state.max_outgoing_inflight);
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

fn request_allocates_client_packet_id(request: &Request) -> bool {
    match request {
        Request::Publish(publish) => publish.qos != crate::mqttbytes::QoS::AtMostOnce,
        Request::Subscribe(_) | Request::Unsubscribe(_) => true,
        _ => false,
    }
}

fn reject_broker_only_session_request(request: &Request, notice: Option<TrackedNoticeTx>) {
    warn!(
        "rejecting outbound request that needs a client packet identifier after broker-only session resume: {request:?}"
    );

    match notice {
        Some(TrackedNoticeTx::Publish(notice)) => {
            notice.error(PublishNoticeError::BrokerOnlySessionResume);
        }
        Some(TrackedNoticeTx::Subscribe(notice)) => {
            notice.error(SubscribeNoticeError::BrokerOnlySessionResume);
        }
        Some(TrackedNoticeTx::Unsubscribe(notice)) => {
            notice.error(UnsubscribeNoticeError::BrokerOnlySessionResume);
        }
        Some(TrackedNoticeTx::Auth(notice)) => {
            drop(notice);
        }
        None => {}
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
    options: &mut MqttOptions,
    state: &mut MqttState,
) -> Result<(Network, ConnAck), ConnectionError> {
    // connect to the broker
    let mut network = network_connect(options).await?;

    // make MQTT connection request (which internally awaits for ack)
    let connack = mqtt_connect(options, &mut network, state).await?;

    Ok((network, connack))
}

#[allow(clippy::too_many_lines)]
async fn network_connect(options: &MqttOptions) -> Result<Network, ConnectionError> {
    let max_incoming_pkt_size = options.max_incoming_packet_size();
    let transport = options.transport();

    // Process Unix files early, as proxy is not supported for them.
    #[cfg(unix)]
    if matches!(&transport, Transport::Unix) {
        let file = options
            .broker()
            .unix_path()
            .ok_or(ConnectionError::BrokerTransportMismatch)?;
        let socket = UnixStream::connect(Path::new(file)).await?;
        let network = Network::new(socket, max_incoming_pkt_size);
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
                    options.network_options(),
                    Some(options.effective_socket_connector()),
                )
                .await?
        } else {
            let addr = format!("{domain}:{port}");
            options
                .socket_connect(addr, options.network_options())
                .await?
        }
        #[cfg(not(feature = "proxy"))]
        {
            let addr = format!("{domain}:{port}");
            options
                .socket_connect(addr, options.network_options())
                .await?
        }
    };

    let network = match transport {
        Transport::Tcp => Network::new(tcp_stream, max_incoming_pkt_size),
        #[cfg(any(feature = "use-native-tls", feature = "use-rustls-no-provider"))]
        Transport::Tls(tls_config) => {
            let (host, port) = options
                .broker()
                .tcp_address()
                .expect("tls transport requires a tcp broker");
            let socket = tls::tls_connect(host, port, &tls_config, tcp_stream).await?;
            Network::new(socket, max_incoming_pkt_size)
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

            Network::new(WsAdapter::new(socket), max_incoming_pkt_size)
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
    state.set_client_topic_alias_max(options.topic_alias_max());
    let authentication_method = options.authentication_method();
    let auth_exchange_started = authentication_method.is_some();
    let start_auth_properties = state.begin_authentication_connect(authentication_method)?;
    let mut connect_properties = options.connect_properties();
    if let Some(auth_properties) = start_auth_properties {
        let properties = connect_properties.get_or_insert_with(ConnectProperties::new);
        properties.authentication_method = auth_properties.method;
        properties.authentication_data = auth_properties.data;
    }

    let result = mqtt_connect_inner(options, network, state, connect_properties).await;
    if result.is_err() && auth_exchange_started {
        state.fail_auth_exchange_due_to_connection_closed();
    }
    result
}

async fn mqtt_connect_inner(
    options: &mut MqttOptions,
    network: &mut Network,
    state: &mut MqttState,
    connect_properties: Option<ConnectProperties>,
) -> Result<ConnAck, ConnectionError> {
    let sent_client_id = options.client_id();

    // send mqtt connect packet
    network
        .write(Packet::Connect(
            Connect {
                client_id: sent_client_id.clone(),
                keep_alive: u16::try_from(options.keep_alive().as_secs()).unwrap_or(u16::MAX),
                clean_start: options.clean_start(),
                properties: connect_properties,
            },
            options.last_will(),
            options.auth().clone(),
        ))
        .await?;
    network.flush().await?;

    // validate connack
    loop {
        match network.read().await? {
            Incoming::ConnAck(connack) => {
                if let Err(err) = validate_connack_session_present_for_reason_code(&connack) {
                    send_protocol_error_disconnect(network).await;
                    return Err(err.into());
                }

                if let Err(err) = validate_connack_response_information(options, &connack) {
                    if connack.code == ConnectReturnCode::Success {
                        send_protocol_error_disconnect(network).await;
                    }
                    return Err(err.into());
                }

                if connack.code != ConnectReturnCode::Success {
                    return Err(ConnectionError::ConnectionRefused(connack.code));
                }

                if let Err(err) = state.validate_successful_connack_authentication_method(&connack)
                {
                    send_protocol_error_disconnect(network).await;
                    return Err(err.into());
                }

                if let Some(props) = &connack.properties
                    && let Some(keep_alive) = props.server_keep_alive
                {
                    options.keep_alive = Duration::from_secs(u64::from(keep_alive));
                }

                if let Some(props) = &connack.properties {
                    match (&props.assigned_client_identifier, sent_client_id.is_empty()) {
                        (Some(assigned_client_identifier), true)
                            if !assigned_client_identifier.is_empty() =>
                        {
                            options.set_client_id(assigned_client_identifier.clone());
                        }
                        (Some(_) | None, true) | (Some(_), false) => {
                            send_protocol_error_disconnect(network).await;
                            return Err(StateError::Deserialization(
                                super::mqttbytes::Error::ProtocolError,
                            )
                            .into());
                        }
                        (None, false) => {}
                    }

                    network.set_max_outgoing_size(props.max_packet_size);

                    // Override local session_expiry_interval value if set by server.
                    if props.session_expiry_interval.is_some() {
                        options.set_session_expiry_interval(props.session_expiry_interval);
                    }
                } else if sent_client_id.is_empty() {
                    send_protocol_error_disconnect(network).await;
                    return Err(StateError::Deserialization(
                        super::mqttbytes::Error::ProtocolError,
                    )
                    .into());
                }
                return Ok(connack);
            }
            Incoming::Auth(auth) => match state.handle_incoming_packet(Incoming::Auth(auth)) {
                Ok(Some(outgoing)) => {
                    network.write(outgoing).await?;
                    network.flush().await?;
                }
                Ok(None) => return Err(ConnectionError::AuthProcessingError),
                Err(err @ StateError::Deserialization(super::mqttbytes::Error::ProtocolError)) => {
                    send_protocol_error_disconnect(network).await;
                    return Err(err.into());
                }
                Err(err) => return Err(err.into()),
            },
            packet => return Err(ConnectionError::NotConnAck(Box::new(packet))),
        }
    }
}

fn validate_connack_response_information(
    options: &MqttOptions,
    connack: &ConnAck,
) -> Result<(), StateError> {
    // [MQTT-3.1.2-28] If Request Response Information was 0 (or absent, which
    // defaults to 0), the Server MUST NOT return Response Information in the
    // CONNACK.
    let request_response_info = options.request_response_info().unwrap_or(0);
    if request_response_info == 0
        && let Some(props) = &connack.properties
        && props.response_information.is_some()
    {
        return Err(StateError::Deserialization(
            super::mqttbytes::Error::ProtocolError,
        ));
    }

    Ok(())
}

fn validate_connack_session_present_for_reason_code(connack: &ConnAck) -> Result<(), StateError> {
    if connack.code != ConnectReturnCode::Success && connack.session_present {
        return Err(StateError::Deserialization(
            super::mqttbytes::Error::ProtocolError,
        ));
    }

    Ok(())
}

async fn send_protocol_error_disconnect(network: &mut Network) {
    if network
        .write(Packet::Disconnect(Disconnect::new(
            DisconnectReasonCode::ProtocolError,
        )))
        .await
        .is_ok()
    {
        let _ = network.flush().await;
    }
}

const fn should_replay_after_reconnect(request: &Request) -> bool {
    !matches!(request, Request::PubAck(_) | Request::PubRec(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AckMode;
    use crate::mqttbytes::{Error as MqttError, QoS};
    use crate::{Auth, AuthProperties, AuthReasonCode};
    use crate::{ConnAckProperties, Filter, PubAck, PubComp, PubRec, PubRel, PublishProperties};
    use bytes::{Bytes, BytesMut};
    use flume::TryRecvError;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    #[derive(Debug)]
    struct StaticAuthManager {
        response: Result<Option<AuthProperties>, String>,
    }

    impl crate::Authenticator for StaticAuthManager {
        fn start(
            &mut self,
            _context: crate::AuthContext<'_>,
        ) -> Result<Option<AuthProperties>, crate::AuthError> {
            Ok(None)
        }

        fn continue_auth(
            &mut self,
            _context: crate::AuthContext<'_>,
            _auth_prop: Option<AuthProperties>,
        ) -> Result<crate::AuthAction, crate::AuthError> {
            self.response
                .clone()
                .map(|props| props.map_or(crate::AuthAction::Complete, crate::AuthAction::Send))
                .map_err(crate::AuthError::from)
        }

        fn success(
            &mut self,
            _context: crate::AuthContext<'_>,
            _incoming: Option<AuthProperties>,
        ) -> Result<(), crate::AuthError> {
            Ok(())
        }

        fn failure(&mut self, _context: crate::AuthContext<'_>, _error: crate::AuthError) {}
    }

    fn build_connack_with_receive_max(receive_max: u16) -> ConnAck {
        ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: Some(ConnAckProperties {
                session_expiry_interval: None,
                receive_max: Some(receive_max),
                max_qos: None,
                retain_available: None,
                max_packet_size: None,
                assigned_client_identifier: None,
                topic_alias_max: None,
                reason_string: None,
                user_properties: vec![],
                wildcard_subscription_available: None,
                subscription_identifiers_available: None,
                shared_subscription_available: None,
                server_keep_alive: None,
                response_information: None,
                server_reference: None,
                authentication_method: None,
                authentication_data: None,
            }),
        }
    }

    fn build_connack_with_authentication_method(authentication_method: Option<&str>) -> ConnAck {
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().authentication_method =
            authentication_method.map(str::to_owned);
        connack
    }

    fn mqtt5_non_success_connect_return_codes() -> [ConnectReturnCode; 21] {
        [
            ConnectReturnCode::UnspecifiedError,
            ConnectReturnCode::MalformedPacket,
            ConnectReturnCode::ProtocolError,
            ConnectReturnCode::ImplementationSpecificError,
            ConnectReturnCode::UnsupportedProtocolVersion,
            ConnectReturnCode::ClientIdentifierNotValid,
            ConnectReturnCode::BadUserNamePassword,
            ConnectReturnCode::NotAuthorized,
            ConnectReturnCode::ServerUnavailable,
            ConnectReturnCode::ServerBusy,
            ConnectReturnCode::Banned,
            ConnectReturnCode::BadAuthenticationMethod,
            ConnectReturnCode::TopicNameInvalid,
            ConnectReturnCode::PacketTooLarge,
            ConnectReturnCode::QuotaExceeded,
            ConnectReturnCode::PayloadFormatInvalid,
            ConnectReturnCode::RetainNotSupported,
            ConnectReturnCode::QoSNotSupported,
            ConnectReturnCode::UseAnotherServer,
            ConnectReturnCode::ServerMoved,
            ConnectReturnCode::ConnectionRateExceeded,
        ]
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
        mut options: MqttOptions,
        connack: ConnAck,
    ) -> (Result<ConnAck, ConnectionError>, Vec<u8>) {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            read_packet_bytes(&mut peer).await
        };

        tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker)
    }

    async fn run_mqtt_connect_with_connack_and_return_state(
        mut options: MqttOptions,
        connack: ConnAck,
    ) -> (Result<ConnAck, ConnectionError>, MqttState) {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        };

        let (result, ()) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        (result, state)
    }

    async fn run_mqtt_connect_with_connack_then_close(
        mut options: MqttOptions,
        connack: ConnAck,
    ) -> Result<ConnAck, ConnectionError> {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        };

        let (result, ()) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        result
    }

    async fn run_mqtt_connect_with_stale_state_auth_method(
        mut options: MqttOptions,
        stale_authentication_method: Option<&str>,
        incoming: Vec<Packet>,
    ) -> Result<ConnAck, ConnectionError> {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            stale_authentication_method.map(str::to_owned),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            for packet in incoming {
                let mut encoded = BytesMut::new();
                packet.write(&mut encoded, None).unwrap();
                peer.write_all(&encoded).await.unwrap();
            }
        };

        let (result, ()) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        result
    }

    async fn run_successful_mqtt_connect_with_connack(
        mut options: MqttOptions,
        connack: ConnAck,
    ) -> Result<ConnAck, ConnectionError> {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        };

        let (result, ()) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        result
    }

    async fn run_successful_mqtt_connect_with_connack_and_return_options(
        mut options: MqttOptions,
        connack: ConnAck,
    ) -> (Result<ConnAck, ConnectionError>, MqttOptions) {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        };

        let (result, ()) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        (result, options)
    }

    fn push_pending(eventloop: &mut EventLoop, request: Request) {
        eventloop.pending.push_back(RequestEnvelope::plain(request));
    }

    fn pending_front_request(eventloop: &EventLoop) -> Option<&Request> {
        eventloop.pending.front().map(|envelope| &envelope.request)
    }

    #[tokio::test]
    async fn graceful_disconnect_fails_active_tracked_reauth_notice() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let (mut eventloop, _) = EventLoop::new_for_async_client(options, 10);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        let (notice_tx, notice) = AuthNoticeTx::new();
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();

        assert!(
            eventloop
                .handle_request(
                    Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                    Some(TrackedNoticeTx::Auth(notice_tx)),
                    &mut should_flush,
                    &mut qos0_notices,
                )
                .await
                .unwrap()
        );
        assert!(eventloop.state.outbound_requests_drained());

        let handled = eventloop
            .handle_request(
                Request::Disconnect(Disconnect::new(DisconnectReasonCode::NormalDisconnection)),
                None,
                &mut should_flush,
                &mut qos0_notices,
            )
            .await
            .unwrap();

        assert!(!handled);
        assert!(eventloop.pending_disconnect.is_some());
        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            crate::notice::AuthNoticeError::ConnectionClosed
        );
        assert!(eventloop.state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::Reauthentication,
                    reason: crate::AuthFailureReason::ConnectionClosed,
                    ..
                })
            )
        }));
    }

    #[tokio::test]
    async fn mqtt_connect_accepts_matching_connack_authentication_method() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let connack = build_connack_with_authentication_method(Some("test-method"));

        let result = run_successful_mqtt_connect_with_connack(options, connack)
            .await
            .unwrap();

        assert_eq!(
            result.properties.unwrap().authentication_method.as_deref(),
            Some("test-method")
        );
    }

    #[tokio::test]
    async fn mqtt_connect_accepts_connack_without_authentication_method_when_connect_omits_it() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let result = run_successful_mqtt_connect_with_connack(options, connack)
            .await
            .unwrap();

        assert!(result.properties.is_none());
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_unexpected_packet_before_connack() {
        let options = MqttOptions::new("test-client", "localhost");

        let result = run_mqtt_connect_with_stale_state_auth_method(
            options,
            None,
            vec![Packet::PingReq(super::super::mqttbytes::v5::PingReq)],
        )
        .await;

        assert!(matches!(
            result,
            Err(ConnectionError::NotConnAck(packet))
                if matches!(
                    &*packet,
                    Packet::PingReq(super::super::mqttbytes::v5::PingReq)
                )
        ));
    }

    #[tokio::test]
    async fn mqtt_connect_adopts_assigned_client_identifier() {
        let options = MqttOptions::new("", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack
            .properties
            .as_mut()
            .unwrap()
            .assigned_client_identifier = Some("server-assigned-client".to_owned());

        let (result, options) =
            run_successful_mqtt_connect_with_connack_and_return_options(options, connack).await;

        result.unwrap();
        assert_eq!(options.client_id(), "server-assigned-client");
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_assigned_client_identifier_for_explicit_client_id() {
        let options = MqttOptions::new("explicit-client", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack
            .properties
            .as_mut()
            .unwrap()
            .assigned_client_identifier = Some("server-assigned-client".to_owned());

        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );
        let broker = async {
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            read_packet_bytes(&mut peer).await
        };

        let mut options = options;
        let (result, disconnect_bytes) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(options.client_id(), "explicit-client");
        assert_eq!(
            disconnect_bytes,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_missing_assigned_client_identifier_for_empty_client_id() {
        let options = MqttOptions::new("", "localhost");
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_properties_without_assigned_client_identifier_for_empty_client_id()
     {
        let options = MqttOptions::new("", "localhost");
        let connack = build_connack_with_receive_max(10);

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_empty_assigned_client_identifier_for_empty_client_id() {
        let options = MqttOptions::new("", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack
            .properties
            .as_mut()
            .unwrap()
            .assigned_client_identifier = Some(String::new());

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_returns_client_identifier_not_valid_rejection() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::ClientIdentifierNotValid,
            properties: None,
        };

        let result = run_successful_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::ConnectionRefused(
                ConnectReturnCode::ClientIdentifierNotValid
            ))
        ));
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_refused_connack_with_session_present() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck {
            session_present: true,
            code: ConnectReturnCode::BadUserNamePassword,
            properties: None,
        };

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[test]
    fn refused_connack_validation_rejects_session_present_for_every_non_success_reason_code() {
        for code in mqtt5_non_success_connect_return_codes() {
            let connack = ConnAck {
                session_present: true,
                code,
                properties: None,
            };

            let err = validate_connack_session_present_for_reason_code(&connack).unwrap_err();

            assert!(
                matches!(err, StateError::Deserialization(MqttError::ProtocolError)),
                "reason code {code:?} returned {err:?}"
            );
        }
    }

    #[test]
    fn refused_connack_validation_accepts_session_absent_for_every_non_success_reason_code() {
        for code in mqtt5_non_success_connect_return_codes() {
            let connack = ConnAck {
                session_present: false,
                code,
                properties: None,
            };

            validate_connack_session_present_for_reason_code(&connack)
                .unwrap_or_else(|err| panic!("reason code {code:?} returned {err:?}"));
        }
    }

    #[tokio::test]
    async fn refused_connack_with_session_present_preserves_protocol_error_after_peer_close() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck {
            session_present: true,
            code: ConnectReturnCode::BadUserNamePassword,
            properties: None,
        };

        let result = run_mqtt_connect_with_connack_then_close(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
    }

    #[tokio::test]
    async fn mqtt_connect_reuses_assigned_client_identifier_on_reconnect() {
        let options = MqttOptions::new("", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack
            .properties
            .as_mut()
            .unwrap()
            .assigned_client_identifier = Some("server-assigned-client".to_owned());
        let (result, mut options) =
            run_successful_mqtt_connect_with_connack_and_return_options(options, connack).await;
        result.unwrap();

        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let broker = async {
            let connect_bytes = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            connect_bytes
        };

        let (result, connect_bytes) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        result.unwrap();
        let mut stream = BytesMut::from(&connect_bytes[..]);
        let packet = Packet::read(&mut stream, Some(1024)).unwrap();

        match packet {
            Packet::Connect(connect, _, _) => {
                assert_eq!(connect.client_id, "server-assigned-client");
            }
            packet => panic!("expected CONNECT packet, got {packet:?}"),
        }
    }

    #[tokio::test]
    async fn mqtt_connect_refreshes_state_authentication_method_from_mutated_options() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("new-method".to_owned()));
        options.set_auth_manager(Arc::new(Mutex::new(StaticAuthManager {
            response: Ok(Some(AuthProperties {
                method: Some("new-method".to_owned()),
                data: None,
                reason: None,
                user_properties: vec![],
            })),
        })));
        let auth = Auth::new(
            AuthReasonCode::Continue,
            Some(AuthProperties {
                method: Some("new-method".to_owned()),
                data: None,
                reason: None,
                user_properties: vec![],
            }),
        );
        let connack = build_connack_with_authentication_method(Some("new-method"));

        let result = run_mqtt_connect_with_stale_state_auth_method(
            options,
            Some("old-method"),
            vec![Packet::Auth(auth), Packet::ConnAck(connack)],
        )
        .await
        .unwrap();

        assert_eq!(
            result.properties.unwrap().authentication_method.as_deref(),
            Some("new-method")
        );
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_missing_connack_authentication_method() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_failure_resets_initial_auth_exchange() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::BadAuthenticationMethod,
            properties: None,
        };

        let (result, state) =
            run_mqtt_connect_with_connack_and_return_state(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::ConnectionRefused(
                ConnectReturnCode::BadAuthenticationMethod
            ))
        ));
        assert!(state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    reason: crate::AuthFailureReason::ConnectionClosed,
                    ..
                })
            )
        }));
    }

    #[tokio::test]
    async fn mqtt_connect_protocol_error_resets_initial_auth_exchange() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let (result, state) =
            run_mqtt_connect_with_connack_and_return_state(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert!(state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    reason: crate::AuthFailureReason::ConnectionClosed,
                    ..
                })
            )
        }));
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_mismatched_connack_authentication_method() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let connack = build_connack_with_authentication_method(Some("other-method"));

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    #[tokio::test]
    async fn mqtt_connect_rejects_connack_authentication_method_when_connect_omits_it() {
        let options = MqttOptions::new("test-client", "localhost");
        let connack = build_connack_with_authentication_method(Some("test-method"));

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    /// [MQTT-3.1.2-28] Server MUST NOT return Response Information when
    /// Request Response Information is absent (defaults to 0).
    #[tokio::test]
    async fn mqtt_connect_rejects_connack_response_information_when_request_response_info_is_absent()
     {
        let options = MqttOptions::new("test-client", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().response_information =
            Some("response-data".to_owned());

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    /// [MQTT-3.1.2-28] Server MUST NOT return Response Information when
    /// Request Response Information is 0.
    #[tokio::test]
    async fn mqtt_connect_rejects_connack_response_information_when_request_response_info_is_zero()
    {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_request_response_info(Some(0));
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().response_information =
            Some("response-data".to_owned());

        let (result, disconnect) = run_mqtt_connect_with_connack(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
        assert_eq!(
            disconnect,
            vec![0xE0, 0x02, DisconnectReasonCode::ProtocolError as u8, 0x00]
        );
    }

    /// [MQTT-3.1.2-28] Server MUST NOT return Response Information in any
    /// CONNACK when Request Response Information is absent (defaults to 0).
    #[tokio::test]
    async fn refused_connack_rejects_response_information_when_request_response_info_is_absent() {
        let options = MqttOptions::new("test-client", "localhost");
        let mut connack = build_connack_with_receive_max(10);
        connack.code = ConnectReturnCode::BadUserNamePassword;
        connack.properties.as_mut().unwrap().response_information =
            Some("response-data".to_owned());

        let result = run_mqtt_connect_with_connack_then_close(options, connack).await;

        assert!(matches!(
            result,
            Err(ConnectionError::MqttState(StateError::Deserialization(
                MqttError::ProtocolError
            )))
        ));
    }

    /// [MQTT-3.1.2-28] Server MAY return Response Information when
    /// Request Response Information is 1.
    #[tokio::test]
    async fn mqtt_connect_accepts_connack_response_information_when_request_response_info_is_one() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_request_response_info(Some(1));
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().response_information =
            Some("response-data".to_owned());

        let result = run_successful_mqtt_connect_with_connack(options, connack)
            .await
            .unwrap();

        assert_eq!(
            result.properties.unwrap().response_information.as_deref(),
            Some("response-data")
        );
    }

    fn build_eventloop(clean_start: bool) -> EventLoop {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_start(clean_start);

        let (eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop
    }

    fn build_eventloop_with_pending(clean_start: bool) -> EventLoop {
        let mut eventloop = build_eventloop(clean_start);
        push_pending(&mut eventloop, Request::PingReq);
        eventloop
    }

    fn publish(qos: QoS) -> Publish {
        Publish::new("hello/world", qos, "payload", None)
    }

    fn subscribe() -> Subscribe {
        Subscribe::new(Filter::new("hello/world", QoS::AtMostOnce), None)
    }

    fn fill_publish_window(eventloop: &mut EventLoop) {
        let mut active = publish(QoS::AtLeastOnce);
        active.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(active);
        eventloop.state.inflight = 1;
    }

    fn next_after_blocked_publish(request: Request) -> Option<Request> {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_outgoing_inflight_upper_limit(1);
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
    fn scheduler_sends_control_after_receive_max_blocked_publish() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_outgoing_inflight_upper_limit(1);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Publish(publish(
                QoS::AtLeastOnce,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Subscribe(subscribe())));

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
        options.set_outgoing_inflight_upper_limit(1);
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
            next_after_blocked_publish(Request::PubAck(PubAck::new(7, None))),
            Some(Request::PubAck(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubRec(PubRec::new(8, None))),
            Some(Request::PubRec(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubRel(PubRel::new(9, None))),
            Some(Request::PubRel(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PubComp(PubComp::new(10, None))),
            Some(Request::PubComp(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::PingReq),
            Some(Request::PingReq)
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Subscribe(subscribe())),
            Some(Request::Subscribe(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Unsubscribe(Unsubscribe::new("hello/world", None))),
            Some(Request::Unsubscribe(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Auth(Auth::new(
                AuthReasonCode::ReAuthenticate,
                None
            ))),
            Some(Request::Auth(_))
        ));
        assert!(matches!(
            next_after_blocked_publish(Request::Disconnect(Disconnect::new(
                DisconnectReasonCode::NormalDisconnection
            ))),
            Some(Request::Disconnect(_))
        ));
    }

    #[test]
    fn scheduler_unsubscribe_after_blocked_publish_uses_non_conflicting_packet_id() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_outgoing_inflight_upper_limit(1);
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
                Unsubscribe::new("hello/world", None),
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
            .push_back(RequestEnvelope::plain(Request::Subscribe(subscribe())));

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
        let mut network = Network::new(client, Some(1024));
        network.set_max_outgoing_size(Some(1024));
        eventloop.network = Some(network);

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
            .send_async(RequestEnvelope::plain(Request::Disconnect(
                Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            )))
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

    fn publish_properties_with_alias(alias: u16) -> PublishProperties {
        PublishProperties {
            topic_alias: Some(alias),
            ..Default::default()
        }
    }

    fn publish_with_alias(topic: &str, qos: QoS, alias: u16) -> Publish {
        Publish::new(
            topic,
            qos,
            "payload",
            Some(publish_properties_with_alias(alias)),
        )
    }

    #[tokio::test]
    async fn poll_closes_connection_after_incoming_topic_alias_exceeds_client_max() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_topic_alias_max(Some(5));
        let mut eventloop = EventLoop::new(options, 1);
        let (client, mut peer) = tokio::io::duplex(64);
        eventloop.network = Some(Network::new(client, Some(1024)));

        peer.write_all(&[0x30, 0x07, 0x00, 0x01, b'a', 0x03, 0x23, 0x00, 0x06])
            .await
            .unwrap();

        let err = time::timeout(Duration::from_secs(1), eventloop.poll())
            .await
            .expect("poll should return after protocol-error disconnect")
            .unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(MqttError::ProtocolViolation(
                DisconnectReasonCode::TopicAliasInvalid
            )))
        ));
        assert!(eventloop.network.is_none());
        assert!(eventloop.state.events.is_empty());

        let mut response = [0; 4];
        peer.read_exact(&mut response).await.unwrap();
        assert_eq!(
            response,
            [
                0xE0,
                0x02,
                DisconnectReasonCode::TopicAliasInvalid as u8,
                0x00
            ]
        );
    }

    #[tokio::test]
    async fn poll_surfaces_incoming_session_taken_over_disconnect() {
        let options = MqttOptions::new("test-client", "localhost");
        let mut eventloop = EventLoop::new(options, 1);
        let (client, mut peer) = tokio::io::duplex(64);
        eventloop.network = Some(Network::new(client, Some(1024)));

        peer.write_all(&[
            0xE0,
            0x02,
            DisconnectReasonCode::SessionTakenOver as u8,
            0x00,
        ])
        .await
        .unwrap();

        let err = time::timeout(Duration::from_secs(1), eventloop.poll())
            .await
            .expect("poll should return after inbound disconnect")
            .unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::ServerDisconnect {
                reason_code: DisconnectReasonCode::SessionTakenOver,
                reason_string: None,
            })
        ));
        assert!(eventloop.network.is_none());
        assert!(eventloop.state.events.is_empty());
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
            .send(RequestEnvelope::plain(Request::PubAck(PubAck::new(
                7, None,
            ))))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::PubRec(PubRec::new(
                8, None,
            ))))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq)
        ));
    }

    #[test]
    fn clean_drops_ack_requests_drained_from_queued_scheduler() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 3);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PubAck(PubAck::new(
                7, None,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PubRec(PubRec::new(
                8, None,
            ))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PingReq));

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq)
        ));
    }

    #[test]
    fn clean_rewrites_alias_only_pending_publish_when_mapping_is_known() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.state.set_broker_topic_alias_max(10);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish_with_alias(
                "hello/replay",
                QoS::AtMostOnce,
                4,
            )))
            .unwrap();

        request_tx
            .send(RequestEnvelope::plain(Request::Publish(
                publish_with_alias("", QoS::AtLeastOnce, 4),
            )))
            .unwrap();

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 1);
        match pending_front_request(&eventloop) {
            Some(Request::Publish(publish)) => {
                assert_eq!(publish.topic, Bytes::from_static(b"hello/replay"));
                assert_eq!(
                    publish
                        .properties
                        .as_ref()
                        .and_then(|props| props.topic_alias),
                    None
                );
            }
            request => panic!("expected replay publish, got {request:?}"),
        }
        assert_eq!(eventloop.state.broker_topic_alias_max(), 0);
    }

    #[test]
    fn clean_uses_earlier_drained_publish_to_rewrite_later_alias_only_publish() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);

        request_tx
            .send(RequestEnvelope::plain(Request::Publish(
                publish_with_alias("fresh/topic", QoS::AtLeastOnce, 1),
            )))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::Publish(
                publish_with_alias("", QoS::AtLeastOnce, 1),
            )))
            .unwrap();

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 2);
        assert!(matches!(
            eventloop.pending.pop_front().map(|envelope| envelope.request),
            Some(Request::Publish(publish)) if publish.topic == Bytes::from_static(b"fresh/topic")
        ));
        assert!(matches!(
            eventloop.pending.pop_front().map(|envelope| envelope.request),
            Some(Request::Publish(publish)) if publish.topic == Bytes::from_static(b"fresh/topic")
        ));
    }

    #[test]
    fn clean_prefers_earlier_replay_alias_mapping_over_stale_previous_mapping() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);
        eventloop.state.set_broker_topic_alias_max(10);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish_with_alias(
                "stale/topic",
                QoS::AtMostOnce,
                1,
            )))
            .unwrap();

        request_tx
            .send(RequestEnvelope::plain(Request::Publish(
                publish_with_alias("fresh/topic", QoS::AtLeastOnce, 1),
            )))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::Publish(
                publish_with_alias("", QoS::AtLeastOnce, 1),
            )))
            .unwrap();

        eventloop.clean();

        assert_eq!(eventloop.pending_len(), 2);
        _ = eventloop.pending.pop_front();
        assert!(matches!(
            eventloop.pending.pop_front().map(|envelope| envelope.request),
            Some(Request::Publish(publish)) if publish.topic == Bytes::from_static(b"fresh/topic")
        ));
    }

    #[test]
    fn clean_fails_tracked_alias_only_publish_when_mapping_is_unknown() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        let (notice_tx, notice) = PublishNoticeTx::new();

        request_tx
            .send(RequestEnvelope::tracked_publish(
                publish_with_alias("", QoS::AtLeastOnce, 5),
                notice_tx,
            ))
            .unwrap();

        eventloop.clean();

        assert!(eventloop.pending_is_empty());
        assert_eq!(
            notice.wait().unwrap_err(),
            PublishNoticeError::TopicAliasReplayUnavailable(5)
        );
    }

    #[test]
    fn clean_preserves_surviving_pending_order_when_alias_replay_is_filtered() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);
        push_pending(&mut eventloop, Request::PingReq);
        let (notice_tx, notice) = PublishNoticeTx::new();
        request_tx
            .send(RequestEnvelope::tracked_publish(
                publish_with_alias("", QoS::AtLeastOnce, 6),
                notice_tx,
            ))
            .unwrap();
        request_tx
            .send(RequestEnvelope::plain(Request::PingResp))
            .unwrap();

        eventloop.clean();

        assert_eq!(
            notice.wait().unwrap_err(),
            PublishNoticeError::TopicAliasReplayUnavailable(6)
        );
        assert_eq!(eventloop.pending_len(), 2);
        assert!(matches!(
            eventloop
                .pending
                .pop_front()
                .map(|envelope| envelope.request),
            Some(Request::PingReq)
        ));
        assert!(matches!(
            eventloop
                .pending
                .pop_front()
                .map(|envelope| envelope.request),
            Some(Request::PingResp)
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn network_connect_rejects_unix_broker_with_tcp_transport() {
        let mut options = MqttOptions::new("test-client", crate::Broker::unix("/tmp/mqtt.sock"));
        options.set_transport(Transport::tcp());

        match network_connect(&options).await {
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

        match network_connect(&options).await {
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

        match network_connect(&options).await {
            Err(ConnectionError::BrokerTransportMismatch) => {}
            Err(err) => panic!("unexpected error: {err:?}"),
            Ok(_) => panic!("mismatched broker and transport should fail"),
        }
    }

    #[test]
    fn connack_resize_skips_shrink_until_pending_retransmit_queue_is_empty() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_outgoing_inflight_upper_limit(10);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );
        publish.pkid = 8;
        push_pending(&mut eventloop, Request::Publish(publish));

        eventloop
            .state
            .handle_incoming_packet(Incoming::ConnAck(build_connack_with_receive_max(3)))
            .unwrap();

        eventloop.reconcile_outgoing_tracking_after_connack();
        assert_eq!(eventloop.state.outgoing_pub.len(), 11);

        eventloop.pending.clear();
        eventloop.reconcile_outgoing_tracking_after_connack();
        assert_eq!(eventloop.state.outgoing_pub.len(), 4);
        assert_eq!(eventloop.state.outgoing_pub_notice.len(), 4);
        assert_eq!(eventloop.state.outgoing_rel_notice.len(), 4);
    }

    #[tokio::test]
    async fn async_client_path_reports_requests_done_after_pending_drain() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        push_pending(&mut eventloop, Request::PingReq);
        drop(request_tx);

        let request = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(request, (Request::PingReq, None)));

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
        push_pending(&mut eventloop, Request::PingReq);

        let delayed = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::from_millis(50),
        );
        let timed_out = time::timeout(Duration::from_millis(5), delayed).await;

        assert!(timed_out.is_err());
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingReq)
        ));
    }

    #[tokio::test]
    async fn try_next_request_applies_pending_throttle_for_followup_pending_item() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 2);
        push_pending(&mut eventloop, Request::PingReq);
        push_pending(&mut eventloop, Request::PingResp);

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq, None)));

        let delayed = EventLoop::try_next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::from_millis(50),
        );
        let timed_out = time::timeout(Duration::from_millis(5), delayed).await;

        assert!(timed_out.is_err());
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::PingResp)
        ));
    }

    #[tokio::test]
    async fn try_next_request_does_not_throttle_when_pending_queue_is_empty() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq))
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

        assert!(matches!(received, Some((Request::PingReq, None))));
    }

    #[tokio::test]
    async fn next_request_prioritizes_pending_over_channel_messages() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 2);
        push_pending(&mut eventloop, Request::PingReq);
        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq))
            .await
            .unwrap();

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq, None)));
        assert!(eventloop.pending.is_empty());

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(second, (Request::PingReq, None)));
    }

    #[tokio::test]
    async fn next_request_preserves_fifo_order_for_plain_and_tracked_requests() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (notice_tx, _notice) = PublishNoticeTx::new();
        let tracked_publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );

        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq))
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
            .send_async(RequestEnvelope::plain(Request::PingResp))
            .await
            .unwrap();

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first, (Request::PingReq, None)));

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
        assert!(matches!(third, (Request::PingResp, None)));
    }

    #[tokio::test]
    async fn tracked_qos0_notice_reports_not_flushed_on_first_write_failure() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (client, _peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        network.set_max_outgoing_size(Some(16));
        eventloop.network = Some(network);

        let (notice_tx, notice) = PublishNoticeTx::new();
        let publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtMostOnce,
            vec![1; 128],
            None,
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
        let mut network = Network::new(client, Some(1024));
        network.set_max_outgoing_size(Some(80));
        eventloop.network = Some(network);

        let small_publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtMostOnce,
            vec![1],
            None,
        );
        let large_publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtMostOnce,
            vec![2; 256],
            None,
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
        let publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_publish(publish, notice_tx));
        eventloop
            .pending
            .push_back(RequestEnvelope::plain(Request::PingReq));

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
        let publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );
        eventloop
            .pending
            .push_back(RequestEnvelope::tracked_publish(publish, publish_notice_tx));

        let (request_notice_tx, request_notice) = SubscribeNoticeTx::new();
        let subscribe = Subscribe::new(
            Filter::new("hello/world", crate::mqttbytes::QoS::AtMostOnce),
            None,
        );
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
        let publish = Publish::new(
            "hello/world",
            crate::mqttbytes::QoS::AtLeastOnce,
            "payload",
            None,
        );
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

    #[tokio::test]
    async fn reset_session_state_fails_active_tracked_reauth_notice() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (notice_tx, notice) = AuthNoticeTx::new();

        eventloop
            .state
            .handle_outgoing_packet_with_notice(
                Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                Some(TrackedNoticeTx::Auth(notice_tx)),
            )
            .unwrap();

        eventloop.reset_session_state();

        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            crate::notice::AuthNoticeError::SessionReset
        );
        assert!(eventloop.state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::Reauthentication,
                    reason: crate::AuthFailureReason::SessionReset,
                    ..
                })
            )
        }));
        eventloop
            .state
            .handle_outgoing_packet(Request::Auth(Auth::new(
                AuthReasonCode::ReAuthenticate,
                None,
            )))
            .unwrap();
    }

    #[tokio::test]
    async fn reset_session_state_does_not_fail_initial_auth_exchange() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some("test-method".to_owned()));
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop
            .state
            .begin_authentication_connect(Some("test-method".to_owned()))
            .unwrap();
        eventloop.reset_session_state();

        assert!(!eventloop.state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Failed {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    ..
                })
            )
        }));
        eventloop
            .state
            .handle_incoming_packet(Incoming::ConnAck(build_connack_with_authentication_method(
                Some("test-method"),
            )))
            .unwrap();
        assert!(eventloop.state.events.iter().any(|event| {
            matches!(
                event,
                Event::Auth(crate::AuthEvent::Succeeded {
                    kind: crate::AuthExchangeKind::InitialConnect,
                    ..
                })
            )
        }));
    }

    #[test]
    fn connack_reconcile_rejects_clean_start_with_session_present() {
        let mut eventloop = build_eventloop_with_pending(true);
        eventloop.session_client_id = Some("test-client".to_owned());

        let err = eventloop.reconcile_connack_session(true).unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::SessionStateMismatch {
                clean_start: true,
                session_present: true
            }
        ));
        assert_eq!(eventloop.pending_len(), 1);
    }

    #[test]
    fn connack_reconcile_rejects_session_present_without_local_state() {
        let mut eventloop = build_eventloop(false);

        let err = eventloop.reconcile_connack_session(true).unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::SessionStateMismatch {
                clean_start: false,
                session_present: true
            }
        ));
    }

    #[test]
    fn connack_reconcile_allows_broker_session_present_without_local_state_when_opted_in() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_allow_broker_session_resume_without_local_state(true);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.reconcile_connack_session(true).unwrap();

        assert!(eventloop.pending_is_empty());
        assert!(eventloop.broker_only_session_resume);
        assert_eq!(eventloop.session_client_id, None);
    }

    #[tokio::test]
    async fn broker_only_session_resume_rejects_outbound_packet_id_allocation() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_allow_broker_session_resume_without_local_state(true);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.reconcile_connack_session(true).unwrap();

        let (publish_tx, publish_notice) = PublishNoticeTx::new();
        let (subscribe_tx, subscribe_notice) = SubscribeNoticeTx::new();
        let (unsubscribe_tx, unsubscribe_notice) = UnsubscribeNoticeTx::new();
        let requests = [
            (
                Request::Publish(publish(QoS::AtLeastOnce)),
                Some(TrackedNoticeTx::Publish(publish_tx)),
            ),
            (
                Request::Subscribe(subscribe()),
                Some(TrackedNoticeTx::Subscribe(subscribe_tx)),
            ),
            (
                Request::Unsubscribe(Unsubscribe::new("hello/world", None)),
                Some(TrackedNoticeTx::Unsubscribe(unsubscribe_tx)),
            ),
            (Request::Publish(publish(QoS::ExactlyOnce)), None),
        ];

        for (request, notice) in requests {
            let mut should_flush = false;
            let mut qos0_notices = Vec::new();
            let keep_batching = eventloop
                .handle_request(request, notice, &mut should_flush, &mut qos0_notices)
                .await
                .unwrap();

            assert!(keep_batching);
            assert!(!should_flush);
            assert!(qos0_notices.is_empty());
        }

        assert_eq!(
            publish_notice.wait_async().await.unwrap_err(),
            PublishNoticeError::BrokerOnlySessionResume
        );
        assert_eq!(
            subscribe_notice.wait_async().await.unwrap_err(),
            SubscribeNoticeError::BrokerOnlySessionResume
        );
        assert_eq!(
            unsubscribe_notice.wait_async().await.unwrap_err(),
            UnsubscribeNoticeError::BrokerOnlySessionResume
        );
        assert!(eventloop.broker_only_session_resume);
    }

    #[test]
    fn connack_reconcile_clears_broker_only_resume_on_new_session() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_allow_broker_session_resume_without_local_state(true);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.reconcile_connack_session(true).unwrap();

        eventloop.reconcile_connack_session(false).unwrap();

        assert!(!eventloop.broker_only_session_resume);
    }

    #[test]
    fn connack_reconcile_resets_pending_when_clean_start_gets_new_session() {
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
        eventloop.session_client_id = Some("test-client".to_owned());

        eventloop.reconcile_connack_session(true).unwrap();

        assert_eq!(eventloop.pending_len(), 1);
    }

    #[test]
    fn reset_session_state_clears_local_session_marker() {
        let mut eventloop = build_eventloop(false);
        eventloop.session_client_id = Some("test-client".to_owned());

        eventloop.reset_session_state();

        assert_eq!(eventloop.session_client_id, None);
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
        eventloop.options.set_client_id("other-client".to_owned());

        eventloop.reset_session_state_if_client_id_changed();

        assert!(eventloop.pending_is_empty());
        assert_eq!(eventloop.session_client_id, None);
    }

    // MQTT-3.1.0-1: After a Network Connection is established by a Client to a Server,
    // the first packet sent from the Client to the Server MUST be a CONNECT packet.
    #[tokio::test]
    async fn mqtt_connect_sends_connect_as_first_packet_on_the_wire() {
        let mut options = MqttOptions::new("test-client", "localhost");
        let connack = ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: None,
        };

        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.auto_topic_aliases(),
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );

        let broker = async {
            let connect_bytes = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            connect_bytes
        };

        let (result, connect_bytes) =
            tokio::join!(mqtt_connect(&mut options, &mut network, &mut state), broker);
        result.unwrap();

        // Parse the first packet from the raw bytes written by the client.
        let mut stream = BytesMut::from(&connect_bytes[..]);
        let packet = Packet::read(&mut stream, Some(1024)).unwrap();

        assert!(
            matches!(packet, Packet::Connect(..)),
            "first packet on the wire must be CONNECT, got {packet:?}"
        );
    }

    // MQTT-3.1.0-1: Verify that poll() cannot process user requests before
    // establishing the network connection (which sends CONNECT).
    #[tokio::test]
    async fn poll_sends_connect_before_queued_requests() {
        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let first_packet = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            ConnAck {
                session_present: false,
                code: ConnectReturnCode::Success,
                properties: None,
            }
            .write(&mut encoded_connack)
            .unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
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
        let mut first_stream = BytesMut::from(&first_packet[..]);
        let mut second_stream = BytesMut::from(&second_packet[..]);
        assert!(matches!(
            Packet::read(&mut first_stream, Some(1024)).unwrap(),
            Packet::Connect(..)
        ));
        assert!(matches!(
            Packet::read(&mut second_stream, Some(1024)).unwrap(),
            Packet::PingReq(_)
        ));
    }

    #[tokio::test]
    async fn queued_requests_are_withheld_until_successful_connack() {
        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let first_packet = read_packet_bytes(&mut peer).await;
            let no_second_before_connack =
                time::timeout(Duration::from_millis(50), read_packet_bytes(&mut peer))
                    .await
                    .is_err();

            let mut encoded_connack = BytesMut::new();
            ConnAck {
                session_present: false,
                code: ConnectReturnCode::Success,
                properties: None,
            }
            .write(&mut encoded_connack)
            .unwrap();
            peer.write_all(&encoded_connack).await.unwrap();

            let second_packet = read_packet_bytes(&mut peer).await;
            (first_packet, no_second_before_connack, second_packet)
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        ));

        let (first_packet, no_second_before_connack, second_packet) = broker.await.unwrap();
        let mut first_stream = BytesMut::from(&first_packet[..]);
        let mut second_stream = BytesMut::from(&second_packet[..]);

        assert!(matches!(
            Packet::read(&mut first_stream, Some(1024)).unwrap(),
            Packet::Connect(..)
        ));
        assert!(
            no_second_before_connack,
            "queued request was observable on the wire before CONNACK"
        );
        assert!(matches!(
            Packet::read(&mut second_stream, Some(1024)).unwrap(),
            Packet::PingReq(_)
        ));
    }

    #[tokio::test]
    async fn duplicate_connack_after_connection_establishment_sends_protocol_error_disconnect() {
        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;

            let mut encoded_connack = BytesMut::new();
            ConnAck {
                session_present: false,
                code: ConnectReturnCode::Success,
                properties: None,
            }
            .write(&mut encoded_connack)
            .unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            peer.write_all(&encoded_connack).await.unwrap();

            read_packet_bytes(&mut peer).await
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));

        let err = eventloop.poll().await.unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::ProtocolViolation(
                crate::ProtocolViolation::DuplicateConnAck
            ))
        ));

        let disconnect_bytes = broker.await.unwrap();
        let mut stream = BytesMut::from(&disconnect_bytes[..]);
        assert!(matches!(
            Packet::read(&mut stream, Some(1024)).unwrap(),
            Packet::Disconnect(Disconnect {
                reason_code: DisconnectReasonCode::ProtocolError,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn poll_records_assigned_client_identifier_as_session_client_id() {
        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            let mut connack = build_connack_with_receive_max(10);
            connack
                .properties
                .as_mut()
                .unwrap()
                .assigned_client_identifier = Some("server-assigned-client".to_owned());

            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        broker.await.unwrap();

        assert_eq!(eventloop.options.client_id(), "server-assigned-client");
        assert_eq!(
            eventloop.session_client_id.as_deref(),
            Some("server-assigned-client")
        );
    }

    // MQTT-3.1.0-2: A Client can only send the CONNECT packet once over a Network Connection.
    // The Request enum has no Connect variant, so users cannot submit a CONNECT
    // through the request channel. This is a compile-time invariant.
    #[test]
    fn request_enum_has_no_connect_variant() {
        use crate::mqttbytes::v5::{
            Auth, Filter, SubAck, SubscribeReasonCode, UnsubAck, UnsubAckReason,
        };
        use std::mem::discriminant;

        // Explicitly enumerate the current variants to make the request-channel
        // surface visible in the MQTT-3.1.0-2 regression tests.
        let variants: Vec<_> = [
            discriminant(&Request::Publish(Publish::new(
                "t",
                QoS::AtMostOnce,
                vec![],
                None,
            ))),
            discriminant(&Request::PubAck(PubAck::new(1, None))),
            discriminant(&Request::PubRec(PubRec::new(1, None))),
            discriminant(&Request::PubComp(PubComp::new(1, None))),
            discriminant(&Request::PubRel(PubRel::new(1, None))),
            discriminant(&Request::PingReq),
            discriminant(&Request::PingResp),
            discriminant(&Request::Subscribe(Subscribe::new(
                Filter::new("t", QoS::AtMostOnce),
                None,
            ))),
            discriminant(&Request::SubAck(SubAck {
                pkid: 1,
                return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
                properties: None,
            })),
            discriminant(&Request::Unsubscribe(Unsubscribe::new("t", None))),
            discriminant(&Request::UnsubAck(UnsubAck {
                pkid: 1,
                reasons: vec![UnsubAckReason::Success],
                properties: None,
            })),
            discriminant(&Request::Auth(Auth::new(AuthReasonCode::Success, None))),
            discriminant(&Request::Disconnect(Disconnect::new(
                DisconnectReasonCode::NormalDisconnection,
            ))),
            discriminant(&Request::DisconnectNow(Disconnect::new(
                DisconnectReasonCode::NormalDisconnection,
            ))),
            discriminant(&Request::DisconnectWithTimeout(
                Disconnect::new(DisconnectReasonCode::NormalDisconnection),
                Duration::from_secs(1),
            )),
        ]
        .into_iter()
        .collect();

        // There are exactly 15 documented Request variants.
        assert_eq!(
            variants.len(),
            15,
            "Request should have exactly 15 documented variants"
        );
    }

    // MQTT-3.1.0-2: Verify that after mqtt_connect succeeds, the eventloop
    // never sends another CONNECT on the same network connection. The select()
    // loop only processes Request variants, none of which is Connect.
    #[tokio::test]
    async fn eventloop_sends_only_one_connect_per_network_connection() {
        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let first_packet = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            ConnAck {
                session_present: false,
                code: ConnectReturnCode::Success,
                properties: None,
            }
            .write(&mut encoded_connack)
            .unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            let second_packet = read_packet_bytes(&mut peer).await;
            (first_packet, second_packet)
        });

        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        request_tx
            .send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        ));

        let (first_packet, second_packet) = broker.await.unwrap();
        let mut first_stream = BytesMut::from(&first_packet[..]);
        let mut second_stream = BytesMut::from(&second_packet[..]);
        assert!(matches!(
            Packet::read(&mut first_stream, Some(1024)).unwrap(),
            Packet::Connect(..)
        ));
        assert!(matches!(
            Packet::read(&mut second_stream, Some(1024)).unwrap(),
            Packet::PingReq(_)
        ));
    }
}
