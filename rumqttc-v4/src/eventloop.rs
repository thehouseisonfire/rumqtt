use crate::notice::{
    PublishNoticeTx, PublishResult, SubscribeNoticeTx, TrackedNoticeTx, UnsubscribeNoticeTx,
};
use crate::{Incoming, MqttState, NetworkOptions, Packet, Request, StateError};
use crate::{MqttOptions, Outgoing};
use crate::{NoticeFailureReason, PublishNoticeError};
use crate::{
    Transport,
    framed::{Network, ReadBatch, ReadBatchError, ReadBatchOutcome},
};

use crate::framed::AsyncReadWrite;
use crate::mqttbytes::v4::{ConnAck, Connect, ConnectReturnCode, Publish, Subscribe, Unsubscribe};
use crate::session::{PersistedSession, SessionRestoreError, SessionStore, SessionStoreError};
use flume::{Receiver, Sender, TryRecvError, bounded, unbounded};
use rumqttc_core::{OutboundScheduler, RequestClass, RequestReadiness, ScheduledRequest};
use tokio::select;
use tokio::time::{self, Instant, Sleep};

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
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
    http::HeaderValue,
};

#[cfg(all(
    any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
    feature = "websocket"
))]
use crate::websockets::split_url_with_default_port;

#[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
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
    deadline: Option<Instant>,
}

