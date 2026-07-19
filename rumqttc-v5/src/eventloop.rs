use super::framed::{Network, ReadBatch, ReadBatchError, ReadBatchOutcome};
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
use crate::session::{PersistedSession, SessionRestoreError, SessionStore, SessionStoreError};
use crate::{AuthEvent, NoticeFailureReason, PublishNoticeError, SubscribeNoticeError};

use flume::{Receiver, Sender, TryRecvError, bounded, unbounded};
use rumqttc_core::{OutboundScheduler, RequestClass, RequestReadiness, ScheduledRequest};
use tokio::select;
use tokio::time::{self, Instant, Sleep, error::Elapsed};

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
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

#[cfg(all(
    any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
    feature = "websocket"
))]
use crate::websockets::split_url_with_default_port;

#[cfg(feature = "proxy")]
use crate::proxy::ProxyError;

#[derive(Debug)]
pub struct RequestEnvelope {
    pub(crate) request: Request,
    pub(crate) notice: Option<TrackedNoticeTx>,
    pub(crate) replay: bool,
}

impl RequestEnvelope {
    const fn from_parts_with_replay(
        request: Request,
        notice: Option<TrackedNoticeTx>,
        replay: bool,
    ) -> Self {
        Self {
            request,
            notice,
            replay,
        }
    }

    pub(crate) const fn plain(request: Request) -> Self {
        Self {
            request,
            notice: None,
            replay: false,
        }
    }

    pub(crate) const fn plain_replay(request: Request) -> Self {
        Self {
            request,
            notice: None,
            replay: true,
        }
    }

    pub(crate) const fn tracked_publish(publish: Publish, notice: PublishNoticeTx) -> Self {
        Self {
            request: Request::Publish(publish),
            notice: Some(TrackedNoticeTx::Publish(notice)),
            replay: false,
        }
    }

    pub(crate) const fn tracked_subscribe(subscribe: Subscribe, notice: SubscribeNoticeTx) -> Self {
        Self {
            request: Request::Subscribe(subscribe),
            notice: Some(TrackedNoticeTx::Subscribe(notice)),
            replay: false,
        }
    }

    pub(crate) const fn tracked_unsubscribe(
        unsubscribe: Unsubscribe,
        notice: UnsubscribeNoticeTx,
    ) -> Self {
        Self {
            request: Request::Unsubscribe(unsubscribe),
            notice: Some(TrackedNoticeTx::Unsubscribe(notice)),
            replay: false,
        }
    }