impl PendingDisconnect {
    fn new(timeout: Option<Duration>) -> Self {
        Self {
            deadline: timeout.map(|timeout| Instant::now() + timeout),
        }
    }
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
    #[cfg(feature = "websocket")]
    #[error("Invalid Url: {0}")]
    InvalidUrl(#[from] UrlError),
    #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
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
    /// Persistent session store lifecycle flags.
    session_store: SessionStoreState,
    pub network_options: NetworkOptions,
    pending_disconnect: Option<PendingDisconnect>,
    disconnect_complete: bool,
    #[cfg(feature = "tracing")]
    telemetry: crate::instrumentation::ConnectionTelemetry,
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
            self.reset_session_state_without_store_invalidation();
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
            session_store_key: None,
            session_store: SessionStoreState::new(),
            network_options: NetworkOptions::new(),
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
        for clean in self.state.clean_with_notices_for_reconnect() {
            self.pending
                .push_back(RequestEnvelope::from_parts_with_replay(
                    clean.request,
                    clean.notice,
                    clean.replay,
                ));
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

        #[cfg(feature = "tracing")]
        crate::instrumentation::replay_prepared(
            self.telemetry.connection_generation(),
            &self.diagnostics(),
        );
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
                session_store_configured: self.mqtt_options.session_store.is_some(),
                session_store_loaded: self.session_store.loaded,
                session_store_clear_pending: self.session_store.clear_pending,
                local_session_state_matches_client_id: self.session_client_id.as_deref()
                    == Some(self.mqtt_options.client_id.as_str()),
            },
            config: RuntimeConfigDiagnostics {
                configured_read_batch_size: self.mqtt_options.read_batch_size(),
                effective_read_batch_size: self.effective_read_batch_size(),
                max_request_batch: self.mqtt_options.max_request_batch(),
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
        self.state.reset_session_state();
        self.session_client_id = None;
        self.session_store_key = None;
        self.session_store.loaded = false;
        if store_reset == SessionStoreResetAction::Clear {
            self.session_store.clear_pending = true;
        }
    }

    fn reset_session_state_if_client_id_changed(&mut self) {
        let current_key = self.mqtt_options.session_store_key();
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

    async fn load_persisted_session_if_needed(&mut self) -> Result<(), ConnectionError> {
        if self.session_store.clear_pending {
            self.clear_persisted_session().await?;
            return Ok(());
        }

        if self.session_store.loaded || self.mqtt_options.clean_session() {
            self.session_store.loaded = true;
            return Ok(());
        }

        let Some(store) = self.mqtt_options.session_store() else {
            self.session_store.loaded = true;
            self.session_store.clear_pending = false;
            return Ok(());
        };

        let key = self.mqtt_options.session_store_key();
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
            .restore_persisted_session(&self.mqtt_options, &session)?;
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

    fn persisted_session_save(&self) -> SessionSave {
        if self.mqtt_options.clean_session() {
            return SessionSave(None);
        }

        let Some(store) = self.mqtt_options.session_store() else {
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
        let session = self
            .state
            .persisted_session(&self.mqtt_options, pending_replay.chain(queued_replay));
        SessionSave(Some((
            store,
            self.mqtt_options.session_store_key(),
            session,
        )))
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
        let Some(store) = self.mqtt_options.session_store() else {
            self.session_store.loaded = true;
            self.session_store.clear_pending = false;
            return Ok(());
        };

        let key = self.mqtt_options.session_store_key();
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

    async fn complete_read_batch(
        &mut self,
        batch: ReadBatch,
    ) -> Result<ReadBatchOutcome, ConnectionError> {
        let ReadBatch { outcome, notices } = batch;
        if let Err(err) = self.save_persisted_session().await {
            let error = err.to_string();
            for notice in notices {
                notice.persistence_error(error.clone());
            }
            return Err(err);
        }

        let flush_result = time::timeout(
            Duration::from_secs(self.network_options.connection_timeout()),
            self.network.as_mut().unwrap().flush(),
        )
        .await
        .map_or_else(
            |_| Err(ConnectionError::FlushTimeout),
            |inner| inner.map_err(ConnectionError::MqttState),
        );
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

    async fn establish_connection(&mut self) -> Result<Event, ConnectionError> {
        self.reset_session_state_if_client_id_changed();
        self.load_persisted_session_if_needed().await?;

        #[cfg(feature = "tracing")]
        let attempt = {
            let attempt = self.telemetry.begin_attempt();
            crate::instrumentation::connection_attempt(attempt);
            attempt
        };

        let (network, connack) = match time::timeout(
            Duration::from_secs(self.network_options.connection_timeout()),
            connect(&self.mqtt_options, self.network_options.clone()),
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
            Err(_) => {
                let error = ConnectionError::NetworkTimeout;
                #[cfg(feature = "tracing")]
                crate::instrumentation::connection_attempt_failed(attempt, "connection", &error);
                return Err(error);
            }
        };

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

        self.session_client_id = Some(self.mqtt_options.client_id());
        self.session_store_key = Some(self.mqtt_options.session_store_key());
        self.network = Some(network);

        #[cfg(feature = "tracing")]
        {
            self.telemetry.mark_connection_established();
            crate::instrumentation::connection_established(attempt, connack.session_present);
        }

        if self.keepalive_timeout.is_none() && !self.mqtt_options.keep_alive.is_zero() {
            self.keepalive_timeout = Some(Box::pin(time::sleep(self.mqtt_options.keep_alive)));
        }

        Ok(Event::Incoming(Packet::ConnAck(connack)))
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
                    self.telemetry.finish_established_connection();
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
                self.network = None;
                self.keepalive_timeout = None;
                self.pending_disconnect = None;
                self.drop_unprocessed_requests();
                self.disconnect_complete = true;
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
                self.save_persisted_session().await?;
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
            return self.establish_connection().await;
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
                    self.mqtt_options.pending_throttle
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
                    if self.keepalive_timeout.is_some() && !self.mqtt_options.keep_alive.is_zero() => {
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
        self.handle_request(envelope, &mut should_flush, &mut qos0_notices)
            .await?;
        self.flush_request_batch(should_flush, qos0_notices).await?;
        Ok(self.state.events.pop_front().unwrap())
    }

    async fn handle_ready_requests(&mut self) -> Result<bool, ConnectionError> {
        let Some(envelope) = self.next_scheduled_request() else {
            return Ok(false);
        };

        let mut should_flush = false;
        let mut qos0_notices = Vec::new();
        match self
            .handle_request(envelope, &mut should_flush, &mut qos0_notices)
            .await?
        {
            BatchControl::Continue => {
                for _ in 1..self.mqtt_options.max_request_batch.max(1) {
                    let Some(envelope) = self.next_scheduled_request() else {
                        break;
                    };

                    match self
                        .handle_request(envelope, &mut should_flush, &mut qos0_notices)
                        .await?
                    {
                        BatchControl::Continue => {}
                        BatchControl::Stop => break,
                    }
                }
            }
            BatchControl::Stop => {}
        }

        self.flush_request_batch(should_flush, qos0_notices).await?;
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
                self.mqtt_options.pending_throttle,
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

        for _ in 1..self.mqtt_options.max_request_batch.max(1) {
            let Some(envelope) = Self::try_next_request(
                &mut self.pending,
                &self.requests_rx,
                self.mqtt_options.pending_throttle,
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

    async fn handle_request(
        &mut self,
        envelope: RequestEnvelope,
        should_flush: &mut bool,
        qos0_notices: &mut Vec<PublishNoticeTx>,
    ) -> Result<BatchControl, ConnectionError> {
        let RequestEnvelope {
            request,
            notice,
            replay,
        } = envelope;
        match request {
            Request::Disconnect(_) => {
                self.pending_disconnect = Some(PendingDisconnect::new(None));
                Ok(BatchControl::Stop)
            }
            Request::DisconnectWithTimeout(_, timeout) => {
                self.pending_disconnect = Some(PendingDisconnect::new(Some(timeout)));
                Ok(BatchControl::Stop)
            }
            Request::DisconnectNow(_) => {
                let (outgoing, _) = self
                    .state
                    .handle_outgoing_packet_with_notice(request, notice)?;
                self.save_persisted_session().await?;
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
        self.pending_disconnect = None;
        let (outgoing, _) = self
            .state
            .handle_outgoing_packet_with_notice(Request::DisconnectNow(crate::Disconnect), None)?;

        if let Some(outgoing) = outgoing {
            self.network.as_mut().unwrap().write(outgoing).await?;
            self.flush_network().await?;
        }

        self.drop_unprocessed_requests();
        self.save_persisted_session().await?;
        self.disconnect_complete = true;
        Ok(self.state.events.pop_front().unwrap())
    }

    pub fn network_options(&self) -> NetworkOptions {
        self.network_options.clone()
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub fn set_network_options(&mut self, network_options: NetworkOptions) -> &mut Self {
        self.network_options = network_options;
        self
    }

    #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
    pub const fn set_network_options(&mut self, network_options: NetworkOptions) -> &mut Self {
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
    ) -> Option<RequestEnvelope> {
        if !pending.is_empty() {
            if pending_throttle.is_zero() {
                tokio::task::yield_now().await;
            } else {
                time::sleep(pending_throttle).await;
            }
            // We must call .pop_front() AFTER sleep() otherwise we would have
            // advanced the iterator but the future might be canceled before return
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
            // We must call .pop_front() AFTER sleep() otherwise we would have
            // advanced the iterator but the future might be canceled before return
            Ok(pending.pop_front().unwrap())
        }
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
        Request::Subscribe(_) | Request::Unsubscribe(_) => ScheduledRequest {
            class: RequestClass::Control,
            readiness: if can_send_control() {
                RequestReadiness::Ready
            } else {
                RequestReadiness::Blocked
            },
        },
        // All remaining request kinds are sent as ready control/default traffic.
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
    mqtt_options: &MqttOptions,
    network_options: NetworkOptions,
) -> Result<(Network, ConnAck), ConnectFailure> {
    // connect to the broker
    let mut network = network_connect(mqtt_options, network_options)
        .await
        .map_err(|error| {
            let phase = match error {
                ConnectionError::BrokerTransportMismatch => "target_setup",
                #[cfg(feature = "websocket")]
                ConnectionError::InvalidUrl(_) | ConnectionError::RequestModifier(_) => {
                    "target_setup"
                }
                _ => "transport",
            };
            ConnectFailure::new(error, phase)
        })?;

    // make MQTT connection request (which internally awaits for ack)
    let connack = mqtt_connect(mqtt_options, &mut network)
        .await
        .map_err(|error| ConnectFailure::new(error, "mqtt_handshake"))?;

    Ok((network, connack))
}

#[expect(clippy::too_many_lines)]
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
        #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
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
        #[cfg(not(any(feature = "http-proxy", feature = "socks-proxy")))]
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
            let socket = tls::tls_connect(&domain, port, &tls_config, tcp_stream).await?;
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
                .ok_or(ConnectionError::BrokerTransportMismatch)?
                .into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));

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
                .ok_or(ConnectionError::BrokerTransportMismatch)?
                .into_client_request()?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));

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
    use crate::{
        Disconnect, PersistedAckMode, PersistedPubRel, PersistedPublish, PersistedQoS,
        PersistedRequest, PersistedSession, PubAck, PubComp, PubRec, PubRel, SessionStoreError,
    };
    use bytes::BytesMut;
    use flume::TryRecvError;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, DuplexStream};

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

    #[derive(Clone, Debug)]
    struct MemorySessionStore {
        session: Arc<Mutex<Option<PersistedSession>>>,
        history: Arc<Mutex<Vec<PersistedSession>>>,
        fail_next_save: Arc<Mutex<bool>>,
    }

    impl MemorySessionStore {
        fn new(session: Option<PersistedSession>) -> Self {
            Self {
                session: Arc::new(Mutex::new(session)),
                history: Arc::new(Mutex::new(Vec::new())),
                fail_next_save: Arc::new(Mutex::new(false)),
            }
        }

        fn current(&self) -> Option<PersistedSession> {
            self.session.lock().unwrap().clone()
        }

        fn history(&self) -> Vec<PersistedSession> {
            self.history.lock().unwrap().clone()
        }

        fn fail_next_save(&self) {
            *self.fail_next_save.lock().unwrap() = true;
        }
    }

    impl SessionStore for MemorySessionStore {
        fn load<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<PersistedSession>, SessionStoreError>>
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
        ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
            Box::pin(async move {
                if std::mem::take(&mut *self.fail_next_save.lock().unwrap()) {
                    return Err(
                        Box::new(std::io::Error::other("session save failed")) as SessionStoreError
                    );
                }
                *self.session.lock().unwrap() = Some(session.clone());
                self.history.lock().unwrap().push(session.clone());
                Ok(())
            })
        }

        fn clear<'a>(
            &'a self,
            _key: &'a crate::SessionStoreKey,
        ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
            Box::pin(async {
                *self.session.lock().unwrap() = None;
                self.history.lock().unwrap().clear();
                Ok(())
            })
        }
    }

    fn mark_local_session(eventloop: &mut EventLoop) {
        eventloop.session_client_id = Some(eventloop.mqtt_options.client_id());
        eventloop.session_store_key = Some(eventloop.mqtt_options.session_store_key());
    }

    fn persisted_qos1_session(client_id: &str) -> PersistedSession {
        PersistedSession {
            format_version: 1,
            client_id: client_id.to_owned(),
            clean_session: false,
            max_inflight: 100,
            ack_mode: PersistedAckMode::Automatic,
            last_pkid: 1,
            last_puback: 0,
            replay: vec![PersistedRequest::Publish(PersistedPublish {
                dup: true,
                qos: PersistedQoS::AtLeastOnce,
                retain: false,
                topic: b"hello/world".to_vec(),
                pkid: 1,
                payload: b"payload".to_vec(),
            })],
            incoming_qos2: vec![],
        }
    }

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

    #[tokio::test]
    async fn admission_checkpoint_is_duplicate_but_first_wire_publish_is_not() {
        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let store = MemorySessionStore::new(None);
            let mut options = MqttOptions::new("test-client", "localhost");
            options
                .set_clean_session(false)
                .set_session_store(store.clone());
            let restore_options = options.clone();
            let (mut eventloop, _) = EventLoop::new_for_async_client(options, 4);
            let (client, mut peer) = tokio::io::duplex(1024);
            eventloop.network = Some(Network::new(client, 1024, 1024));

            let mut should_flush = false;
            let mut qos0_notices = Vec::new();
            eventloop
                .handle_request(
                    RequestEnvelope::plain(Request::Publish(Publish::new(
                        "checkpoint/live",
                        qos,
                        vec![1, 2, 3],
                    ))),
                    &mut should_flush,
                    &mut qos0_notices,
                )
                .await
                .unwrap();

            let checkpoints = store.history();
            assert_eq!(
                checkpoints.len(),
                1,
                "admission must be the only pre-ack save"
            );
            let decoded = PersistedSession::decode(&checkpoints[0].encode().unwrap()).unwrap();
            let persisted = decoded
                .replay
                .iter()
                .find_map(|request| match request {
                    PersistedRequest::Publish(publish) => Some(publish),
                    _ => None,
                })
                .expect("admission checkpoint must contain the publish");
            assert!(persisted.dup);
            assert_ne!(persisted.pkid, 0);

            // The live packet has been fed, but the explicit batch flush has not happened.
            // The admission checkpoint alone must already be a valid replay.
            let mut restored_before_flush = MqttState::builder(4).build();
            let replay = restored_before_flush
                .restore_persisted_session(&restore_options, &decoded)
                .unwrap();
            assert!(matches!(&replay[0], Request::Publish(publish) if publish.dup));
            assert_eq!(
                restored_before_flush
                    .outbound_diagnostics()
                    .packet_identifiers_in_use,
                1
            );

            eventloop
                .flush_request_batch(should_flush, qos0_notices)
                .await
                .unwrap();
            assert_eq!(
                store.history().len(),
                1,
                "flush must not promote DUP with another save"
            );

            let mut bytes = BytesMut::from(read_packet_bytes(&mut peer).await.as_slice());
            match Packet::read(&mut bytes, 1024).unwrap() {
                Packet::Publish(publish) => {
                    assert!(!publish.dup);
                    assert_eq!(publish.qos, qos);
                    assert_eq!(publish.pkid, persisted.pkid);
                }
                packet => panic!("expected live PUBLISH, got {packet:?}"),
            }

            let mut restored_after_flush = MqttState::builder(4).build();
            let replay = restored_after_flush
                .restore_persisted_session(&restore_options, &decoded)
                .unwrap();
            assert!(matches!(&replay[0], Request::Publish(publish) if publish.dup));
            assert_eq!(
                restored_after_flush
                    .outbound_diagnostics()
                    .packet_identifiers_in_use,
                1
            );
        }
    }

    #[tokio::test]
    async fn publish_batch_has_conservative_checkpoints_without_flush_save() {
        let store = MemorySessionStore::new(None);
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_max_request_batch(3)
            .set_session_store(store.clone());
        let (mut eventloop, _) = EventLoop::new_for_async_client(options, 4);
        let (client, mut peer) = tokio::io::duplex(4096);
        eventloop.network = Some(Network::new(client, 4096, 4096));

        for index in 0_u8..3 {
            eventloop
                .queued
                .push_back(RequestEnvelope::plain(Request::Publish(Publish::new(
                    format!("batch/{index}"),
                    QoS::AtLeastOnce,
                    vec![index; usize::from(index) + 1],
                ))));
        }

        assert!(eventloop.handle_ready_requests().await.unwrap());
        let checkpoints = store.history();
        assert_eq!(checkpoints.len(), 3);
        let publishes: Vec<_> = checkpoints
            .last()
            .unwrap()
            .replay
            .iter()
            .filter_map(|request| match request {
                PersistedRequest::Publish(publish) => Some(publish),
                _ => None,
            })
            .collect();
        assert_eq!(publishes.len(), 3);
        assert!(publishes.iter().all(|publish| publish.dup));
        let mut pkids: Vec<_> = publishes.iter().map(|publish| publish.pkid).collect();
        pkids.sort_unstable();
        pkids.dedup();
        assert_eq!(pkids.len(), 3);
        assert_eq!(
            eventloop
                .state
                .outbound_diagnostics()
                .packet_identifiers_in_use,
            3
        );

        for _ in 0..3 {
            let mut bytes = BytesMut::from(read_packet_bytes(&mut peer).await.as_slice());
            assert!(matches!(
                Packet::read(&mut bytes, 4096).unwrap(),
                Packet::Publish(publish) if !publish.dup
            ));
        }
    }

    #[tokio::test]
    async fn readiness_can_advance_earlier_publish_only_after_its_checkpoint() {
        use std::io;
        use std::task::{Context, Poll};
        use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

        struct ObservingIo {
            checkpoints: Arc<Mutex<Vec<PersistedSession>>>,
            observed_checkpoint_counts: Arc<Mutex<Vec<usize>>>,
        }

        impl AsyncRead for ObservingIo {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Pending
            }
        }

        impl AsyncWrite for ObservingIo {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                self.observed_checkpoint_counts
                    .lock()
                    .unwrap()
                    .push(self.checkpoints.lock().unwrap().len());
                Poll::Ready(Ok(buf.len()))
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }

        let store = MemorySessionStore::new(None);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let io = ObservingIo {
            checkpoints: Arc::clone(&store.history),
            observed_checkpoint_counts: Arc::clone(&observed),
        };
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_max_request_batch(2)
            .set_session_store(store.clone());
        let (mut eventloop, _) = EventLoop::new_for_async_client(options, 2);
        let mut network = Network::new(io, 4096, 4096);
        network.set_backpressure_boundary(1);
        eventloop.network = Some(network);
        for topic in ["ready/first", "ready/second"] {
            eventloop
                .queued
                .push_back(RequestEnvelope::plain(Request::Publish(Publish::new(
                    topic,
                    QoS::AtLeastOnce,
                    vec![1],
                ))));
        }

        eventloop.handle_ready_requests().await.unwrap();

        let observed = observed.lock().unwrap();
        assert!(!observed.is_empty());
        assert!(observed.iter().all(|count| *count >= 1));
        assert!(
            store.history()[0].replay.iter().any(
                |request| matches!(request, PersistedRequest::Publish(publish) if publish.dup)
            )
        );
    }

    #[tokio::test]
    async fn isolated_qos_exchanges_keep_terminal_and_pubrel_save_counts() {
        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let store = MemorySessionStore::new(None);
            let mut options = MqttOptions::new("test-client", "localhost");
            options
                .set_clean_session(false)
                .set_session_store(store.clone());
            let (mut eventloop, _) = EventLoop::new_for_async_client(options, 1);
            let (client, _peer) = tokio::io::duplex(4096);
            eventloop.network = Some(Network::new(client, 4096, 4096));

            let mut should_flush = false;
            let mut qos0_notices = Vec::new();
            eventloop
                .handle_request(
                    RequestEnvelope::plain(Request::Publish(Publish::new(
                        "save/count",
                        qos,
                        vec![1],
                    ))),
                    &mut should_flush,
                    &mut qos0_notices,
                )
                .await
                .unwrap();
            eventloop
                .flush_request_batch(should_flush, qos0_notices)
                .await
                .unwrap();
            assert_eq!(store.history().len(), 1);

            if qos == QoS::AtLeastOnce {
                let effects = eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubAck(PubAck::new(1)))
                    .unwrap();
                eventloop
                    .complete_read_batch(ReadBatch {
                        outcome: ReadBatchOutcome::NoResponseWritten,
                        notices: effects.notices,
                    })
                    .await
                    .unwrap();
                assert_eq!(store.history().len(), 2);
            } else {
                let effects = eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubRec(PubRec::new(1)))
                    .unwrap();
                eventloop
                    .network
                    .as_mut()
                    .unwrap()
                    .write(effects.outgoing.expect("PUBREC must produce PUBREL"))
                    .await
                    .unwrap();
                eventloop
                    .complete_read_batch(ReadBatch {
                        outcome: ReadBatchOutcome::ResponseWritten,
                        notices: effects.notices,
                    })
                    .await
                    .unwrap();
                assert_eq!(store.history().len(), 2);
                assert!(matches!(
                    store.history()[1].replay.as_slice(),
                    [PersistedRequest::PubRel(PersistedPubRel { pkid: 1 })]
                ));

                let effects = eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubComp(PubComp::new(1)))
                    .unwrap();
                eventloop
                    .complete_read_batch(ReadBatch {
                        outcome: ReadBatchOutcome::NoResponseWritten,
                        notices: effects.notices,
                    })
                    .await
                    .unwrap();
                assert_eq!(store.history().len(), 3);
            }

            assert!(store.current().unwrap().replay.is_empty());
            assert_eq!(
                eventloop
                    .state
                    .outbound_diagnostics()
                    .packet_identifiers_in_use,
                0
            );
        }
    }