    pub(crate) const fn tracked_auth(
        auth: super::mqttbytes::v5::Auth,
        notice: AuthNoticeTx,
    ) -> Self {
        Self {
            request: Request::Auth(auth),
            notice: Some(TrackedNoticeTx::Auth(notice)),
            replay: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestChannelCapacity {
    Bounded(usize),
    Unbounded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchControl {
    Continue,
    Stop,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionCheckpointAction {
    Save,
    ApplySessionExpiry(Option<u32>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStoreResetAction {
    Keep,
    Clear,
}

struct SessionSave(
    Option<(
        Arc<dyn SessionStore>,
        crate::SessionStoreKey,
        PersistedSession,
    )>,
);

impl SessionSave {
    async fn save(self) -> Result<bool, ConnectionError> {
        let Some((store, key, session)) = self.0 else {
            return Ok(false);
        };

        store
            .save(&key, &session)
            .await
            .map_err(ConnectionError::SessionStore)?;
        Ok(true)
    }
}

/// Critical errors during eventloop polling
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("Mqtt state: {0}")]
    MqttState(#[from] StateError),
    #[error("Connection timeout")]
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
    #[error("MQTT session store error: {0}")]
    SessionStore(#[source] SessionStoreError),
    #[error("MQTT persisted session restore error: {0}")]
    SessionRestore(#[from] SessionRestoreError),
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
    #[error("Websocket response validation error: {0}")]
    ResponseValidation(#[from] crate::websockets::ValidationError),
    #[cfg(feature = "websocket")]
    #[error("Websocket request modifier failed: {0}")]
    RequestModifier(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Tracks the lifecycle of an optional persistent session store within the event loop.
///
/// This groups the flags that control whether the store has been hydrated and whether
/// a deferred clear is still outstanding, keeping the `EventLoop` struct below the
/// Clippy `struct_excessive_bools` threshold.
struct SessionStoreState {
    /// Whether the store has been checked/loaded for this event loop instance.
    loaded: bool,
    /// Whether an explicit local session reset still needs to clear the store.
    clear_pending: bool,
}

impl SessionStoreState {
    const fn new() -> Self {
        Self {
            loaded: false,
            clear_pending: false,
        }
    }
}

/// Observation-only snapshot of an [`EventLoop`].
///
/// The fields are read sequentially while [`EventLoop::diagnostics`] runs. Internal event-loop
/// state cannot change during the call, but request-channel producers can change the channel
/// lengths concurrently, so the channel lengths are observational and do not form a transactional
/// view. A snapshot describes one observation; it is not an event history.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLoopDiagnostics {
    /// Whether an MQTT transport has been established and its initial `CONNACK` accepted.
    ///
    /// This remains `true` while a graceful disconnect is draining and until a transport failure
    /// is detected. It does not probe the broker or guarantee that the transport is still usable.
    pub connected: bool,
    /// Whether a graceful disconnect has been requested and is waiting for outbound work to drain.
    pub disconnecting: bool,
    /// Whether disconnect processing has completed and further polling will return
    /// [`ConnectionError::RequestsDone`].
    pub disconnect_complete: bool,
    /// Request and replay queue depths observed by the event loop.
    pub queues: QueueDiagnostics,
    /// Outbound MQTT protocol state.
    pub outbound: crate::OutboundDiagnostics,
    /// Local persistent-session lifecycle state.
    pub session: SessionDiagnostics,
    /// Configured and effective event-loop batching limits.
    pub config: RuntimeConfigDiagnostics,
}

/// Queue lengths observed by an [`EventLoop`].
///
/// `pending_len` is the sum of `pending_replay_len` and `queued_len`; it must not be added to
/// those component fields. The receiver lengths can change immediately because cloned clients
/// can enqueue requests concurrently.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueDiagnostics {
    /// Requests retained for replay, including work recovered after a disconnect.
    pub pending_replay_len: usize,
    /// Requests admitted by the event loop but not yet passed to the MQTT state machine.
    pub queued_len: usize,
    /// Combined internal pending work: `pending_replay_len + queued_len`.
    pub pending_len: usize,
    /// Requests waiting in the normal client request channel.
    pub requests_rx_len: usize,
    /// Requests waiting in the control-request channel.
    pub control_requests_rx_len: usize,
    /// Immediate-disconnect requests waiting to be observed.
    pub immediate_disconnect_rx_len: usize,
}

/// Session-related event-loop diagnostics.
#[non_exhaustive]
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDiagnostics {
    /// Whether a durable session store is configured.
    pub session_store_configured: bool,
    /// Whether this event loop has loaded a session-store checkpoint.
    pub session_store_loaded: bool,
    /// Whether a failed store-clear operation must succeed before another checkpoint can load.
    pub session_store_clear_pending: bool,
    /// Whether retained local session state belongs to the currently configured client ID.
    ///
    /// This reports identity agreement only; it does not mean the broker resumed the session.
    pub local_session_state_matches_client_id: bool,
    /// Whether this connection resumed broker state without a matching restored local checkpoint.
    pub broker_only_session_resume: bool,
}

/// Runtime batching configuration observed by an [`EventLoop`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfigDiagnostics {
    /// Configured network read batch size. Zero selects adaptive batching.
    pub configured_read_batch_size: usize,
    /// Network read batch size the next read would use for the current connection state.
    pub effective_read_batch_size: usize,
    /// Maximum client requests admitted in one event-loop batch.
    pub max_request_batch: usize,
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
    /// Store key associated with the currently retained local session state.
    session_store_key: Option<crate::SessionStoreKey>,
    /// Session Expiry Interval governing the current or most recently closed connection.
    effective_session_expiry_interval: Option<u32>,
    /// Current connection resumed broker state without matching local packet-id ownership.
    broker_only_session_resume: bool,
    /// Persistent session store lifecycle flags.
    session_store: SessionStoreState,
    pending_disconnect: Option<PendingDisconnect>,
    disconnect_complete: bool,
    #[cfg(feature = "tracing")]
    telemetry: crate::instrumentation::ConnectionTelemetry,
}

/// Events which can be yielded by the event loop
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(clippy::large_enum_variant)]
pub enum Event {
    Incoming(Incoming),
    Outgoing(Outgoing),
    Auth(AuthEvent),
}

impl EventLoop {
    fn has_local_session_state(&self) -> bool {
        self.session_store_key.as_ref() == Some(&self.options.session_store_key())
    }

    fn reconcile_connack_session(&mut self, session_present: bool) -> Result<(), ConnectionError> {
        let clean_start = self.options.clean_start();
        let has_local_state = self.has_local_session_state();
        let missing_local_state = !has_local_state;
        let allow_broker_only_resume = self.options.broker_session_resume_policy()
            == crate::BrokerSessionResumePolicy::AllowBrokerOnly;
        if session_present && (clean_start || (missing_local_state && !allow_broker_only_resume)) {
            return Err(ConnectionError::SessionStateMismatch {
                clean_start,
                session_present,
            });
        }

        if !session_present {
            self.reset_session_state_without_store_invalidation();
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
        let topic_alias_policy = options.topic_alias_policy();
        let client_topic_alias_max = options.topic_alias_max().unwrap_or(0);
        let effective_session_expiry_interval = options.session_expiry_interval();

        let authenticator = options.authenticator();
        let authentication_method = options.authentication_method();

        Self {
            options,
            state: MqttState::new_internal(
                inflight_limit,
                ack_mode,
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
            session_store_key: None,
            effective_session_expiry_interval,
            broker_only_session_resume: false,
            session_store: SessionStoreState::new(),
            pending_disconnect: None,
            disconnect_complete: false,
            #[cfg(feature = "tracing")]
            telemetry: crate::instrumentation::ConnectionTelemetry::default(),
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
        #[cfg(feature = "tracing")]
        self.telemetry.finish_established_connection();
        self.network = None;
        self.keepalive_timeout = None;
        self.pending_disconnect = None;
        let mut replay_topic_aliases = self.state.replay_topic_aliases();

        for clean in self.state.clean_with_notices_for_reconnect() {
            self.push_replay_envelope(
                RequestEnvelope::from_parts_with_replay(clean.request, clean.notice, clean.replay),
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

        #[cfg(feature = "tracing")]
        crate::instrumentation::replay_prepared(
            self.telemetry.connection_generation(),
            &self.diagnostics(),
        );

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
            if envelope.replay {
                self.state.discard_replayed_request(&envelope.request);
            }
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

    /// Returns an observation-only snapshot of event-loop runtime state.
    ///
    /// The snapshot is captured synchronously during this call and does not record state changes
    /// before or after it. Request-channel lengths may change while their fields are read because
    /// client handles can enqueue concurrently. Constructing the nested outbound snapshot scans
    /// protocol tracking vectors and bitsets whose sizes scale with active/configured protocol
    /// state. Incoming publish tracking bitsets span the MQTT packet-identifier range; the
    /// outbound packet-identifier reservation count itself is maintained incrementally.
    ///
    /// Code that owns and drives the event loop can inspect it between polls:
    ///
    /// ```no_run
    /// # use rumqttc::{AsyncClient, ConnectionError, MqttOptions};
    /// # async fn inspect_once() -> Result<(), ConnectionError> {
    /// let options = MqttOptions::new("diagnostics-example", "localhost");
    /// let (_client, mut eventloop) = AsyncClient::builder(options).build();
    ///
    /// let _event = eventloop.poll().await?;
    /// let snapshot = eventloop.diagnostics();
    /// println!("connected={}, inflight={}", snapshot.connected, snapshot.outbound.inflight);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn diagnostics(&self) -> EventLoopDiagnostics {
        let pending_replay_len = self.pending.len();
        let queued_len = self.queued.len();
        EventLoopDiagnostics {
            connected: self.network.is_some(),
            disconnecting: self.pending_disconnect.is_some(),
            disconnect_complete: self.disconnect_complete,
            queues: QueueDiagnostics {
                pending_replay_len,
                queued_len,
                pending_len: pending_replay_len + queued_len,
                requests_rx_len: self.requests_rx.len(),
                control_requests_rx_len: self.control_requests_rx.len(),
                immediate_disconnect_rx_len: self.immediate_disconnect_rx.len(),
            },
            outbound: self.state.outbound_diagnostics(),
            session: SessionDiagnostics {
                session_store_configured: self.options.session_store.is_some(),
                session_store_loaded: self.session_store.loaded,
                session_store_clear_pending: self.session_store.clear_pending,
                local_session_state_matches_client_id: self.session_client_id.as_deref()
                    == Some(self.options.client_id.as_str()),
                broker_only_session_resume: self.broker_only_session_resume,
            },
            config: RuntimeConfigDiagnostics {
                configured_read_batch_size: self.options.read_batch_size(),
                effective_read_batch_size: self.effective_read_batch_size(),
                max_request_batch: self.options.max_request_batch(),
            },
        }
    }

    /// Returns true when all request sources are drained and no more work remains.
    fn all_request_sources_drained(&self) -> bool {
        self.queued.is_empty()
            && self.pending.is_empty()
            && self.pending_disconnect.is_none()
            && self.requests_rx.is_disconnected()
            && self.requests_rx.is_empty()
            && self.control_requests_rx.is_disconnected()
            && self.control_requests_rx.is_empty()
            && self.state.outbound_requests_drained()
    }

    /// Drains pending retransmission queue and fails tracked notices with the given reason.
    ///
    /// Returns the number of pending requests removed from the queue.
    pub fn drain_pending_as_failed(&mut self, reason: NoticeFailureReason) -> usize {
        let mut drained = 0;
        for envelope in self.pending.drain(..) {
            drained += 1;
            if let Some(notice) = envelope.notice {
                Self::fail_tracked_notice(notice, reason);
            }
        }
        for envelope in self.queued.drain() {
            drained += 1;
            if let Some(notice) = envelope.notice {
                Self::fail_tracked_notice(notice, reason);
            }
        }

        drained
    }

    fn fail_tracked_notice(notice: TrackedNoticeTx, reason: NoticeFailureReason) {
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

    fn drop_unprocessed_requests(&mut self) {
        // These requests never became MQTT protocol state. Dropping their notice senders
        // resolves tracked handles with the existing Recv error.
        self.pending.clear();
        self.queued.clear();
        self.requests_rx.drain().for_each(drop);
        self.control_requests_rx.drain().for_each(drop);
    }

    /// Clears eventloop and state tracking bound to a previous session.
    pub fn reset_session_state(&mut self) {
        self.reset_session_state_internal(SessionStoreResetAction::Clear);
    }

    fn reset_session_state_without_store_invalidation(&mut self) {
        self.reset_session_state_internal(SessionStoreResetAction::Keep);
    }

    fn reset_session_state_internal(&mut self, store_reset: SessionStoreResetAction) {
        self.drain_pending_as_failed(NoticeFailureReason::SessionReset);
        self.state.fail_reauth_exchange_due_to_session_reset();
        self.state.reset_session_state();
        self.session_client_id = None;
        self.session_store_key = None;
        self.broker_only_session_resume = false;
        self.session_store.loaded = false;
        if store_reset == SessionStoreResetAction::Clear {
            self.session_store.clear_pending = true;
        }
    }

    fn reset_session_state_if_client_id_changed(&mut self) {
        let current_key = self.options.session_store_key();
        if self.session_store_key.as_ref() == Some(&current_key) {
            return;
        }

        if self.session_store_key.is_some() {
            self.reset_session_state_without_store_invalidation();
        }
        self.session_client_id = None;
        self.session_store_key = None;
        self.session_store.loaded = false;
    }

    fn reconcile_outgoing_tracking_after_connack(&mut self) {
        self.state
            .reconcile_outgoing_tracking_capacity(self.pending.is_empty());
    }

    async fn load_persisted_session_if_needed(&mut self) -> Result<(), ConnectionError> {
        if self.session_store.clear_pending {
            self.clear_persisted_session().await?;
            return Ok(());
        }

        if self.session_store.loaded || self.options.clean_start() {
            self.session_store.loaded = true;
            return Ok(());
        }

        let Some(store) = self.options.session_store() else {
            self.session_store.loaded = true;
            self.session_store.clear_pending = false;
            return Ok(());
        };

        let key = self.options.session_store_key();
        let client_id = key.client_id().to_owned();
        let Some(session) = store
            .load(&key)
            .await
            .map_err(ConnectionError::SessionStore)?
        else {
            self.session_store.loaded = true;
            return Ok(());
        };

        let replay = self
            .state
            .restore_persisted_session(&self.options, &session)?;
        #[cfg(feature = "tracing")]
        let replay_count = replay.len();
        self.pending
            .extend(replay.into_iter().map(RequestEnvelope::plain_replay));
        self.session_client_id = Some(client_id);
        self.session_store_key = Some(key);
        self.session_store.loaded = true;
        #[cfg(feature = "tracing")]
        crate::instrumentation::session_restored(
            self.telemetry.connection_generation(),
            replay_count,
        );
        Ok(())
    }

    fn persisted_session_save_with_expiry(
        &self,
        session_expiry_interval: Option<u32>,
    ) -> SessionSave {
        if self.options.clean_start() {
            return SessionSave(None);
        }

        let Some(store) = self.options.session_store() else {
            return SessionSave(None);
        };

        let pending_replay = self
            .pending
            .iter()
            .filter(|envelope| envelope.replay)
            .map(|envelope| &envelope.request);
        let queued_replay = self
            .queued
            .iter()
            .filter(|envelope| envelope.replay)
            .map(|envelope| &envelope.request);
        let session = self.state.persisted_session(
            &self.options,
            session_expiry_interval,
            pending_replay.chain(queued_replay),
        );
        SessionSave(Some((store, self.options.session_store_key(), session)))
    }

    fn persisted_session_save(&self) -> SessionSave {
        self.persisted_session_save_with_expiry(self.effective_session_expiry_interval)
    }

    async fn save_persisted_session(&mut self) -> Result<(), ConnectionError> {
        if self.persisted_session_save().save().await? {
            self.session_store.loaded = true;
            self.session_store.clear_pending = false;
        }

        Ok(())
    }

    async fn persist_session_or_fail_qos0_notices(
        &mut self,
        qos0_notices: &mut Vec<PublishNoticeTx>,
        flush_notice: Option<PublishNoticeTx>,
    ) -> Result<(), ConnectionError> {
        if let Err(err) = self.save_persisted_session().await {
            let error = PublishNoticeError::SessionPersistence(err.to_string());
            for notice in qos0_notices.drain(..) {
                notice.error(error.clone());
            }
            if let Some(notice) = flush_notice {
                notice.error(error);
            }
            return Err(err);
        }

        if let Some(notice) = flush_notice {
            qos0_notices.push(notice);
        }
        Ok(())
    }

    async fn clear_persisted_session(&mut self) -> Result<(), ConnectionError> {
        let Some(store) = self.options.session_store() else {
            self.session_store.loaded = true;
            self.session_store.clear_pending = false;
            return Ok(());
        };

        let key = self.options.session_store_key();
        store
            .clear(&key)
            .await
            .map_err(ConnectionError::SessionStore)?;
        self.session_store.loaded = true;
        self.session_store.clear_pending = false;
        Ok(())
    }

    async fn clear_persisted_session_or_block_reload(&mut self) -> Result<(), ConnectionError> {
        self.session_store.clear_pending = true;
        self.clear_persisted_session().await
    }

    fn disconnect_session_expiry_interval(&self, disconnect: &Disconnect) -> Option<u32> {
        Self::disconnect_session_expiry_override(disconnect)
            .or(self.effective_session_expiry_interval)
    }

    fn disconnect_session_expiry_override(disconnect: &Disconnect) -> Option<u32> {
        disconnect
            .properties
            .as_ref()
            .and_then(|properties| properties.session_expiry_interval)
    }

    async fn checkpoint_after_connection_loss(&mut self) -> Result<(), ConnectionError> {
        if self.effective_session_expiry_interval.unwrap_or(0) == 0 {
            self.clear_persisted_session().await
        } else {
            self.save_persisted_session().await
        }
    }

    fn apply_connack_session_expiry_interval(&mut self, connack: &ConnAck) {
        self.effective_session_expiry_interval = connack
            .properties
            .as_ref()
            .and_then(|properties| properties.session_expiry_interval)
            .or_else(|| self.options.session_expiry_interval());
    }

    async fn complete_read_batch(
        &mut self,
        batch: ReadBatch,
    ) -> Result<ReadBatchOutcome, ConnectionError> {
        let ReadBatch { outcome, notices } = batch;
        self.state.mark_outgoing_publishes_flush_attempted();
        if let Err(err) = self.save_persisted_session().await {
            let error = err.to_string();
            for notice in notices {
                notice.persistence_error(error.clone());
            }
            return Err(err);
        }

        let flush_result = self.network.as_mut().unwrap().flush().await;
        for notice in notices {
            notice.complete();
        }
        flush_result?;
        Ok(outcome)
    }

    async fn complete_failed_read_batch(&mut self, error: ReadBatchError) -> ConnectionError {
        let ReadBatchError { source, batch } = error;
        let ReadBatch { notices, .. } = batch;
        if notices.is_empty() {
            return ConnectionError::MqttState(source);
        }

        if let Err(err) = self.save_persisted_session().await {
            let error = err.to_string();
            for notice in notices {
                notice.persistence_error(error.clone());
            }
            return err;
        }

        for notice in notices {
            notice.complete();
        }
        ConnectionError::MqttState(source)
    }

    async fn establish_connection(&mut self) -> Result<(), ConnectionError> {
        self.reset_session_state_if_client_id_changed();
        self.load_persisted_session_if_needed().await?;

        #[cfg(feature = "tracing")]
        let attempt = {
            let attempt = self.telemetry.begin_attempt();
            crate::instrumentation::connection_attempt(attempt);
            attempt
        };

        let (network, connack) = match time::timeout(
            self.options.connect_timeout(),
            connect(&mut self.options, &mut self.state),
        )
        .await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(failure)) => {
                #[cfg(feature = "tracing")]
                crate::instrumentation::connection_attempt_failed(
                    attempt,
                    failure.phase,
                    &failure.error,
                );
                return Err(failure.error);
            }
            Err(elapsed) => {
                let error = ConnectionError::Timeout(elapsed);
                #[cfg(feature = "tracing")]
                crate::instrumentation::connection_attempt_failed(attempt, "connection", &error);
                return Err(error);
            }
        };

        self.apply_connack_session_expiry_interval(&connack);

        #[cfg(feature = "tracing")]
        self.reconcile_connack_session(connack.session_present)
            .inspect_err(|error| {
                crate::instrumentation::connection_attempt_failed(
                    attempt,
                    "session_reconciliation",
                    error,
                );
            })?;
        #[cfg(not(feature = "tracing"))]
        self.reconcile_connack_session(connack.session_present)?;

        if !connack.session_present {
            #[cfg(feature = "tracing")]
            self.clear_persisted_session_or_block_reload()
                .await
                .inspect_err(|error| {
                    crate::instrumentation::connection_attempt_failed(
                        attempt,
                        "session_reconciliation",
                        error,
                    );
                })?;
            #[cfg(not(feature = "tracing"))]
            self.clear_persisted_session_or_block_reload().await?;
        }

        if !self.broker_only_session_resume {
            self.session_client_id = Some(self.options.client_id());
            self.session_store_key = Some(self.options.session_store_key());
        }
        self.network = Some(network);

        if self.keepalive_timeout.is_none() && !self.options.keep_alive.is_zero() {
            self.keepalive_timeout = Some(Box::pin(time::sleep(self.options.keep_alive)));
        }

        if let Err(source) = self
            .state
            .handle_incoming_packet(Incoming::ConnAck(connack.clone()))
        {
            let error = ConnectionError::MqttState(source);
            #[cfg(feature = "tracing")]
            crate::instrumentation::connection_attempt_failed(attempt, "mqtt_handshake", &error);
            return Err(error);
        }
        self.reconcile_outgoing_tracking_after_connack();

        #[cfg(feature = "tracing")]
        {
            self.telemetry.mark_connection_established();
            crate::instrumentation::connection_established(attempt, connack.session_present);
        }

        Ok(())
    }

    async fn handle_network_result(
        &mut self,
        result: Result<Event, ConnectionError>,
    ) -> Result<Event, ConnectionError> {
        match result {
            Ok(v) => Ok(v),
            Err(ConnectionError::DisconnectTimeout) => {
                let error = ConnectionError::DisconnectTimeout;
                #[cfg(feature = "tracing")]
                {
                    crate::instrumentation::connection_lost(
                        self.telemetry.last_attempt(),
                        &error,
                        &self.diagnostics(),
                    );
                }
                #[cfg(not(feature = "tracing"))]
                warn!(
                    "Graceful disconnect timed out before outbound protocol state drained: {}; \
                     pending={}, queued={}, requests_rx={}, control_requests_rx={}",
                    self.state.outbound_drain_diagnostics(),
                    self.pending.len(),
                    self.queued.len(),
                    self.requests_rx.len(),
                    self.control_requests_rx.len()
                );
                // The timeout closes the network without sending DISCONNECT. Normalize
                // all in-flight protocol state into replay envelopes before replacing
                // the durable checkpoint, then discard terminal in-memory work.
                self.clean();
                self.disconnect_complete = true;
                self.checkpoint_after_connection_loss().await?;
                self.drop_unprocessed_requests();
                Err(error)
            }
            Err(e) => {
                #[cfg(feature = "tracing")]
                crate::instrumentation::connection_lost(
                    self.telemetry.last_attempt(),
                    &e,
                    &self.diagnostics(),
                );
                // MQTT requires that packets pending acknowledgement should be republished on session resume.
                // Move pending messages from state to eventloop.
                self.clean();
                self.checkpoint_after_connection_loss().await?;
                Err(e)
            }
        }
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
                drop(envelope.notice);
                return Err(ConnectionError::RequestsDone);
            }
            self.establish_connection().await?;
        }

        let result = self.select().await;
        self.handle_network_result(result).await
    }

    /// Converts this event loop into an async [`Stream`](futures_core::Stream).
    ///
    /// The stream owns and drives the event loop by repeatedly calling [`poll`](Self::poll).
    /// This is an integration adapter for APIs and codebases that work with streams;
    /// direct [`poll`](Self::poll) calls can still be used with `tokio::select!`.
    ///
    /// [`ConnectionError::RequestsDone`] terminates the stream; all other poll results,
    /// including connection errors, are yielded as stream items. Continue polling after
    /// those errors to allow the event loop to reconnect.
    ///
    /// Fallible-stream combinators such as `TryStreamExt::try_for_each` stop on the first
    /// error. Use them only when fail-fast behavior is intended.
    #[cfg(feature = "stream")]
    #[cfg_attr(docsrs, doc(cfg(feature = "stream")))]
    pub fn into_stream(self) -> impl futures_core::Stream<Item = Result<Event, ConnectionError>> {
        futures_util::stream::unfold(self, |mut eventloop| async move {
            match eventloop.poll().await {
                Err(ConnectionError::RequestsDone) => None,
                result => Some((result, eventloop)),
            }
        })
    }

    /// Select on network and requests and generate keepalive pings when necessary
    async fn select(&mut self) -> Result<Event, ConnectionError> {
        loop {
            if let Some(event) = self.state.events.pop_front() {
                return Ok(event);
            }

            if let Ok(envelope) = self.immediate_disconnect_rx.try_recv() {
                return self.handle_immediate_disconnect(envelope).await;
            }

            if self.all_request_sources_drained() {
                return Err(ConnectionError::RequestsDone);
            }

            if self.pending_disconnect.is_none() && self.handle_ready_requests().await? {
                if let Some(event) = self.state.events.pop_front() {
                    return Ok(event);
                }
                continue;
            }

            if self.pending_disconnect.is_some() {
                if self.state.outbound_requests_drained() {
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
                    Ok(envelope) => {
                        self.admit_normal_request_batch(envelope).await;
                        continue;
                    }
                    Err(_) => continue,
                },
                o = self.network.as_mut().unwrap().readb(&mut self.state, read_batch_size) => {
                    let batch = match o {
                        Ok(batch) => batch,
                        Err(err) => {
                            return Err(self.complete_failed_read_batch(err).await);
                        }
                    };
                    let outcome = self.complete_read_batch(batch).await?;
                    if matches!(outcome, ReadBatchOutcome::ResponseWritten) {
                        self.reset_keepalive_timeout();
                    }
                    Ok(self.state.events.pop_front().unwrap())
                },
                () = self.keepalive_timeout.as_mut().unwrap_or(no_sleep),
                    if self.keepalive_timeout.is_some() && !self.options.keep_alive.is_zero() => {
                    let (outgoing, _flush_notice) = self
                        .state
                        .handle_outgoing_packet_with_notice(Request::PingReq, None)?;
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
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;
        self.handle_request_internal(
            envelope,
            &mut should_flush,
            &mut qos0_notices,
            &mut checkpoint_action,
        )
        .await?;
        self.flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await?;
        Ok(self.state.events.pop_front().unwrap())
    }

    async fn handle_ready_requests(&mut self) -> Result<bool, ConnectionError> {
        let Some(envelope) = self.next_scheduled_request() else {
            return Ok(false);
        };

        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        match self
            .handle_request_internal(
                envelope,
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await?
        {
            BatchControl::Continue => {
                for _ in 1..self.options.max_request_batch.max(1) {
                    let Some(envelope) = self.next_scheduled_request() else {
                        break;
                    };

                    match self
                        .handle_request_internal(
                            envelope,
                            &mut should_flush,
                            &mut qos0_notices,
                            &mut checkpoint_action,
                        )
                        .await?
                    {
                        BatchControl::Continue => {}
                        BatchControl::Stop => break,
                    }
                }
            }
            BatchControl::Stop => {}
        }

        self.flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await?;
        Ok(true)
    }

    fn next_scheduled_request(&mut self) -> Option<RequestEnvelope> {
        let state = &self.state;
        self.queued
            .pop_next(|envelope| classify_envelope(state, envelope))
    }

    fn normal_request_admission_allowed(&self) -> bool {
        self.queued.is_empty()
            || self
                .queued
                .has_ready(|envelope| classify_envelope(&self.state, envelope))
    }

    async fn try_admit_existing_normal_requests(&mut self) {
        let ready = self.pending.len() + self.requests_rx.len();
        for _ in 0..ready {
            if !self.normal_request_admission_allowed() && self.pending.is_empty() {
                break;
            }

            let Some(envelope) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.options.pending_throttle,
            )
            .await
            else {
                break;
            };

            let stop_batch = is_disconnect_request(&envelope.request);
            self.queued.push_back(envelope);
            if stop_batch {
                break;
            }
        }
    }

    async fn admit_normal_request_batch(&mut self, first: RequestEnvelope) {
        let stop_batch = is_disconnect_request(&first.request);
        self.queued.push_back(first);
        if stop_batch || !self.normal_request_admission_allowed() && self.pending.is_empty() {
            return;
        }

        for _ in 1..self.options.max_request_batch.max(1) {
            let Some(envelope) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.options.pending_throttle,
            )
            .await
            else {
                break;
            };
            let stop_batch = is_disconnect_request(&envelope.request);
            self.queued.push_back(envelope);
            if stop_batch {
                break;
            }
        }
    }

    async fn handle_request_internal(
        &mut self,
        envelope: RequestEnvelope,
        should_flush: &mut bool,
        qos0_notices: &mut Vec<PublishNoticeTx>,
        checkpoint_action: &mut SessionCheckpointAction,
    ) -> Result<BatchControl, ConnectionError> {
        let RequestEnvelope {
            request,
            notice,
            replay,
        } = envelope;
        if self.broker_only_session_resume && request_allocates_client_packet_id(&request) {
            if replay {
                self.state.discard_replayed_request(&request);
            }
            reject_broker_only_session_request(&request, notice);
            return Ok(BatchControl::Continue);
        }
        if matches!(&request, Request::Publish(publish) if publish.retain)
            && !self.state.retain_available()
        {
            if replay {
                self.state.discard_replayed_request(&request);
            }
            reject_unsupported_retained_publish(&request, notice);
            return Ok(BatchControl::Continue);
        }

        match request {
            Request::Disconnect(disconnect) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
                self.pending_disconnect = Some(PendingDisconnect::new(disconnect, None));
                Ok(BatchControl::Stop)
            }
            Request::DisconnectWithTimeout(disconnect, timeout) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
                self.pending_disconnect = Some(PendingDisconnect::new(disconnect, Some(timeout)));
                Ok(BatchControl::Stop)
            }
            Request::DisconnectNow(disconnect) => {
                self.state.fail_auth_exchange_due_to_client_disconnect();
                let session_expiry_interval = self.disconnect_session_expiry_interval(&disconnect);
                let session_expiry_override = Self::disconnect_session_expiry_override(&disconnect);
                let (outgoing, _) = self.state.handle_outgoing_packet_with_notice(
                    Request::DisconnectNow(disconnect),
                    notice,
                )?;
                if session_expiry_interval.unwrap_or(0) == 0 || session_expiry_override.is_some() {
                    *checkpoint_action =
                        SessionCheckpointAction::ApplySessionExpiry(session_expiry_interval);
                }
                if let Some(outgoing) = outgoing {
                    if let Err(err) = self.network.as_mut().unwrap().write(outgoing).await {
                        return Err(ConnectionError::MqttState(err));
                    }
                    *should_flush = true;
                }
                self.disconnect_complete = true;
                Ok(BatchControl::Stop)
            }
            request => {
                let (outgoing, flush_notice) = if replay {
                    self.state
                        .handle_replayed_outgoing_packet_with_notice(request, notice)?
                } else {
                    self.state
                        .handle_outgoing_packet_with_notice(request, notice)?
                };
                self.persist_session_or_fail_qos0_notices(qos0_notices, flush_notice)
                    .await?;
                if let Some(outgoing) = outgoing {
                    if let Err(err) = self.network.as_mut().unwrap().write(outgoing).await {
                        for notice in qos0_notices.drain(..) {
                            notice.error(PublishNoticeError::Qos0NotFlushed);
                        }
                        return Err(ConnectionError::MqttState(err));
                    }
                    *should_flush = true;
                }
                Ok(BatchControl::Continue)
            }
        }
    }

    async fn flush_request_batch(
        &mut self,
        should_flush: bool,
        qos0_notices: Vec<PublishNoticeTx>,
        checkpoint_action: SessionCheckpointAction,
    ) -> Result<(), ConnectionError> {
        if !should_flush {
            match checkpoint_action {
                SessionCheckpointAction::Save => {}
                SessionCheckpointAction::ApplySessionExpiry(session_expiry_interval) => {
                    self.effective_session_expiry_interval = session_expiry_interval;
                    self.checkpoint_after_connection_loss().await?;
                }
            }
            return Ok(());
        }

        match self
            .flush_network_with_checkpoint_action(checkpoint_action)
            .await
        {
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
        self.save_persisted_session().await?;
        self.network.as_mut().unwrap().flush().await?;
        Ok(())
    }

    async fn flush_network_with_checkpoint_action(
        &mut self,
        checkpoint_action: SessionCheckpointAction,
    ) -> Result<(), ConnectionError> {
        match checkpoint_action {
            SessionCheckpointAction::Save => self.flush_network().await,
            SessionCheckpointAction::ApplySessionExpiry(session_expiry_interval) => {
                // Preserve current resumable state before exposing a nonzero
                // DISCONNECT override. A zero interval will clear it after flush.
                if session_expiry_interval.unwrap_or(0) == 0 {
                    self.network.as_mut().unwrap().flush().await?;
                } else {
                    self.flush_network().await?;
                }
                self.effective_session_expiry_interval = session_expiry_interval;
                self.checkpoint_after_connection_loss().await
            }
        }
    }

    async fn poll_disconnect_drain(&mut self) -> Result<Option<Event>, ConnectionError> {
        let read_batch_size = self.effective_read_batch_size();
        let read = self
            .network
            .as_mut()
            .unwrap()
            .readb(&mut self.state, read_batch_size);

        let batch = if let Some(deadline) = self
            .pending_disconnect
            .as_ref()
            .and_then(|pending| pending.deadline)
        {
            select! {
                o = self.immediate_disconnect_rx.recv_async(), if !self.immediate_disconnect_rx.is_disconnected() => match o {
                    Ok(envelope) => return self.handle_immediate_disconnect(envelope).await.map(Some),
                    Err(_) => return Ok(None),
                },
                result = read => match result {
                    Ok(batch) => batch,
                    Err(err) => {
                        return Err(self.complete_failed_read_batch(err).await);
                    }
                },
                () = time::sleep_until(deadline) => return Err(ConnectionError::DisconnectTimeout),
            }
        } else {
            select! {
                o = self.immediate_disconnect_rx.recv_async(), if !self.immediate_disconnect_rx.is_disconnected() => match o {
                    Ok(envelope) => return self.handle_immediate_disconnect(envelope).await.map(Some),
                    Err(_) => return Ok(None),
                },
                result = read => match result {
                    Ok(batch) => batch,
                    Err(err) => {
                        return Err(self.complete_failed_read_batch(err).await);
                    }
                },
            }
        };

        let outcome = self.complete_read_batch(batch).await?;
        if matches!(outcome, ReadBatchOutcome::ResponseWritten) {
            self.reset_keepalive_timeout();
        }
        Ok(None)
    }

    async fn send_pending_disconnect(&mut self) -> Result<Event, ConnectionError> {
        let disconnect = self
            .pending_disconnect
            .take()
            .expect("pending disconnect checked by caller")
            .disconnect;
        let session_expiry_interval = self.disconnect_session_expiry_interval(&disconnect);
        let session_expiry_override = Self::disconnect_session_expiry_override(&disconnect);
        let clear_persisted_session = session_expiry_interval.unwrap_or(0) == 0;
        let (outgoing, _) = self
            .state
            .handle_outgoing_packet_with_notice(Request::DisconnectNow(disconnect), None)?;

        if let Some(outgoing) = outgoing {
            self.network.as_mut().unwrap().write(outgoing).await?;
            if clear_persisted_session {
                self.network.as_mut().unwrap().flush().await?;
            } else {
                self.flush_network().await?;
            }

            if clear_persisted_session || session_expiry_override.is_some() {
                self.effective_session_expiry_interval = session_expiry_interval;
            }
        }

        self.drop_unprocessed_requests();
        self.checkpoint_after_connection_loss().await?;
        self.disconnect_complete = true;
        Ok(self.state.events.pop_front().unwrap())
    }

    fn reset_keepalive_timeout(&mut self) {
        if self.options.keep_alive.is_zero() {
            return;
        }

        if let Some(timeout) = &mut self.keepalive_timeout {
            timeout
                .as_mut()
                .reset(Instant::now() + self.options.keep_alive);
        }
    }

    async fn try_next_request(
        pending: &mut VecDeque<RequestEnvelope>,
        rx: &Receiver<RequestEnvelope>,
        pending_throttle: Duration,
    ) -> Option<RequestEnvelope> {
        if !pending.is_empty() {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .next() AFTER sleep() otherwise .next() would
            // advance the iterator but the future might be canceled before return
            return pending.pop_front();
        }

        match rx.try_recv() {
            Ok(envelope) => return Some(envelope),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }

        None
    }

    async fn next_request(
        pending: &mut VecDeque<RequestEnvelope>,
        rx: &Receiver<RequestEnvelope>,
        pending_throttle: Duration,
    ) -> Result<RequestEnvelope, ConnectionError> {
        if pending.is_empty() {
            rx.recv_async()
                .await
                .map_err(|_| ConnectionError::RequestsDone)
        } else {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .next() AFTER sleep() otherwise .next() would
            // advance the iterator but the future might be canceled before return
            Ok(pending.pop_front().unwrap())
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

fn classify_envelope(state: &MqttState, envelope: &RequestEnvelope) -> ScheduledRequest {
    if envelope.replay {
        classify_replay_request(state, &envelope.request)
    } else {
        classify_request(state, &envelope.request)
    }
}

fn classify_request(state: &MqttState, request: &Request) -> ScheduledRequest {
    classify_publish_or_control_request(
        request,
        |publish| state.can_send_publish(publish),
        || state.control_packet_identifier_available(),
    )
}

fn classify_replay_request(state: &MqttState, request: &Request) -> ScheduledRequest {
    classify_publish_or_control_request(
        request,
        |publish| state.can_send_replayed_publish(publish),
        || match request {
            Request::Subscribe(subscribe) => state.can_send_replayed_control_packet(subscribe.pkid),
            Request::Unsubscribe(unsubscribe) => {
                state.can_send_replayed_control_packet(unsubscribe.pkid)
            }
            _ => true,
        },
    )
}

fn classify_publish_or_control_request(
    request: &Request,
    can_send_publish: impl FnOnce(&Publish) -> bool,
    can_send_control: impl FnOnce() -> bool,
) -> ScheduledRequest {
    match request {
        Request::Publish(publish) if publish.qos != crate::mqttbytes::QoS::AtMostOnce => {
            ScheduledRequest {
                class: RequestClass::FlowControlledPublish,
                readiness: if can_send_publish(publish) {
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
        request if request_needs_fresh_client_packet_id(request) => ScheduledRequest {
            class: RequestClass::Control,
            readiness: if can_send_control() {
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

const fn request_needs_fresh_client_packet_id(request: &Request) -> bool {
    matches!(request, Request::Subscribe(_) | Request::Unsubscribe(_))
}

fn request_allocates_client_packet_id(request: &Request) -> bool {
    match request {
        Request::Publish(publish) => publish.qos != crate::mqttbytes::QoS::AtMostOnce,
        request if request_needs_fresh_client_packet_id(request) => true,
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

fn reject_unsupported_retained_publish(request: &Request, notice: Option<TrackedNoticeTx>) {
    warn!(
        "rejecting retained publish because the broker does not support retained messages: {request:?}"
    );

    if let Some(TrackedNoticeTx::Publish(notice)) = notice {
        notice.error(PublishNoticeError::RetainNotSupported);
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
struct ConnectFailure {
    error: ConnectionError,
    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    phase: &'static str,
}

impl ConnectFailure {
    const fn new(error: ConnectionError, phase: &'static str) -> Self {
        Self { error, phase }
    }
}

async fn connect(
    options: &mut MqttOptions,
    state: &mut MqttState,
) -> Result<(Network, ConnAck), ConnectFailure> {
    // connect to the broker
    let mut network = network_connect(options).await.map_err(|error| {
        let phase = match error {
            ConnectionError::BrokerTransportMismatch => "target_setup",
            #[cfg(feature = "websocket")]
            ConnectionError::InvalidUrl(_) | ConnectionError::RequestModifier(_) => "target_setup",
            _ => "transport",
        };
        ConnectFailure::new(error, phase)
    })?;

    // make MQTT connection request (which internally awaits for ack)
    let connack = mqtt_connect(options, &mut network, state)
        .await
        .map_err(|error| ConnectFailure::new(error, "mqtt_handshake"))?;

    Ok((network, connack))
}

#[expect(clippy::too_many_lines)]
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
        Transport::Wss(_) => split_url_with_default_port(
            options
                .broker()
                .websocket_url()
                .ok_or(ConnectionError::BrokerTransportMismatch)?,
            Some(443),
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
            let socket = tls::tls_connect(&domain, port, &tls_config, tcp_stream).await?;
            Network::new(socket, max_incoming_pkt_size)
        }
        #[cfg(unix)]
        Transport::Unix => unreachable!(),
        #[cfg(feature = "websocket")]
        Transport::Ws => {
            let mut request = options
                .broker()
                .websocket_url()
                .ok_or(ConnectionError::BrokerTransportMismatch)?
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
            validate_response_headers(&response)?;

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
                .ok_or(ConnectionError::BrokerTransportMismatch)?
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
            validate_response_headers(&response)?;

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
        && let Err(error) = network.flush().await
    {
        warn!("ignoring protocol error disconnect flush failure: {error:?}");
    }
}

const fn should_replay_after_reconnect(request: &Request) -> bool {
    !matches!(request, Request::PubAck(_) | Request::PubRec(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framed::ReadBatch;
    use crate::mqttbytes::{Error as MqttError, QoS};
    use crate::notice::DeferredNotice;
    use crate::session::PersistedSession;
    use crate::{AckMode, BrokerSessionResumePolicy};
    use crate::{Auth, AuthProperties, AuthReasonCode};
    use crate::{
        ConnAckProperties, PubAck, PubComp, PubCompReason, PubRec, PubRel, PublishProperties,
        SubAck, SubscribeFilter, SubscribeReasonCode, UnsubAck, UnsubAckReason,
    };
    use bytes::{Bytes, BytesMut};
    use flume::TryRecvError;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};

    #[tokio::test]
    async fn connection_timeout_display_is_specific() {
        let elapsed = tokio::time::timeout(
            std::time::Duration::from_millis(0),
            futures_util::future::pending::<()>(),
        )
        .await
        .unwrap_err();

        assert_eq!(
            ConnectionError::Timeout(elapsed).to_string(),
            "Connection timeout"
        );
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_response_validation_display_includes_source() {
        let error = ConnectionError::ResponseValidation(
            crate::websockets::ValidationError::SubprotocolHeaderMissing,
        );

        assert_eq!(
            error.to_string(),
            "Websocket response validation error: Websocket response does not contain subprotocol header"
        );
    }

    #[derive(Debug)]
    struct FailingSessionStore;

    impl crate::SessionStore for FailingSessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, crate::SessionStoreError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }

        fn save<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
            _session: &'a PersistedSession,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                Err(Box::new(std::io::Error::other("session save failed"))
                    as crate::SessionStoreError)
            })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Debug)]
    struct SuccessfulSessionStore;

    impl crate::SessionStore for SuccessfulSessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, crate::SessionStoreError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }

        fn save<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
            _session: &'a PersistedSession,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Clone, Debug)]
    struct CapturingSessionStore {
        session: Arc<Mutex<Option<PersistedSession>>>,
    }

    impl CapturingSessionStore {
        fn new() -> Self {
            Self {
                session: Arc::new(Mutex::new(None)),
            }
        }

        fn current(&self) -> Option<PersistedSession> {
            self.session.lock().unwrap().clone()
        }
    }

    impl crate::SessionStore for CapturingSessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, crate::SessionStoreError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(self.session.lock().unwrap().clone()) })
        }

        fn save<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
            session: &'a PersistedSession,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                *self.session.lock().unwrap() = Some(session.clone());
                Ok(())
            })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                *self.session.lock().unwrap() = None;
                Ok(())
            })
        }
    }

    #[derive(Clone, Debug)]
    struct TransientSessionStore {
        session: Arc<Mutex<Option<PersistedSession>>>,
        save_calls: Arc<Mutex<usize>>,
        clear_calls: Arc<Mutex<usize>>,
    }

    impl TransientSessionStore {
        fn new() -> Self {
            Self {
                session: Arc::new(Mutex::new(None)),
                save_calls: Arc::new(Mutex::new(0)),
                clear_calls: Arc::new(Mutex::new(0)),
            }
        }

        fn current(&self) -> Option<PersistedSession> {
            self.session.lock().unwrap().clone()
        }
    }

    impl crate::SessionStore for TransientSessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, crate::SessionStoreError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(self.session.lock().unwrap().clone()) })
        }

        fn save<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
            session: &'a PersistedSession,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                let mut calls = self.save_calls.lock().unwrap();
                *calls += 1;
                if *calls == 2 {
                    return Err(
                        Box::new(std::io::Error::other("transient session save failure"))
                            as crate::SessionStoreError,
                    );
                }
                *self.session.lock().unwrap() = Some(session.clone());
                Ok(())
            })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                let mut calls = self.clear_calls.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    return Err(
                        Box::new(std::io::Error::other("transient session clear failure"))
                            as crate::SessionStoreError,
                    );
                }
                *self.session.lock().unwrap() = None;
                Ok(())
            })
        }
    }