    #[tokio::test]
    async fn terminal_save_failure_keeps_durable_owner_and_blocks_notice() {
        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let store = MemorySessionStore::new(None);
            let mut options = MqttOptions::new("test-client", "localhost");
            options
                .set_clean_session(false)
                .set_session_store(store.clone());
            let (mut eventloop, _) = EventLoop::new_for_async_client(options, 1);
            let (client, _peer) = tokio::io::duplex(4096);
            eventloop.network = Some(Network::new(client, 4096, 4096));
            let (notice_tx, notice) = PublishNoticeTx::new();

            let mut should_flush = false;
            let mut qos0_notices = Vec::new();
            eventloop
                .handle_request(
                    RequestEnvelope::tracked_publish(
                        Publish::new("terminal/failure", qos, vec![1]),
                        notice_tx,
                    ),
                    &mut should_flush,
                    &mut qos0_notices,
                )
                .await
                .unwrap();
            eventloop
                .flush_request_batch(should_flush, qos0_notices)
                .await
                .unwrap();

            if qos == QoS::ExactlyOnce {
                let effects = eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubRec(PubRec::new(1)))
                    .unwrap();
                eventloop
                    .network
                    .as_mut()
                    .unwrap()
                    .write(effects.outgoing.unwrap())
                    .await
                    .unwrap();
                eventloop
                    .complete_read_batch(ReadBatch {
                        outcome: ReadBatchOutcome::ResponseWritten,
                        notices: effects.notices,
                    })
                    .await
                    .unwrap();
            }