    #[derive(Clone, Debug)]
    struct ClearFailingSessionStore {
        session: Arc<Mutex<Option<PersistedSession>>>,
        loads: Arc<Mutex<usize>>,
    }

    impl ClearFailingSessionStore {
        fn new() -> Self {
            Self {
                session: Arc::new(Mutex::new(None)),
                loads: Arc::new(Mutex::new(0)),
            }
        }

        fn current(&self) -> Option<PersistedSession> {
            self.session.lock().unwrap().clone()
        }

        fn load_count(&self) -> usize {
            *self.loads.lock().unwrap()
        }
    }

    impl crate::SessionStore for ClearFailingSessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, crate::SessionStoreError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                *self.loads.lock().unwrap() += 1;
                Ok(self.session.lock().unwrap().clone())
            })
        }

        fn save<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
            session: &'a PersistedSession,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                *self.session.lock().unwrap() = Some(session.clone());
                Ok(())
            })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::SessionStoreError>> + Send + 'a>>
        {
            Box::pin(async {
                Err(Box::new(std::io::Error::other("session clear failed"))
                    as crate::SessionStoreError)
            })
        }
    }

    fn mark_local_session(eventloop: &mut EventLoop) {
        eventloop.session_client_id = Some(eventloop.options.client_id());
        eventloop.session_store_key = Some(eventloop.options.session_store_key());
    }

    #[derive(Debug)]
    struct FlushFailingIo;

    impl AsyncRead for FlushFailingIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for FlushFailingIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::Error::other("flush failed")))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

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

    fn build_connack_with_retain_available(retain_available: u8) -> ConnAck {
        ConnAck {
            session_present: false,
            code: ConnectReturnCode::Success,
            properties: Some(ConnAckProperties {
                session_expiry_interval: None,
                receive_max: None,
                max_qos: None,
                retain_available: Some(retain_available),
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

    fn eventloop_with_failing_session_store() -> EventLoop {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(FailingSessionStore);
        EventLoop::new_for_async_client(options, 1).0
    }

    fn eventloop_with_successful_session_store_and_failing_flush() -> EventLoop {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(SuccessfulSessionStore);
        let mut eventloop = EventLoop::new_for_async_client(options, 1).0;
        eventloop.network = Some(Network::new(FlushFailingIo, Some(1024)));
        eventloop
    }

    fn assert_persistence_message(message: &str) {
        assert!(
            message.contains("session save failed"),
            "unexpected persistence notice error: {message}"
        );
    }

    #[tokio::test]
    async fn read_batch_publish_notice_reports_persistence_failure_before_success() {
        let mut eventloop = eventloop_with_failing_session_store();
        let (notice_tx, notice) = PublishNoticeTx::new();
        let puback = PubAck::new(1, None);
        let batch = ReadBatch {
            outcome: ReadBatchOutcome::NoResponseWritten,
            notices: vec![DeferredNotice::Publish(
                notice_tx,
                PublishResult::Qos1(puback),
            )],
        };

        let err = eventloop.complete_read_batch(batch).await.unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        match notice.wait_async().await {
            Err(PublishNoticeError::SessionPersistence(message)) => {
                assert_persistence_message(&message);
            }
            result => panic!("unexpected publish notice result: {result:?}"),
        }
    }

    #[tokio::test]
    async fn read_batch_subscribe_notice_reports_persistence_failure_before_success() {
        let mut eventloop = eventloop_with_failing_session_store();
        let (notice_tx, notice) = SubscribeNoticeTx::new();
        let suback = SubAck {
            pkid: 1,
            return_codes: vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            properties: None,
        };
        let batch = ReadBatch {
            outcome: ReadBatchOutcome::NoResponseWritten,
            notices: vec![DeferredNotice::Subscribe(notice_tx, suback)],
        };

        let err = eventloop.complete_read_batch(batch).await.unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        match notice.wait_async().await {
            Err(SubscribeNoticeError::SessionPersistence(message)) => {
                assert_persistence_message(&message);
            }
            result => panic!("unexpected subscribe notice result: {result:?}"),
        }
    }

    #[tokio::test]
    async fn read_batch_unsubscribe_notice_reports_persistence_failure_before_success() {
        let mut eventloop = eventloop_with_failing_session_store();
        let (notice_tx, notice) = UnsubscribeNoticeTx::new();
        let unsuback = UnsubAck {
            pkid: 1,
            reasons: vec![UnsubAckReason::Success],
            properties: None,
        };
        let batch = ReadBatch {
            outcome: ReadBatchOutcome::NoResponseWritten,
            notices: vec![DeferredNotice::Unsubscribe(notice_tx, unsuback)],
        };

        let err = eventloop.complete_read_batch(batch).await.unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        match notice.wait_async().await {
            Err(UnsubscribeNoticeError::SessionPersistence(message)) => {
                assert_persistence_message(&message);
            }
            result => panic!("unexpected unsubscribe notice result: {result:?}"),
        }
    }

    #[tokio::test]
    async fn tracked_qos0_notice_reports_persistence_failure_before_flush() {
        let mut eventloop = eventloop_with_failing_session_store();
        let (notice_tx, notice) = PublishNoticeTx::new();
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let err = eventloop
            .handle_request_internal(
                RequestEnvelope {
                    request: Request::Publish(publish(QoS::AtMostOnce)),
                    notice: Some(TrackedNoticeTx::Publish(notice_tx)),
                    replay: false,
                },
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        assert!(!should_flush);
        assert!(qos0_notices.is_empty());
        match notice.wait_async().await {
            Err(PublishNoticeError::SessionPersistence(message)) => {
                assert_persistence_message(&message);
            }
            result => panic!("unexpected publish notice result: {result:?}"),
        }
    }

    #[tokio::test]
    async fn read_batch_publish_notice_completes_when_flush_fails_after_persistence() {
        let mut eventloop = eventloop_with_successful_session_store_and_failing_flush();
        let (notice_tx, notice) = PublishNoticeTx::new();
        let puback = PubAck::new(1, None);
        let batch = ReadBatch {
            outcome: ReadBatchOutcome::NoResponseWritten,
            notices: vec![DeferredNotice::Publish(
                notice_tx,
                PublishResult::Qos1(puback.clone()),
            )],
        };

        let err = eventloop.complete_read_batch(batch).await.unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(_))
        ));
        assert_eq!(
            notice.wait_async().await.unwrap(),
            PublishResult::Qos1(puback)
        );
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
        let mut checkpoint_action = SessionCheckpointAction::Save;

        assert_eq!(
            eventloop
                .handle_request_internal(
                    RequestEnvelope {
                        request: Request::Auth(Auth::new(AuthReasonCode::ReAuthenticate, None)),
                        notice: Some(TrackedNoticeTx::Auth(notice_tx)),
                        replay: false,
                    },
                    &mut should_flush,
                    &mut qos0_notices,
                    &mut checkpoint_action,
                )
                .await
                .unwrap(),
            BatchControl::Continue
        );
        assert!(eventloop.state.outbound_requests_drained());

        let handled = eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::Disconnect(Disconnect::new(
                    DisconnectReasonCode::NormalDisconnection,
                ))),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(handled, BatchControl::Stop);
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

        let result =
            run_mqtt_connect_with_stale_state_auth_method(options, None, vec![Packet::PingReq])
                .await;

        assert!(matches!(
            result,
            Err(ConnectionError::NotConnAck(packet))
                if matches!(
                    &*packet,
                    Packet::PingReq
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
    async fn mqtt_connect_reuses_configured_session_expiry_after_connack_override() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_session_expiry_interval(Some(60));
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().session_expiry_interval = Some(300);

        let (result, mut options) =
            run_successful_mqtt_connect_with_connack_and_return_options(options, connack).await;
        result.unwrap();
        assert_eq!(options.session_expiry_interval(), Some(60));

        let (client, mut peer) = tokio::io::duplex(1024);
        let mut network = Network::new(client, Some(1024));
        let mut state = MqttState::new_internal(
            10,
            AckMode::Automatic,
            options.topic_alias_policy(),
            options.topic_alias_max().unwrap_or(0),
            options.authentication_method(),
            options.auth_manager(),
        );
        let connack = build_connack_with_receive_max(10);
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

        match Packet::read(&mut stream, Some(1024)).unwrap() {
            Packet::Connect(connect, _, _) => assert_eq!(
                connect
                    .properties
                    .and_then(|properties| properties.session_expiry_interval),
                Some(60)
            ),
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
        Subscribe::new(SubscribeFilter::new("hello/world", QoS::AtMostOnce), None)
    }

    fn fill_publish_window(eventloop: &mut EventLoop) {
        let mut active = publish(QoS::AtLeastOnce);
        active.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(active);
        eventloop.state.inflight = 1;
        eventloop.state.rebuild_outbound_pkid_index();
    }

    #[test]
    fn diagnostics_reports_fresh_eventloop_state() {
        let options = MqttOptions::new("test-client", "localhost");
        let (eventloop, _request_tx) = EventLoop::new_for_async_client(options, 4);

        let diagnostics = eventloop.diagnostics();

        assert!(!diagnostics.connected);
        assert!(!diagnostics.disconnecting);
        assert!(!diagnostics.disconnect_complete);
        assert_eq!(diagnostics.queues.pending_replay_len, 0);
        assert_eq!(diagnostics.queues.queued_len, 0);
        assert_eq!(diagnostics.queues.pending_len, 0);
        assert_eq!(diagnostics.queues.requests_rx_len, 0);
        assert_eq!(diagnostics.queues.control_requests_rx_len, 0);
        assert_eq!(diagnostics.queues.immediate_disconnect_rx_len, 0);
        assert!(diagnostics.outbound.outbound_drained);
        assert!(!diagnostics.session.session_store_configured);
        assert!(!diagnostics.session.session_store_loaded);
        assert!(!diagnostics.session.session_store_clear_pending);
        assert!(!diagnostics.session.local_session_state_matches_client_id);
        assert!(!diagnostics.session.broker_only_session_resume);
        assert_eq!(
            diagnostics.config.configured_read_batch_size,
            eventloop.options.read_batch_size()
        );
        assert_eq!(
            diagnostics.config.effective_read_batch_size,
            eventloop.effective_read_batch_size()
        );
        assert_eq!(
            diagnostics.config.max_request_batch,
            eventloop.options.max_request_batch()
        );
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn public_clean_advances_generation_once_after_establishment() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.clean();
        assert_eq!(eventloop.telemetry.connection_generation(), 0);

        eventloop.telemetry.begin_attempt();
        eventloop.telemetry.mark_connection_established();
        eventloop.clean();
        eventloop.clean();

        assert_eq!(eventloop.telemetry.connection_generation(), 1);
        let reconnect = eventloop.telemetry.begin_attempt();
        assert_eq!(reconnect.connection_generation, 1);
        assert_eq!(reconnect.attempt_in_generation, 1);
        assert!(reconnect.connection_generation > 0);
    }

    #[test]
    fn diagnostics_reports_eventloop_queues_and_connection_state() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx, control_request_tx, immediate_disconnect_tx) =
            EventLoop::new_for_async_client_with_capacity(
                options,
                RequestChannelCapacity::Bounded(4),
            );
        request_tx
            .try_send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();
        control_request_tx
            .try_send(RequestEnvelope::plain(Request::PingReq))
            .unwrap();
        immediate_disconnect_tx
            .try_send(RequestEnvelope::plain(Request::DisconnectNow(
                Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            )))
            .unwrap();
        push_pending(&mut eventloop, Request::PingReq);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PingReq));
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            None,
        ));
        eventloop.session_client_id = Some("test-client".to_owned());
        eventloop.session_store.loaded = true;
        eventloop.broker_only_session_resume = true;

        let diagnostics = eventloop.diagnostics();

        assert!(diagnostics.connected);
        assert!(diagnostics.disconnecting);
        assert_eq!(diagnostics.queues.pending_replay_len, 1);
        assert_eq!(diagnostics.queues.queued_len, 1);
        assert_eq!(diagnostics.queues.pending_len, 2);
        assert_eq!(
            diagnostics.queues.pending_len,
            diagnostics.queues.pending_replay_len + diagnostics.queues.queued_len
        );
        assert_eq!(diagnostics.queues.requests_rx_len, 1);
        assert_eq!(diagnostics.queues.control_requests_rx_len, 1);
        assert_eq!(diagnostics.queues.immediate_disconnect_rx_len, 1);
        assert!(diagnostics.session.session_store_loaded);
        assert!(diagnostics.session.local_session_state_matches_client_id);
        assert!(diagnostics.session.broker_only_session_resume);
    }

    #[cfg(feature = "stream")]
    fn eventloop_with_network() -> (EventLoop, Sender<RequestEnvelope>, DuplexStream) {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (client, peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        (eventloop, request_tx, peer)
    }

    #[cfg(feature = "stream")]
    #[tokio::test]
    async fn eventloop_stream_yields_pending_event() {
        use futures_util::{StreamExt, pin_mut};

        let (mut eventloop, _request_tx, _peer) = eventloop_with_network();
        eventloop
            .state
            .events
            .push_back(Event::Outgoing(Outgoing::PingReq));

        let stream = eventloop.into_stream();
        pin_mut!(stream);

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        );
    }

    #[cfg(feature = "stream")]
    #[tokio::test]
    async fn eventloop_stream_ends_on_requests_done() {
        use futures_util::{StreamExt, pin_mut};

        let (eventloop, request_tx, _peer) = eventloop_with_network();
        drop(request_tx);

        let stream = eventloop.into_stream();
        pin_mut!(stream);

        assert!(stream.next().await.is_none());
    }

    #[cfg(feature = "stream")]
    #[tokio::test]
    async fn eventloop_stream_remains_usable_after_cancelled_next() {
        use futures_util::{StreamExt, pin_mut};

        let (eventloop, request_tx, _peer) = eventloop_with_network();
        let stream = eventloop.into_stream();
        pin_mut!(stream);

        {
            let next = stream.next();
            tokio::pin!(next);
            tokio::select! {
                result = &mut next => panic!("stream unexpectedly completed: {result:?}"),
                () = time::sleep(Duration::from_millis(10)) => {}
            }
        }

        request_tx
            .send_async(RequestEnvelope::plain(Request::PingReq))
            .await
            .unwrap();

        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        );
    }

    #[cfg(feature = "stream")]
    #[tokio::test]
    async fn eventloop_stream_yields_non_terminal_errors() {
        use futures_util::{StreamExt, pin_mut};

        let (eventloop, _request_tx, peer) = eventloop_with_network();
        drop(peer);

        let stream = eventloop.into_stream();
        pin_mut!(stream);

        assert!(matches!(stream.next().await, Some(Err(_))));
    }

    #[cfg(feature = "stream")]
    #[tokio::test]
    async fn eventloop_stream_reconnects_after_non_terminal_error() {
        use futures_util::{StreamExt, pin_mut};

        let (peer_tx, peer_rx) = flume::bounded(2);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_start(false);
        options.set_keep_alive(0);
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        let mut seeded = publish(QoS::AtLeastOnce);
        seeded.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(seeded);
        eventloop.state.inflight = 1;
        mark_local_session(&mut eventloop);

        let broker = tokio::spawn(async move {
            let connack = ConnAck {
                session_present: true,
                code: ConnectReturnCode::Success,
                properties: None,
            };

            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            drop(peer);

            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        });

        let stream = eventloop.into_stream();
        pin_mut!(stream);

        assert!(matches!(
            stream.next().await,
            Some(Ok(Event::Incoming(Packet::ConnAck(ref connack)))) if connack.session_present
        ));

        assert!(matches!(
            stream.next().await,
            Some(Err(error)) if !matches!(error, ConnectionError::RequestsDone)
        ));

        assert!(matches!(
            stream.next().await,
            Some(Ok(Event::Incoming(Packet::ConnAck(ref connack)))) if connack.session_present
        ));

        broker.await.unwrap();
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
            .map(|envelope| envelope.request)
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

        let envelope = eventloop.next_scheduled_request().unwrap();

        assert!(!envelope.replay);
        assert!(matches!(&envelope.request, Request::Subscribe(_)));
        match eventloop
            .state
            .handle_outgoing_packet(envelope.request)
            .unwrap()
        {
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

        let envelope = eventloop.next_scheduled_request().unwrap();

        assert!(!envelope.replay);
        assert!(matches!(&envelope.request, Request::Unsubscribe(_)));
        match eventloop
            .state
            .handle_outgoing_packet(envelope.request)
            .unwrap()
        {
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

        let envelope = eventloop.next_scheduled_request().unwrap();

        assert!(!envelope.replay);
        assert!(matches!(&envelope.request, Request::Publish(_)));
    }

    #[tokio::test]
    async fn immediate_disconnect_waits_for_buffered_public_event() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx, _control_request_tx, immediate_disconnect_tx) =
            EventLoop::new_for_async_client_with_capacity(
                options,
                RequestChannelCapacity::Bounded(1),
            );
        let (client, _peer) = tokio::io::duplex(64);
        eventloop.network = Some(Network::new(client, Some(64)));
        eventloop
            .state
            .events
            .push_back(Event::Outgoing(Outgoing::PingResp));
        immediate_disconnect_tx
            .send_async(RequestEnvelope::plain(Request::DisconnectNow(
                Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            )))
            .await
            .unwrap();

        let buffered = eventloop.select().await.unwrap();
        let disconnect = eventloop.select().await.unwrap();

        assert_eq!(buffered, Event::Outgoing(Outgoing::PingResp));
        assert_eq!(disconnect, Event::Outgoing(Outgoing::Disconnect));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finite_request_flood_eventually_allows_ready_network_read() {
        const REQUEST_COUNT: usize = 1_024;
        const CHANNEL_CAPACITY: usize = 10;

        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_max_request_batch(1);
        let (mut eventloop, request_tx, _control_request_tx, _immediate_disconnect_tx) =
            EventLoop::new_for_async_client_with_capacity(
                options,
                RequestChannelCapacity::Bounded(CHANNEL_CAPACITY),
            );
        let (client, mut peer) = tokio::io::duplex(64 * 1024);
        let mut network = Network::new(client, Some(64 * 1024));
        network.set_max_outgoing_size(Some(64 * 1024));
        eventloop.network = Some(network);

        for _ in 0..CHANNEL_CAPACITY {
            request_tx
                .try_send(RequestEnvelope::plain(Request::Publish(publish(
                    QoS::AtMostOnce,
                ))))
                .unwrap();
        }
        let producer = tokio::spawn(async move {
            for _ in CHANNEL_CAPACITY..REQUEST_COUNT {
                request_tx
                    .send_async(RequestEnvelope::plain(Request::Publish(publish(
                        QoS::AtMostOnce,
                    ))))
                    .await
                    .unwrap();
            }
        });
        peer.write_all(&[0xD0, 0x00]).await.unwrap();
        let drain = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while peer.read(&mut bytes).await.unwrap_or(0) != 0 {}
        });

        let mut outgoing_before_read = 0;
        loop {
            let event = time::timeout(Duration::from_secs(5), eventloop.select())
                .await
                .expect("request flood should make finite progress")
                .expect("event loop should process request flood");
            match event {
                Event::Outgoing(Outgoing::Publish(0)) => outgoing_before_read += 1,
                Event::Incoming(Packet::PingResp) => break,
                event => panic!("unexpected event during request flood: {event:?}"),
            }
        }
        drain.abort();
        producer.abort();

        assert!(
            (CHANNEL_CAPACITY..=REQUEST_COUNT).contains(&outgoing_before_read),
            "ready network read should make progress during or immediately after the finite request flood; \
             observed {outgoing_before_read} requests"
        );
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
        assert!(matches!(
            request,
            RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            }
        ));

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
        assert!(matches!(
            first,
            RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            }
        ));

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

        assert!(matches!(
            received,
            Some(RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            })
        ));
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
        assert!(matches!(
            first,
            RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            }
        ));
        assert!(eventloop.pending.is_empty());

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(
            second,
            RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            }
        ));
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
        assert!(matches!(
            first,
            RequestEnvelope {
                request: Request::PingReq,
                notice: None,
                replay: false
            }
        ));

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(
            second,
            RequestEnvelope { request: Request::Publish(publish), notice: Some(_), replay: false } if publish == tracked_publish
        ));

        let third = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(
            third,
            RequestEnvelope {
                request: Request::PingResp,
                notice: None,
                replay: false
            }
        ));
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
            SubscribeFilter::new("hello/world", crate::mqttbytes::QoS::AtMostOnce),
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
        mark_local_session(&mut eventloop);

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
            .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly);
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
            .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly);
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
            let mut checkpoint_action = SessionCheckpointAction::Save;
            let keep_batching = eventloop
                .handle_request_internal(
                    RequestEnvelope {
                        request,
                        notice,
                        replay: false,
                    },
                    &mut should_flush,
                    &mut qos0_notices,
                    &mut checkpoint_action,
                )
                .await
                .unwrap();

            assert_eq!(keep_batching, BatchControl::Continue);
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

    #[tokio::test]
    async fn broker_only_session_resume_rejection_releases_replay_packet_identifier() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.reconcile_connack_session(true).unwrap();
        let mut subscribe = subscribe();
        subscribe.pkid = 7;
        eventloop.state.outbound_pkid_in_use.insert(7);
        eventloop.state.outbound_pkid_count = 1;

        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;
        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain_replay(Request::Subscribe(subscribe)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Continue);
        assert!(!eventloop.state.outbound_pkid_in_use.contains(7));
        assert_eq!(eventloop.state.outbound_pkid_count, 0);
    }

    #[tokio::test]
    async fn unsupported_retained_publish_is_rejected_without_dropping_connection() {
        let mut eventloop = build_eventloop(true);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        eventloop
            .state
            .handle_incoming_packet(Incoming::ConnAck(build_connack_with_retain_available(0)))
            .unwrap();
        eventloop.state.events.clear();

        let (notice_tx, notice) = PublishNoticeTx::new();
        let mut retained = publish(QoS::AtLeastOnce);
        retained.retain = true;
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope {
                    request: Request::Publish(retained),
                    notice: Some(TrackedNoticeTx::Publish(notice_tx)),
                    replay: false,
                },
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Continue);
        assert!(!should_flush);
        assert!(qos0_notices.is_empty());
        assert!(eventloop.network.is_some());
        assert!(eventloop.state.events.is_empty());
        assert_eq!(
            notice.wait_async().await.unwrap_err(),
            PublishNoticeError::RetainNotSupported
        );

        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::Publish(publish(QoS::AtMostOnce))),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Continue);
        assert!(should_flush);
        assert!(eventloop.network.is_some());
        assert!(matches!(
            eventloop.state.events.back(),
            Some(Event::Outgoing(Outgoing::Publish(0)))
        ));
    }

    #[tokio::test]
    async fn unsupported_retained_replay_releases_reserved_packet_identifier() {
        let mut eventloop = build_eventloop(true);
        eventloop
            .state
            .handle_incoming_packet(Incoming::ConnAck(build_connack_with_retain_available(0)))
            .unwrap();
        eventloop.state.events.clear();
        let mut retained = publish(QoS::AtLeastOnce);
        retained.pkid = 7;
        retained.retain = true;
        eventloop.state.outbound_pkid_in_use.insert(7);
        eventloop.state.outbound_pkid_count = 1;

        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;
        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain_replay(Request::Publish(retained)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Continue);
        assert!(!eventloop.state.outbound_pkid_in_use.contains(7));
        assert_eq!(eventloop.state.outbound_pkid_count, 0);
    }

    #[test]
    fn connack_reconcile_clears_broker_only_resume_on_new_session() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_broker_session_resume_policy(BrokerSessionResumePolicy::AllowBrokerOnly);
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

    #[tokio::test]
    async fn fresh_connack_clear_marks_session_store_loaded_after_reset() {
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(SuccessfulSessionStore);
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        mark_local_session(&mut eventloop);
        eventloop.session_store.loaded = true;

        eventloop.reconcile_connack_session(false).unwrap();
        assert!(!eventloop.session_store.loaded);

        eventloop.clear_persisted_session().await.unwrap();

        assert!(eventloop.session_store.loaded);
    }

    #[tokio::test]
    async fn fresh_connack_clear_failure_blocks_stale_store_reload() {
        let store = ClearFailingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish(QoS::AtLeastOnce)))
            .unwrap();
        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        mark_local_session(&mut eventloop);
        eventloop.session_store.loaded = true;
        eventloop.reconcile_connack_session(false).unwrap();

        let err = eventloop
            .clear_persisted_session_or_block_reload()
            .await
            .unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        assert!(eventloop.session_store.clear_pending);
        assert_eq!(store.load_count(), 0);

        let err = eventloop
            .load_persisted_session_if_needed()
            .await
            .unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        assert!(eventloop.session_store.clear_pending);
        assert!(eventloop.pending_is_empty());
        assert_eq!(store.load_count(), 0);
    }

    #[test]
    fn connack_reconcile_keeps_pending_when_resumed_session_exists() {
        let mut eventloop = build_eventloop_with_pending(false);
        mark_local_session(&mut eventloop);

        eventloop.reconcile_connack_session(true).unwrap();

        assert_eq!(eventloop.pending_len(), 1);
    }

    #[tokio::test]
    async fn persist_session_includes_queued_replay_requests() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut replay_subscribe = subscribe();
        replay_subscribe.pkid = 7;
        let mut fresh_subscribe = subscribe();
        fresh_subscribe.pkid = 8;

        eventloop
            .queued
            .push_back(RequestEnvelope::plain_replay(Request::Subscribe(
                replay_subscribe,
            )));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::Subscribe(fresh_subscribe)));

        eventloop.persisted_session_save().save().await.unwrap();

        let session = store.current().expect("session should be saved");
        assert_eq!(session.replay.len(), 1);
        assert!(matches!(
            &session.replay[0],
            crate::PersistedRequest::Subscribe(subscribe) if subscribe.pkid == 7
        ));
    }

    #[tokio::test]
    async fn connack_session_expiry_override_is_effective_without_replacing_configuration() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut connack = build_connack_with_receive_max(10);
        connack.properties.as_mut().unwrap().session_expiry_interval = Some(300);

        eventloop.apply_connack_session_expiry_interval(&connack);
        eventloop.save_persisted_session().await.unwrap();

        assert_eq!(eventloop.options.session_expiry_interval(), Some(60));
        assert_eq!(eventloop.effective_session_expiry_interval, Some(300));
        assert_eq!(store.current().unwrap().session_expiry_interval, Some(300));
    }

    #[tokio::test]
    async fn persist_session_includes_pending_replay_requests_only() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut replay_subscribe = subscribe();
        replay_subscribe.pkid = 7;
        let mut fresh_subscribe = subscribe();
        fresh_subscribe.pkid = 8;

        eventloop
            .pending
            .push_back(RequestEnvelope::plain_replay(Request::Subscribe(
                replay_subscribe,
            )));
        eventloop
            .pending
            .push_back(RequestEnvelope::plain(Request::Subscribe(fresh_subscribe)));

        eventloop.persisted_session_save().save().await.unwrap();

        let session = store.current().expect("session should be saved");
        assert_eq!(session.replay.len(), 1);
        assert!(matches!(
            &session.replay[0],
            crate::PersistedRequest::Subscribe(subscribe) if subscribe.pkid == 7
        ));
    }

    #[tokio::test]
    async fn persist_session_excludes_pending_collision_retry_after_clean() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        let mut collision = Publish::new("collision/topic", QoS::AtLeastOnce, "collision", None);
        collision.pkid = 1;
        eventloop.state.collision = Some(collision);

        eventloop.clean();
        eventloop.persisted_session_save().save().await.unwrap();

        let session = store.current().expect("session should be saved");
        let publishes: Vec<_> = session
            .replay
            .iter()
            .filter_map(|request| match request {
                crate::PersistedRequest::Publish(publish) => Some(publish),
                _ => None,
            })
            .collect();
        assert_eq!(publishes.len(), 1);
        assert_eq!(publishes[0].pkid, 1);
        assert_eq!(publishes[0].topic, b"hello/world");
    }

    #[tokio::test]
    async fn successful_persisted_session_save_clears_pending_store_invalidation() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.reset_session_state();
        assert!(eventloop.session_store.clear_pending);

        let mut subscribe = subscribe();
        subscribe.pkid = 7;
        eventloop
            .queued
            .push_back(RequestEnvelope::plain_replay(Request::Subscribe(subscribe)));

        eventloop.save_persisted_session().await.unwrap();

        assert!(!eventloop.session_store.clear_pending);
        assert!(store.current().is_some());

        eventloop.queued.clear();
        eventloop.session_store.loaded = false;
        eventloop.load_persisted_session_if_needed().await.unwrap();

        assert!(store.current().is_some());
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Subscribe(subscribe)) if subscribe.pkid == 7
        ));
    }

    #[tokio::test]
    async fn reset_session_state_clears_store_before_next_load() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish(QoS::AtLeastOnce)))
            .unwrap();

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        eventloop.reset_session_state();
        eventloop.load_persisted_session_if_needed().await.unwrap();

        assert!(eventloop.pending_is_empty());
        assert!(store.current().is_none());
        assert!(eventloop.session_store.loaded);
    }

    /// MQTT-3.1.2-4: with Clean Start=1 the Client MUST discard any existing session;
    /// nothing from a persisted session is loaded or replayed.
    #[tokio::test]
    async fn clean_start_does_not_load_or_replay_persisted_session() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        // Seed a replayable subscribe into the queued set so the persisted session's
        // replay list is non-empty before we save the checkpoint.
        let mut replay_subscribe = subscribe();
        replay_subscribe.pkid = 7;
        eventloop
            .queued
            .push_back(RequestEnvelope::plain_replay(Request::Subscribe(
                replay_subscribe,
            )));

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        // Drain the current session's queues so the assertions below isolate the load path.
        eventloop.queued.clear();
        assert!(eventloop.pending_is_empty());

        // Flip to a Clean Start=1 session: the persisted checkpoint still exists in the
        // store, so a load would have replayed it without the clean_start short-circuit.
        eventloop.options.set_clean_start(true);
        eventloop.load_persisted_session_if_needed().await.unwrap();

        assert!(eventloop.pending_is_empty());
        assert!(eventloop.session_store.loaded);
        assert!(store.current().is_some());
    }

    #[tokio::test]
    async fn graceful_disconnect_clears_store_when_effective_expiry_is_zero() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        eventloop.effective_session_expiry_interval = Some(0);
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            None,
        ));

        let event = eventloop.send_pending_disconnect().await.unwrap();

        assert_eq!(event, Event::Outgoing(Outgoing::Disconnect));
        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn disconnect_now_with_expiry_zero_clears_store_after_flush() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::NormalDisconnection,
            crate::mqttbytes::v5::DisconnectProperties {
                session_expiry_interval: Some(0),
                reason_string: None,
                user_properties: Vec::new(),
                server_reference: None,
            },
        );
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::DisconnectNow(disconnect)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Stop);
        assert!(should_flush);
        assert_eq!(
            checkpoint_action,
            SessionCheckpointAction::ApplySessionExpiry(Some(0))
        );
        assert!(store.current().is_some());

        eventloop
            .flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await
            .unwrap();

        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn disconnect_now_records_session_expiry_override_after_flush() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::NormalDisconnection,
            crate::mqttbytes::v5::DisconnectProperties {
                session_expiry_interval: Some(300),
                reason_string: None,
                user_properties: Vec::new(),
                server_reference: None,
            },
        );
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::DisconnectNow(disconnect)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert!(should_flush);
        assert_eq!(
            checkpoint_action,
            SessionCheckpointAction::ApplySessionExpiry(Some(300))
        );
        assert!(store.current().is_none());

        eventloop
            .flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await
            .unwrap();

        assert_eq!(store.current().unwrap().session_expiry_interval, Some(300));
        assert_eq!(eventloop.options.session_expiry_interval(), Some(60));
    }

    #[tokio::test]
    async fn disconnect_now_does_not_record_session_expiry_override_when_flush_fails() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.network = Some(Network::new(FlushFailingIo, Some(1024)));

        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::NormalDisconnection,
            crate::mqttbytes::v5::DisconnectProperties {
                session_expiry_interval: Some(300),
                reason_string: None,
                user_properties: Vec::new(),
                server_reference: None,
            },
        );
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;
        eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::DisconnectNow(disconnect)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        let err = eventloop
            .flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(_))
        ));
        assert_eq!(store.current().unwrap().session_expiry_interval, Some(60));
    }

    #[tokio::test]
    async fn pending_disconnect_records_session_expiry_override_after_flush() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            Disconnect::new_with_properties(
                DisconnectReasonCode::NormalDisconnection,
                crate::mqttbytes::v5::DisconnectProperties {
                    session_expiry_interval: Some(300),
                    reason_string: None,
                    user_properties: Vec::new(),
                    server_reference: None,
                },
            ),
            None,
        ));

        let event = eventloop.send_pending_disconnect().await.unwrap();

        assert_eq!(event, Event::Outgoing(Outgoing::Disconnect));
        assert_eq!(store.current().unwrap().session_expiry_interval, Some(300));
        assert_eq!(eventloop.options.session_expiry_interval(), Some(60));
    }

    #[tokio::test]
    async fn abrupt_connection_loss_clears_store_when_effective_expiry_is_zero() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.save_persisted_session().await.unwrap();
        assert!(store.current().is_some());

        eventloop.effective_session_expiry_interval = Some(0);
        eventloop.checkpoint_after_connection_loss().await.unwrap();

        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn disconnect_timeout_clears_store_when_effective_expiry_is_zero() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.save_persisted_session().await.unwrap();
        assert!(store.current().is_some());

        eventloop.effective_session_expiry_interval = Some(0);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish(QoS::AtLeastOnce)))
            .unwrap();
        eventloop.state.events.clear();
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            Some(Duration::ZERO),
        ));

        let err = eventloop.poll().await.unwrap_err();

        assert!(matches!(err, ConnectionError::DisconnectTimeout));
        assert!(eventloop.disconnect_complete);
        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn disconnect_timeout_preserves_replay_checkpoint_when_expiry_is_nonzero() {
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let restore_options = options.clone();
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop
            .state
            .handle_outgoing_packet(Request::Publish(publish(QoS::AtLeastOnce)))
            .unwrap();
        eventloop.state.mark_outgoing_publishes_flush_attempted();

        // Model a previous connection loss: the complete PUBLISH now lives in a
        // replay envelope while MqttState retains its packet-id reservation.
        eventloop.clean();
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Publish(publish)) if publish.pkid == 1
        ));
        assert!(!eventloop.state.outbound_requests_drained());

        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            Disconnect::new(DisconnectReasonCode::NormalDisconnection),
            Some(Duration::ZERO),
        ));

        let err = eventloop.poll().await.unwrap_err();

        assert!(matches!(err, ConnectionError::DisconnectTimeout));
        assert!(eventloop.pending_is_empty());
        let checkpoint = store.current().expect("timeout should save the session");
        assert!(matches!(
            checkpoint.replay.as_slice(),
            [crate::PersistedRequest::Publish(publish)]
                if publish.pkid == 1 && publish.payload == b"payload"
        ));

        let (mut restored, _request_tx) = EventLoop::new_for_async_client(restore_options, 1);
        restored.load_persisted_session_if_needed().await.unwrap();

        assert!(matches!(
            pending_front_request(&restored),
            Some(Request::Publish(publish))
                if publish.pkid == 1 && publish.payload.as_ref() == b"payload" && publish.dup
        ));
    }

    #[tokio::test]
    async fn failed_post_flush_save_retries_with_disconnect_expiry_override() {
        let store = TransientSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        let err = eventloop
            .flush_network_with_checkpoint_action(SessionCheckpointAction::ApplySessionExpiry(
                Some(300),
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        assert_eq!(eventloop.effective_session_expiry_interval, Some(300));
        assert_eq!(store.current().unwrap().session_expiry_interval, Some(60));

        eventloop.checkpoint_after_connection_loss().await.unwrap();

        assert_eq!(store.current().unwrap().session_expiry_interval, Some(300));
    }

    #[tokio::test]
    async fn failed_post_flush_clear_retries_with_zero_disconnect_expiry() {
        let store = TransientSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        eventloop.save_persisted_session().await.unwrap();
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        let err = eventloop
            .flush_network_with_checkpoint_action(SessionCheckpointAction::ApplySessionExpiry(
                Some(0),
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, ConnectionError::SessionStore(_)));
        assert_eq!(eventloop.effective_session_expiry_interval, Some(0));
        assert!(store.current().is_some());

        eventloop.checkpoint_after_connection_loss().await.unwrap();

        assert!(store.current().is_none());
    }

    #[tokio::test]
    async fn disconnect_without_expiry_property_saves_store_when_connect_option_is_nonzero() {
        // MQTT-3.1.2-23: when the effective Session Expiry Interval is greater than
        // 0, the client MUST store the Session State after the Network Connection
        // closes. A DISCONNECT that omits the Session Expiry Interval property must
        // fall back to the CONNECT option (here 60),
        // so the persisted session is saved rather than cleared.
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        let disconnect = Disconnect::new(DisconnectReasonCode::NormalDisconnection);
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::DisconnectNow(disconnect)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Stop);
        assert!(should_flush);
        assert_eq!(checkpoint_action, SessionCheckpointAction::Save);

        eventloop
            .flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await
            .unwrap();

        let saved = store
            .current()
            .expect("session state must be retained when effective expiry is greater than 0");
        assert_eq!(saved.client_id, "test-client");
        assert_eq!(saved.session_expiry_interval, Some(60));
    }

    #[tokio::test]
    async fn disconnect_with_uint_max_expiry_saves_store() {
        // MQTT-3.1.2-23 read with 3.1.2.11.2: a Session Expiry Interval of
        // 0xFFFFFFFF means the Session never expires, so it must be treated as
        // greater than 0 and stored. This guards the UINT_MAX sentinel boundary
        // in the disconnect expiry decision.
        let store = CapturingSessionStore::new();
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(u32::MAX))
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));

        eventloop.persisted_session_save().save().await.unwrap();
        assert!(store.current().is_some());

        let disconnect = Disconnect::new_with_properties(
            DisconnectReasonCode::NormalDisconnection,
            crate::mqttbytes::v5::DisconnectProperties {
                session_expiry_interval: Some(u32::MAX),
                reason_string: None,
                user_properties: Vec::new(),
                server_reference: None,
            },
        );
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let keep_batching = eventloop
            .handle_request_internal(
                RequestEnvelope::plain(Request::DisconnectNow(disconnect)),
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Stop);
        assert!(should_flush);
        assert_eq!(
            checkpoint_action,
            SessionCheckpointAction::ApplySessionExpiry(Some(u32::MAX))
        );

        eventloop
            .flush_request_batch(should_flush, qos0_notices, checkpoint_action)
            .await
            .unwrap();

        let saved = store
            .current()
            .expect("session state must be retained when Session Expiry Interval is UINT_MAX");
        assert_eq!(saved.client_id, "test-client");
        assert_eq!(saved.session_expiry_interval, Some(u32::MAX));
    }

    #[tokio::test]
    async fn reconnect_replay_preserves_pending_subscribe_packet_identifier() {
        let mut eventloop = build_eventloop(false);
        eventloop
            .state
            .handle_outgoing_packet(Request::Subscribe(subscribe()))
            .unwrap();

        eventloop.clean();
        let envelope = eventloop
            .pending
            .pop_front()
            .expect("pending subscribe should be replayed after reconnect");
        assert!(envelope.replay);
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, Some(1024)));
        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        let mut checkpoint_action = SessionCheckpointAction::Save;

        let keep_batching = eventloop
            .handle_request_internal(
                envelope,
                &mut should_flush,
                &mut qos0_notices,
                &mut checkpoint_action,
            )
            .await
            .unwrap();

        assert_eq!(keep_batching, BatchControl::Continue);
        assert!(should_flush);
        assert!(qos0_notices.is_empty());
        assert!(eventloop.state.pending_subscribe.contains_key(&1));
        assert_eq!(
            eventloop.state.events.pop_front(),
            Some(Event::Outgoing(Outgoing::Subscribe(1)))
        );
    }

    #[test]
    fn connack_reconcile_keeps_incomplete_incoming_qos2_when_resumed_session_exists() {
        let mut eventloop = build_eventloop_with_pending(false);
        mark_local_session(&mut eventloop);
        let mut publish = publish(QoS::ExactlyOnce);
        publish.pkid = 7;
        eventloop
            .state
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap();
        eventloop.clean();

        eventloop.reconcile_connack_session(true).unwrap();

        let packet = eventloop
            .state
            .handle_incoming_packet(Incoming::PubRel(PubRel::new(7, None)))
            .unwrap()
            .unwrap();
        assert!(matches!(packet, Packet::PubComp(pubcomp) if pubcomp.pkid == 7));
    }

    #[test]
    fn connack_reconcile_clears_incomplete_incoming_qos2_when_resumed_session_is_missing() {
        let mut eventloop = build_eventloop_with_pending(false);
        mark_local_session(&mut eventloop);
        let mut publish = publish(QoS::ExactlyOnce);
        publish.pkid = 7;
        eventloop
            .state
            .handle_incoming_packet(Incoming::Publish(publish))
            .unwrap();
        eventloop.clean();

        eventloop.reconcile_connack_session(false).unwrap();

        let packet = eventloop
            .state
            .handle_incoming_packet(Incoming::PubRel(PubRel::new(7, None)))
            .unwrap()
            .unwrap();
        assert!(
            matches!(packet, Packet::PubComp(pubcomp) if pubcomp.pkid == 7
                && pubcomp.reason == PubCompReason::PacketIdentifierNotFound)
        );
    }

    #[test]
    fn reset_session_state_clears_local_session_marker() {
        let mut eventloop = build_eventloop(false);
        mark_local_session(&mut eventloop);

        eventloop.reset_session_state();

        assert_eq!(eventloop.session_client_id, None);
    }

    #[test]
    fn client_id_guard_keeps_state_when_client_id_is_unchanged() {
        let mut eventloop = build_eventloop_with_pending(false);
        mark_local_session(&mut eventloop);

        eventloop.reset_session_state_if_client_id_changed();

        assert_eq!(eventloop.pending_len(), 1);
        assert_eq!(eventloop.session_client_id.as_deref(), Some("test-client"));
    }

    #[test]
    fn session_key_guard_resets_state_when_store_scope_changes() {
        let mut eventloop = build_eventloop_with_pending(false);
        eventloop.options.set_session_store_scope("scope-a");
        mark_local_session(&mut eventloop);
        eventloop.options.set_session_store_scope("scope-b");

        eventloop.reset_session_state_if_client_id_changed();

        assert!(eventloop.pending_is_empty());
        assert_eq!(eventloop.session_client_id, None);
        assert_eq!(eventloop.session_store_key, None);
        assert!(!eventloop.session_store.loaded);
    }

    #[test]
    fn client_id_guard_resets_state_when_client_id_changes() {
        let mut eventloop = build_eventloop_with_pending(false);
        mark_local_session(&mut eventloop);
        eventloop.options.set_client_id("other-client".to_owned());

        eventloop.reset_session_state_if_client_id_changed();

        assert!(eventloop.pending_is_empty());
        assert_eq!(eventloop.session_client_id, None);
        assert_eq!(eventloop.session_store_key, None);
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
            Packet::PingReq
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
            Packet::PingReq
        ));
    }

    // [MQTT-3.1.2-30]: When the CONNECT carries an Authentication Method, the
    // client MUST NOT send any packet other than AUTH or DISCONNECT before it
    // receives a CONNACK. This combines the enhanced-auth CONNECT path (which
    // the *_authentication_method tests exercise without a queued user request)
    // with a queued user request (which queued_requests_are_withheld_* exercises
    // without an Authentication Method). The wire must show CONNECT, then only
    // AUTH packets during the challenge exchange, and the Subscribe must appear
    // only after CONNACK.
    //
    // The exchange is driven across multiple AUTH Continue rounds (SCRAM-style
    // multi-step auth) so the withholding guarantee holds for the whole exchange,
    // not only the first round. This guards against a future fast-path that might
    // flush pending user requests partway through the CONNECT read loop.
    #[tokio::test]
    async fn enhanced_auth_withholds_queued_user_request_until_connack() {
        const AUTH_METHOD: &str = "SCRAM-SHA-256";
        // Number of AUTH Continue challenge/response rounds before CONNACK. More
        // than one proves the withholding guarantee holds across a multi-step
        // exchange, not just the first round.
        const AUTH_ROUNDS: usize = 3;

        let (peer_tx, peer_rx) = flume::bounded(1);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_authentication_method(Some(AUTH_METHOD.to_owned()));
        // On each AUTH Continue from the broker, reply with AUTH Continue so the
        // client actually emits an AUTH packet on the wire during CONNECT.
        options.set_auth_manager(Arc::new(Mutex::new(StaticAuthManager {
            response: Ok(Some(AuthProperties {
                method: Some(AUTH_METHOD.to_owned()),
                ..Default::default()
            })),
        })));
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 1);
        // Queue a user request before the connection is established.
        request_tx
            .send(RequestEnvelope::plain(Request::Subscribe(Subscribe::new(
                SubscribeFilter::new("hello/topic", QoS::AtMostOnce),
                None,
            ))))
            .unwrap();

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();

            // First packet must be CONNECT.
            let connect_bytes = read_packet_bytes(&mut peer).await;

            // Drive several AUTH Continue challenge/response rounds before
            // CONNACK (SCRAM-style multi-step exchange). Each round the only
            // packet the client may emit is its AUTH Continue reply.
            let mut auth_reply_bytes = Vec::with_capacity(AUTH_ROUNDS);
            for _ in 0..AUTH_ROUNDS {
                let mut challenge = BytesMut::new();
                Packet::Auth(Auth::new(
                    AuthReasonCode::Continue,
                    Some(AuthProperties {
                        method: Some(AUTH_METHOD.to_owned()),
                        ..Default::default()
                    }),
                ))
                .write(&mut challenge, None)
                .unwrap();
                peer.write_all(&challenge).await.unwrap();

                // The only packet the client may emit now is its AUTH reply.
                auth_reply_bytes.push(read_packet_bytes(&mut peer).await);
            }

            // While still pre-CONNACK, no other packet (notably not the queued
            // Subscribe) may be observable on the wire.
            let no_extra_packet_before_connack =
                time::timeout(Duration::from_millis(50), read_packet_bytes(&mut peer))
                    .await
                    .is_err();

            let mut encoded_connack = BytesMut::new();
            build_connack_with_authentication_method(Some(AUTH_METHOD))
                .write(&mut encoded_connack)
                .unwrap();
            peer.write_all(&encoded_connack).await.unwrap();

            // After CONNACK the withheld Subscribe is finally sent.
            let post_connack_bytes = read_packet_bytes(&mut peer).await;

            (
                connect_bytes,
                auth_reply_bytes,
                no_extra_packet_before_connack,
                post_connack_bytes,
            )
        });

        // Drain the auth-lifecycle events and AUTH packets that the CONNECT phase
        // emits before the ConnAck. Per [MQTT-3.1.2-30], the only packets the
        // client may send before CONNACK are AUTH or DISCONNECT. The only
        // permissible pre-CONNACK events are therefore auth lifecycle events and
        // AUTH-related packets; no outgoing user activity may surface.
        //
        // Each AUTH round surfaces several auth-lifecycle/AUTH events, so the
        // bound scales with the number of rounds.
        let mut saw_connack = false;
        for _ in 0..(4 + AUTH_ROUNDS * 4) {
            match eventloop.poll().await.unwrap() {
                Event::Auth(_) => continue,
                Event::Incoming(Packet::Auth(_)) => continue,
                Event::Outgoing(Outgoing::Auth) => continue,
                Event::Incoming(Packet::ConnAck(_)) => {
                    saw_connack = true;
                    break;
                }
                other => {
                    panic!(
                        "unexpected event surfaced before CONNACK: {other:?} — \
                         [MQTT-3.1.2-30] requires that no non-AUTH/DISCONNECT \
                         packets be sent before CONNACK when Authentication Method is set"
                    )
                }
            }
        }
        assert!(saw_connack, "CONNACK was never surfaced by poll()");

        // After CONNACK the auth-success lifecycle event and the withheld Subscribe
        // are surfaced. Drain auth events first, then assert the Subscribe.
        loop {
            match eventloop.poll().await.unwrap() {
                Event::Auth(_) => continue,
                Event::Outgoing(Outgoing::Subscribe(_)) => break,
                other => panic!("unexpected post-CONNACK event: {other:?}"),
            }
        }

        let (connect_bytes, auth_reply_bytes, no_extra_packet_before_connack, post_connack_bytes) =
            broker.await.unwrap();

        let mut stream = BytesMut::from(&connect_bytes[..]);
        assert!(
            matches!(
                Packet::read(&mut stream, Some(1024)).unwrap(),
                Packet::Connect(..)
            ),
            "first packet on the wire must be CONNECT"
        );

        // Every reply across all AUTH rounds must be an AUTH Continue — never
        // the queued Subscribe or any other packet type.
        assert_eq!(
            auth_reply_bytes.len(),
            AUTH_ROUNDS,
            "expected one AUTH reply per challenge round"
        );
        for (i, reply) in auth_reply_bytes.iter().enumerate() {
            let mut stream = BytesMut::from(&reply[..]);
            assert!(
                matches!(
                    Packet::read(&mut stream, Some(1024)).unwrap(),
                    Packet::Auth(auth) if auth.code == AuthReasonCode::Continue
                ),
                "round {i}: only AUTH may be sent during the CONNECT auth exchange"
            );
        }

        assert!(
            no_extra_packet_before_connack,
            "queued Subscribe was observable on the wire before CONNACK"
        );

        let mut stream = BytesMut::from(&post_connack_bytes[..]);
        assert!(
            matches!(
                Packet::read(&mut stream, Some(1024)).unwrap(),
                Packet::Subscribe(..)
            ),
            "withheld Subscribe must be sent only after CONNACK"
        );
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

    // MQTT-3.1.3-2: The ClientID MUST be used by Clients and by Servers to identify
    // state that they hold relating to this MQTT Session. An unacked outgoing QoS 1
    // publish must survive a network failure and reconnection with the same ClientID.
    #[tokio::test]
    async fn poll_preserves_pending_state_across_reconnect_with_same_client_id() {
        let (peer_tx, peer_rx) = flume::bounded(2);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_start(false);
        options.set_keep_alive(0);
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = tokio::io::duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        // Seed an unacked QoS 1 publish in outgoing_pub, simulating a previous session.
        let mut seeded = publish(QoS::AtLeastOnce);
        seeded.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(seeded);
        eventloop.state.inflight = 1;
        mark_local_session(&mut eventloop);

        let broker = tokio::spawn(async move {
            // First connection: CONNACK with session_present=true (session resumed).
            // Broker drops the peer after sending CONNACK to simulate network failure.
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            let connack = ConnAck {
                session_present: true,
                code: ConnectReturnCode::Success,
                properties: None,
            };
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
            drop(peer);

            // Second connection: CONNACK with session_present=true (session resumed).
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            let mut encoded_connack = BytesMut::new();
            connack.write(&mut encoded_connack).unwrap();
            peer.write_all(&encoded_connack).await.unwrap();
        });

        // First poll: connects, session_present=true preserves outgoing_pub.
        let event = eventloop.poll().await.unwrap();
        assert!(matches!(
            event,
            Event::Incoming(Packet::ConnAck(ref connack)) if connack.session_present
        ));
        assert_eq!(eventloop.session_client_id.as_deref(), Some("test-client"));

        // Second poll: network read fails (peer dropped), clean() moves
        // outgoing_pub to pending.
        let err = eventloop.poll().await.unwrap_err();
        assert!(!matches!(err, ConnectionError::RequestsDone));

        // Pending state must exist after clean().
        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Publish(_))
        ));

        // Third poll: reconnects with same ClientID, session resumed.
        let event = eventloop.poll().await.unwrap();
        assert!(matches!(
            event,
            Event::Incoming(Packet::ConnAck(ref connack)) if connack.session_present
        ));

        // The unacked publish from the previous session must survive.
        assert_eq!(eventloop.pending_len(), 1);
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Publish(p)) if p.pkid == 1
        ));

        broker.await.unwrap();
    }

    // MQTT-3.1.0-2: A Client can only send the CONNECT packet once over a Network Connection.
    // The Request enum has no Connect variant, so users cannot submit a CONNECT
    // through the request channel. This is a compile-time invariant.
    #[test]
    fn request_enum_has_no_connect_variant() {
        use crate::mqttbytes::v5::{
            Auth, SubAck, SubscribeFilter, SubscribeReasonCode, UnsubAck, UnsubAckReason,
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
                SubscribeFilter::new("t", QoS::AtMostOnce),
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
            Packet::PingReq
        ));
    }
}