            let effects = if qos == QoS::AtLeastOnce {
                eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubAck(PubAck::new(1)))
                    .unwrap()
            } else {
                eventloop
                    .state
                    .handle_incoming_packet_with_effects(Incoming::PubComp(PubComp::new(1)))
                    .unwrap()
            };
            store.fail_next_save();
            assert!(
                eventloop
                    .complete_read_batch(ReadBatch {
                        outcome: ReadBatchOutcome::NoResponseWritten,
                        notices: effects.notices,
                    })
                    .await
                    .is_err()
            );

            assert!(matches!(
                notice.wait_async().await,
                Err(PublishNoticeError::SessionPersistence(_))
            ));
            let checkpoint = store.current().unwrap();
            if qos == QoS::AtLeastOnce {
                assert!(matches!(
                    checkpoint.replay.as_slice(),
                    [PersistedRequest::Publish(publish)] if publish.pkid == 1
                ));
            } else {
                assert!(matches!(
                    checkpoint.replay.as_slice(),
                    [PersistedRequest::PubRel(PersistedPubRel { pkid: 1 })]
                ));
            }
        }
    }

    #[tokio::test]
    async fn flush_failure_needs_no_dup_promotion_checkpoint() {
        use std::io;
        use std::task::{Context, Poll};
        use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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

        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let store = MemorySessionStore::new(None);
            let mut options = MqttOptions::new("test-client", "localhost");
            options
                .set_clean_session(false)
                .set_session_store(store.clone());
            let restore_options = options.clone();
            let (mut eventloop, _) = EventLoop::new_for_async_client(options, 1);
            eventloop.network = Some(Network::new(FlushFailingIo, 1024, 1024));
            let mut should_flush = false;
            let mut qos0_notices = Vec::new();
            eventloop
                .handle_request(
                    RequestEnvelope::plain(Request::Publish(Publish::new(
                        "flush/failure",
                        qos,
                        vec![1],
                    ))),
                    &mut should_flush,
                    &mut qos0_notices,
                )
                .await
                .unwrap();

            assert!(
                eventloop
                    .flush_request_batch(should_flush, qos0_notices)
                    .await
                    .is_err()
            );
            assert_eq!(store.history().len(), 1);
            let checkpoint = store.current().unwrap();
            assert!(matches!(
                checkpoint.replay.as_slice(),
                [PersistedRequest::Publish(publish)] if publish.dup && publish.pkid == 1
            ));
            let mut restored = MqttState::builder(1).build();
            let replay = restored
                .restore_persisted_session(&restore_options, &checkpoint)
                .unwrap();
            assert!(matches!(&replay[0], Request::Publish(publish) if publish.dup));
            assert_eq!(restored.outbound_diagnostics().packet_identifiers_in_use, 1);
        }
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
        push_pending(&mut eventloop, Request::PingReq);
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
        assert_eq!(
            diagnostics.config.configured_read_batch_size,
            eventloop.mqtt_options.read_batch_size()
        );
        assert_eq!(
            diagnostics.config.effective_read_batch_size,
            eventloop.effective_read_batch_size()
        );
        assert_eq!(
            diagnostics.config.max_request_batch,
            eventloop.mqtt_options.max_request_batch()
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
            .try_send(RequestEnvelope::plain(Request::DisconnectNow(Disconnect)))
            .unwrap();
        push_pending(&mut eventloop, Request::PingReq);
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PingReq));
        let (client, _peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(None));
        eventloop.session_client_id = Some("test-client".to_owned());
        eventloop.session_store.loaded = true;

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
    }

    #[cfg(feature = "stream")]
    fn eventloop_with_network() -> (EventLoop, Sender<RequestEnvelope>, DuplexStream) {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (client, peer) = tokio::io::duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));
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
        use tokio::io::{AsyncWriteExt, duplex};

        let (peer_tx, peer_rx) = flume::bounded(2);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_session(false);
        options.set_keep_alive(0);
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = duplex(1024);
                peer_tx.send(peer).unwrap();
                Ok(client)
            }
        });

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        let mut seeded = publish(QoS::AtLeastOnce);
        seeded.pkid = 1;
        eventloop.state.outgoing_pub[1] = Some(seeded);
        eventloop.state.inflight = 1;

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x01, 0x00]).await.unwrap();
            drop(peer);

            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x01, 0x00]).await.unwrap();
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
            .map(|envelope| envelope.request)
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

        let request = eventloop.next_scheduled_request().unwrap().request;

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
            next_after_blocked_publish(Request::PingReq),
            Some(Request::PingReq)
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

        let request = eventloop.next_scheduled_request().unwrap().request;

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

        let request = eventloop.next_scheduled_request().unwrap().request;

        assert!(matches!(request, Request::Publish(_)));
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
        eventloop.network = Some(Network::new(client, 64, 64));
        eventloop
            .state
            .events
            .push_back(Event::Outgoing(Outgoing::PingResp));
        immediate_disconnect_tx
            .send_async(RequestEnvelope::plain(Request::DisconnectNow(Disconnect)))
            .await
            .unwrap();

        let buffered = eventloop.select().await.unwrap();
        let disconnect = eventloop.select().await.unwrap();

        assert_eq!(buffered, Event::Outgoing(Outgoing::PingResp));
        assert_eq!(disconnect, Event::Outgoing(Outgoing::Disconnect));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finite_request_flood_eventually_allows_ready_network_read() {
        use tokio::io::AsyncWriteExt;

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
        eventloop.network = Some(Network::new(client, 64 * 1024, 64 * 1024));

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
            .push_back(RequestEnvelope::plain(Request::PubAck(PubAck::new(7))));
        eventloop
            .queued
            .push_back(RequestEnvelope::plain(Request::PubRec(PubRec::new(8))));
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
        push_pending(&mut eventloop, Request::PingReq);
        drop(request_tx);

        let request = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(request.request, Request::PingReq));

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
        push_pending(&mut eventloop, Request::Disconnect(Disconnect));

        let first = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(first.request, Request::PingReq));

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
                ..
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
        assert!(matches!(first.request, Request::PingReq));
        assert!(eventloop.pending.is_empty());

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(second.request, Request::PingReq));
    }

    #[tokio::test]
    async fn next_request_preserves_fifo_order_for_plain_and_tracked_requests() {
        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, request_tx) = EventLoop::new_for_async_client(options, 4);
        let (notice_tx, _notice) = PublishNoticeTx::new();
        let tracked_publish =
            Publish::new("hello/world", crate::mqttbytes::QoS::AtLeastOnce, "payload");

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
        assert!(matches!(first.request, Request::PingReq));

        let second = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(
            second,
            RequestEnvelope { request: Request::Publish(ref publish), notice: Some(_), .. } if *publish == tracked_publish
        ));

        let third = EventLoop::next_request(
            &mut eventloop.pending,
            &eventloop.requests_rx,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(matches!(third.request, Request::Disconnect(_)));
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
    async fn poll_drops_network_after_incoming_packet_with_invalid_fixed_header_flags() {
        use crate::mqttbytes::Error as MqttError;
        use tokio::io::{AsyncWriteExt, duplex};

        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, mut peer) = duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));

        peer.write_all(&[
            0x11, 0x00, // CONNECT with invalid fixed-header flags
        ])
        .await
        .unwrap();

        let err = eventloop.poll().await.unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(
                MqttError::IncorrectPacketFormat
            ))
        ));
        assert!(
            eventloop.network.is_none(),
            "poll() must drop the network after incoming invalid fixed-header flags"
        );
    }

    #[tokio::test]
    async fn poll_drops_network_after_incoming_publish_with_malformed_utf8_topic() {
        use crate::mqttbytes::Error as MqttError;
        use tokio::io::{AsyncWriteExt, duplex};

        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, mut peer) = duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));

        peer.write_all(&[
            0b0011_0000,
            5, // packet type, flags and remaining len
            0x00,
            0x03, // topic length = 3
            0xED,
            0xA0,
            0x80, // topic = U+D800 encoded as CESU-8
        ])
        .await
        .unwrap();

        let err = eventloop.poll().await.unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(MqttError::TopicNotUtf8 { .. }))
        ));
        assert!(
            eventloop.network.is_none(),
            "poll() must drop the network after an incoming PUBLISH with malformed UTF-8"
        );
    }

    #[tokio::test]
    async fn poll_drops_network_after_incoming_publish_with_null_character_in_topic() {
        use crate::mqttbytes::Error as MqttError;
        use tokio::io::{AsyncWriteExt, duplex};

        let options = MqttOptions::new("test-client", "localhost");
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let (client, mut peer) = duplex(1024);
        eventloop.network = Some(Network::new(client, 1024, 1024));

        peer.write_all(&[
            0b0011_0000,
            5, // packet type, flags and remaining len
            0x00,
            0x03, // topic length = 3
            b'a',
            0x00,
            b'b',
        ])
        .await
        .unwrap();

        let err = eventloop.poll().await.unwrap_err();
        assert!(matches!(
            err,
            ConnectionError::MqttState(StateError::Deserialization(MqttError::MalformedPacket))
        ));
        assert!(
            eventloop.network.is_none(),
            "poll() must drop the network after an incoming PUBLISH with U+0000 in the topic"
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

    #[tokio::test]
    async fn persisted_session_store_restores_qos1_replay() {
        let store = MemorySessionStore::new(Some(persisted_qos1_session("test-client")));
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.load_persisted_session_if_needed().await.unwrap();

        assert!(eventloop.session_store.loaded);
        assert_eq!(eventloop.session_client_id, Some("test-client".to_owned()));
        assert!(matches!(
            pending_front_request(&eventloop),
            Some(Request::Publish(publish)) if publish.pkid == 1 && publish.dup
        ));
    }

    #[tokio::test]
    async fn persisted_session_store_is_cleared_when_broker_has_no_session() {
        let store = MemorySessionStore::new(Some(persisted_qos1_session("test-client")));
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.load_persisted_session_if_needed().await.unwrap();
        assert!(!eventloop.pending_is_empty());

        eventloop.reconcile_connack_session(false).unwrap();
        eventloop
            .clear_persisted_session_or_block_reload()
            .await
            .unwrap();

        assert!(eventloop.pending_is_empty());
        assert!(store.current().is_none());
        assert!(eventloop.session_store.loaded);
        assert!(!eventloop.session_store.clear_pending);
    }

    #[tokio::test]
    async fn persist_session_includes_queued_replay_requests() {
        let store = MemorySessionStore::new(None);
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
        subscribe.pkid = 7;
        eventloop
            .queued
            .push_back(RequestEnvelope::plain_replay(Request::Subscribe(subscribe)));

        eventloop.persisted_session_save().save().await.unwrap();

        let session = store.current().expect("session should be saved");
        assert!(session.replay.iter().any(|request| matches!(
            request,
            PersistedRequest::Subscribe(subscribe) if subscribe.pkid == 7
        )));
    }

    #[tokio::test]
    async fn persist_session_includes_pending_replay_requests_only() {
        let store = MemorySessionStore::new(None);
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        let mut replay_subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
        replay_subscribe.pkid = 7;
        let mut fresh_subscribe = Subscribe::new("c/d", QoS::AtMostOnce);
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
            PersistedRequest::Subscribe(subscribe) if subscribe.pkid == 7
        ));
    }

    #[tokio::test]
    async fn persist_session_excludes_pending_collision_retry_after_clean() {
        let store = MemorySessionStore::new(None);
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        fill_publish_window(&mut eventloop);
        let mut collision = Publish::new("collision/topic", QoS::AtLeastOnce, "collision");
        collision.pkid = 1;
        eventloop.state.collision = Some(collision);

        eventloop.clean();
        eventloop.persisted_session_save().save().await.unwrap();

        let session = store.current().expect("session should be saved");
        let publishes: Vec<_> = session
            .replay
            .iter()
            .filter_map(|request| match request {
                PersistedRequest::Publish(publish) => Some(publish),
                _ => None,
            })
            .collect();
        assert_eq!(publishes.len(), 1);
        assert_eq!(publishes[0].pkid, 1);
        assert_eq!(publishes[0].topic, b"hello/world");
    }

    #[tokio::test]
    async fn successful_persisted_session_save_clears_pending_store_invalidation() {
        let store = MemorySessionStore::new(Some(persisted_qos1_session("test-client")));
        let mut options = MqttOptions::new("test-client", "localhost");
        options
            .set_clean_session(false)
            .set_session_store(store.clone());
        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);

        eventloop.reset_session_state();
        assert!(eventloop.session_store.clear_pending);

        let mut subscribe = Subscribe::new("a/b", QoS::AtMostOnce);
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

    #[test]
    fn connack_reconcile_keeps_incomplete_incoming_qos2_when_resumed_session_exists() {
        let mut eventloop = build_eventloop_with_pending(false);
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
            .handle_incoming_packet(Incoming::PubRel(PubRel::new(7)))
            .unwrap()
            .unwrap();
        assert!(matches!(packet, Packet::PubComp(pubcomp) if pubcomp.pkid == 7));
    }

    #[test]
    fn connack_reconcile_clears_incomplete_incoming_qos2_when_resumed_session_is_missing() {
        let mut eventloop = build_eventloop_with_pending(false);
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
            .handle_incoming_packet(Incoming::PubRel(PubRel::new(7)))
            .unwrap()
            .unwrap();
        assert!(matches!(packet, Packet::PubComp(pubcomp) if pubcomp.pkid == 7));
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
        eventloop.mqtt_options.set_session_store_scope("scope-a");
        mark_local_session(&mut eventloop);
        eventloop.mqtt_options.set_session_store_scope("scope-b");

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
        eventloop
            .mqtt_options
            .set_client_id("other-client".to_owned());

        eventloop.reset_session_state_if_client_id_changed();

        assert!(eventloop.pending_is_empty());
        assert_eq!(eventloop.session_client_id, None);
        assert_eq!(eventloop.session_store_key, None);
    }

    // MQTT-3.1.3-2: The ClientId MUST be used by Clients and by Servers to identify
    // state that they hold relating to this MQTT Session.
    #[tokio::test]
    async fn poll_sets_session_client_id_after_successful_connect() {
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

        let (mut eventloop, _request_tx) = EventLoop::new_for_async_client(options, 1);
        assert_eq!(eventloop.session_client_id, None);

        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x00, 0x00]).await.unwrap();
        });

        let event = eventloop.poll().await.unwrap();
        assert!(matches!(
            event,
            Event::Incoming(Packet::ConnAck(ref connack)) if !connack.session_present
        ));
        assert_eq!(eventloop.session_client_id, Some("test-client".to_owned()));

        broker.await.unwrap();
    }

    // MQTT-3.1.3-2: The ClientId MUST be used by Clients and by Servers to identify
    // state that they hold relating to this MQTT Session. An unacked outgoing QoS 1
    // publish must survive a network failure and reconnection with the same ClientId.
    #[tokio::test]
    async fn poll_preserves_pending_state_across_reconnect_with_same_client_id() {
        use tokio::io::{AsyncWriteExt, duplex};

        let (peer_tx, peer_rx) = flume::bounded(2);
        let mut options = MqttOptions::new("test-client", "localhost");
        options.set_clean_session(false);
        options.set_keep_alive(0);
        options.set_socket_connector(move |_host, _network_options| {
            let peer_tx = peer_tx.clone();
            async move {
                let (client, peer) = duplex(1024);
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

        // First connection: CONNACK with session_present=true (session resumed).
        // Broker drops the peer after sending CONNACK to simulate network failure.
        let broker = tokio::spawn(async move {
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            // ConnAck: type=0x20, remaining_len=2, session_present=1, return_code=0
            peer.write_all(&[0x20, 0x02, 0x01, 0x00]).await.unwrap();
            drop(peer);

            // Second connection: CONNACK with session_present=true (session resumed).
            let mut peer = peer_rx.recv_async().await.unwrap();
            let _connect = read_packet_bytes(&mut peer).await;
            peer.write_all(&[0x20, 0x02, 0x01, 0x00]).await.unwrap();
        });

        // First poll: connects, session_present=true preserves outgoing_pub.
        let event = eventloop.poll().await.unwrap();
        assert!(matches!(
            event,
            Event::Incoming(Packet::ConnAck(ref connack)) if connack.session_present
        ));
        assert_eq!(eventloop.session_client_id, Some("test-client".to_owned()));

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

        // Third poll: reconnects with same ClientId, session resumed.
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
            .send(RequestEnvelope::plain(Request::PingReq))
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
        use crate::mqttbytes::v4::{SubAck, UnsubAck};
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
            discriminant(&Request::PingReq),
            discriminant(&Request::PingResp),
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
            .send(RequestEnvelope::plain(Request::PingReq))
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
