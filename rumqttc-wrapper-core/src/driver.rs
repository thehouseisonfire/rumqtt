use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use flume::{Receiver, Sender};
use futures_util::stream::{FuturesUnordered, StreamExt};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::Notify;
use tokio_rustls::rustls::{ClientConfig as RustlsClientConfig, RootCertStore};

use crate::completion::{
    BrokerReason, SubscribeCompletion, SubscribeResult, UnsubscribeCompletion, UnsubscribeResult,
};
use crate::{
    AckMode, AckToken, Admission, ClientConfig, Command, Completion, CompletionHandle,
    ConnectionPhase, DeliveryStatus, DiagnosticsSnapshot, Error, ErrorKind, IncomingPublish,
    LifecycleState, OperationId, OutgoingActivity, ProtocolConfig, ProtocolVersion, PublishCommand,
    PublishCompletion, QoS, Result, SubscribeCommand, TlsConfig, TransportConfig,
    V5PublishProperties, WrapperEvent,
};

type CompletionFuture = Pin<Box<dyn Future<Output = Result<Completion>> + Send + 'static>>;
type PendingFuture = Pin<Box<dyn Future<Output = (OperationId, Result<Completion>)> + Send>>;

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

enum ProtocolClient {
    V311(rumqttc_v4::AsyncClient),
    V5(rumqttc_v5::AsyncClient),
}

enum PreparedAck {
    V311(rumqttc_v4::ManualAck),
    V5(rumqttc_v5::ManualAck),
}

impl PreparedAck {
    const fn key(&self) -> AckKey {
        match self {
            Self::V311(rumqttc_v4::ManualAck::PubAck(ack)) => AckKey::V311PubAck(ack.pkid),
            Self::V311(rumqttc_v4::ManualAck::PubRec(ack)) => AckKey::V311PubRec(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubAck(ack)) => AckKey::V5PubAck(ack.pkid),
            Self::V5(rumqttc_v5::ManualAck::PubRec(ack)) => AckKey::V5PubRec(ack.pkid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AckKey {
    V311PubAck(u16),
    V311PubRec(u16),
    V5PubAck(u16),
    V5PubRec(u16),
}

struct CompletionRegistration {
    operation_id: OperationId,
    sender: Sender<Result<Completion>>,
    future: CompletionFuture,
    shutdown_completion: Option<Completion>,
}

struct DiagnosticsRequest {
    sender: Sender<Result<Completion>>,
}

struct PendingSender {
    sender: Sender<Result<Completion>>,
    shutdown_completion: Option<Completion>,
}

#[derive(Default)]
struct AcknowledgementRegistry {
    by_token: HashMap<AckToken, PreparedAck>,
    by_key: HashMap<AckKey, AckToken>,
}

impl AcknowledgementRegistry {
    fn clear(&mut self) {
        self.by_token.clear();
        self.by_key.clear();
    }

    fn insert(&mut self, token: AckToken, ack: PreparedAck) {
        let key = ack.key();
        self.by_token.insert(token, ack);
        self.by_key.insert(key, token);
    }

    fn remove(&mut self, token: &AckToken) -> Option<PreparedAck> {
        let ack = self.by_token.remove(token)?;
        self.by_key.remove(&ack.key());
        Some(ack)
    }

    fn token(&self, key: AckKey) -> Option<AckToken> {
        self.by_key.get(&key).copied()
    }
}

struct Shared {
    client: ProtocolClient,
    client_identity: u64,
    connection_generation: AtomicU64,
    broker_topic_alias_max: AtomicU16,
    broker_maximum_qos: AtomicU8,
    broker_retain_available: AtomicBool,
    broker_capabilities_known: AtomicBool,
    next_ack_serial: AtomicU64,
    next_operation: AtomicU64,
    lifecycle: AtomicU8,
    shutdown_kind: AtomicU8,
    shutdown_operation: AtomicU64,
    shutdown_registration_ready: AtomicBool,
    handle_count: AtomicUsize,
    admission_gate: Mutex<()>,
    acknowledgements: Mutex<AcknowledgementRegistry>,
    acknowledgement_completions: Mutex<HashMap<AckKey, Sender<Result<Completion>>>>,
    outbound_topic_aliases: Mutex<HashMap<u16, String>>,
    completion_tx: Sender<CompletionRegistration>,
    diagnostics_tx: Sender<DiagnosticsRequest>,
    immediate_shutdown_tx: Sender<()>,
    request_progress: Notify,
}

#[derive(Clone, Copy)]
struct V5PublishCapabilities {
    topic_alias_max: u16,
    maximum_qos: u8,
    retain_available: bool,
    known: bool,
}

struct AckReservation {
    shared: Arc<Shared>,
    token: AckToken,
    ack: Option<PreparedAck>,
}

impl AckReservation {
    const fn ack(&self) -> &PreparedAck {
        self.ack.as_ref().expect("active ACK reservation")
    }

    fn commit(mut self) {
        self.ack = None;
    }
}

impl Drop for AckReservation {
    fn drop(&mut self) {
        if let Some(ack) = self.ack.take() {
            self.shared
                .acknowledgements
                .lock()
                .expect("acknowledgement map mutex poisoned")
                .insert(self.token, ack);
        }
    }
}

impl Shared {
    fn v5_publish_capabilities(&self) -> V5PublishCapabilities {
        V5PublishCapabilities {
            topic_alias_max: self.broker_topic_alias_max.load(Ordering::Acquire),
            maximum_qos: self.broker_maximum_qos.load(Ordering::Acquire),
            retain_available: self.broker_retain_available.load(Ordering::Acquire),
            known: self.broker_capabilities_known.load(Ordering::Acquire),
        }
    }

    fn immediate_shutdown_requested(&self) -> bool {
        self.shutdown_kind.load(Ordering::Acquire) == 2
    }

    fn state(&self) -> LifecycleState {
        LifecycleState::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    fn require_running(&self) -> Result<()> {
        if self.state() == LifecycleState::Running {
            Ok(())
        } else {
            Err(
                Error::new(ErrorKind::Shutdown, "client is closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted),
            )
        }
    }

    fn next_operation_id(&self) -> Result<OperationId> {
        // `fetch_update` was renamed to `try_update` after our Rust 1.88 MSRV.
        #[allow(deprecated)]
        let value = self
            .next_operation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| Error::new(ErrorKind::Internal, "operation identifier space exhausted"))?;
        Ok(OperationId(
            NonZeroU64::new(value).expect("operation IDs start at one"),
        ))
    }

    fn admission(&self, future: CompletionFuture) -> Result<Admission> {
        self.register_admission(future, None)
    }

    fn register_admission(
        &self,
        future: CompletionFuture,
        shutdown_completion: Option<Completion>,
    ) -> Result<Admission> {
        let operation_id = self.next_operation_id()?;
        let (sender, receiver) = flume::bounded(1);
        self.completion_tx
            .send(CompletionRegistration {
                operation_id,
                sender,
                future,
                shutdown_completion,
            })
            .map_err(|_| {
                Error::new(ErrorKind::Shutdown, "driver stopped during admission")
                    .with_delivery(DeliveryStatus::Ambiguous)
            })?;
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(operation_id, receiver),
        })
    }

    fn shutdown_admission(&self, completion: Completion) -> Result<Admission> {
        let admission = self.register_admission(Box::pin(std::future::pending()), Some(completion));
        if let Ok(admission) = &admission {
            self.shutdown_operation
                .store(admission.operation_id.get(), Ordering::Release);
        }
        self.shutdown_registration_ready
            .store(true, Ordering::Release);
        admission
    }

    fn transition_to_closing(&self) -> Result<()> {
        self.lifecycle
            .compare_exchange(
                LifecycleState::Running as u8,
                LifecycleState::Closing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| {
                self.shutdown_operation.store(0, Ordering::Release);
                self.shutdown_registration_ready
                    .store(false, Ordering::Release);
                self.request_progress.notify_waiters();
            })
            .map_err(|_| {
                Error::new(ErrorKind::Shutdown, "client is already closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted)
            })
    }

    fn restore_running(&self) {
        _ = self.lifecycle.compare_exchange(
            LifecycleState::Closing as u8,
            LifecycleState::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn best_effort_immediate_close(&self) {
        let _shutdown_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let newly_closing = match self.state() {
            LifecycleState::Running => {
                self.lifecycle
                    .store(LifecycleState::Closing as u8, Ordering::Release);
                self.request_progress.notify_waiters();
                true
            }
            LifecycleState::Closing => false,
            LifecycleState::Closed | LifecycleState::Failed => return,
        };
        match &self.client {
            ProtocolClient::V311(client) => _ = client.try_disconnect_now(),
            ProtocolClient::V5(client) => _ = client.try_disconnect_now(),
        }
        self.shutdown_kind.store(2, Ordering::Release);
        if newly_closing {
            self.shutdown_operation.store(0, Ordering::Release);
            self.shutdown_registration_ready
                .store(true, Ordering::Release);
        }
        _ = self.immediate_shutdown_tx.send(());
    }
}

/// Cloneable command handle containing only thread-safe client/control senders and shared status.
pub struct ClientHandle {
    shared: Arc<Shared>,
}

impl Clone for ClientHandle {
    fn clone(&self) -> Self {
        self.shared.handle_count.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl std::fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientHandle")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        if self.shared.handle_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.best_effort_immediate_close();
        }
    }
}

impl ClientHandle {
    #[must_use]
    pub fn state(&self) -> LifecycleState {
        self.shared.state()
    }

    /// Idempotently requests immediate shutdown, including escalation from an
    /// in-progress graceful shutdown.
    ///
    /// This control path is intended for native-wrapper cleanup and finalizers.
    /// It makes no delivery claim for unfinished work and does not wait for the
    /// driver thread to terminate; the owning [`NativeClient`] can subsequently
    /// use [`NativeClient::join`] for bounded cleanup.
    pub fn close_now_idempotent(&self) {
        self.shared.best_effort_immediate_close();
    }

    /// Nonblocking admission into the underlying bounded MQTT request channel.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, the request channel is full or closed, or the
    /// client is shutting down.
    pub fn try_admit(&self, command: Command) -> Result<Admission> {
        match command {
            Command::Publish(command) => self.try_publish(command),
            Command::Subscribe(command) => self.try_subscribe(command),
            Command::Unsubscribe(filters) => self.try_unsubscribe(filters),
            Command::Acknowledge(token) => self.try_acknowledge(token),
            Command::GracefulDisconnect { timeout } => self.try_close(timeout),
            Command::ImmediateDisconnect => self.try_close_now(),
            Command::Diagnostics => self.try_diagnostics(),
        }
    }

    /// Waits asynchronously for bounded request-channel capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, the request channel closes, or the client is
    /// shutting down.
    pub async fn admit_async(&self, command: Command) -> Result<Admission> {
        match command {
            Command::Publish(command) => self.publish(command).await,
            Command::Subscribe(command) => self.subscribe(command).await,
            Command::Unsubscribe(filters) => self.unsubscribe(filters).await,
            Command::Acknowledge(token) => self.acknowledge(token).await,
            // Shutdown and diagnostics use priority/control paths and never wait for the publish queue.
            other => self.try_admit(other),
        }
    }

    /// Blocking counterpart to [`Self::admit_async`].
    ///
    /// # Blocking
    ///
    /// This function blocks the calling thread while it waits for request-channel capacity. Do
    /// not call it from a JavaScript event-loop thread, a Python async-executor thread, or another
    /// latency-sensitive async thread. Native wrappers should expose it only through an explicitly
    /// blocking API or invoke it on a wrapper-owned worker thread. Async wrappers should normally
    /// use [`Self::admit_async`], while callers that cannot wait should use [`Self::try_admit`].
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::admit_async`].
    pub fn admit(&self, command: Command) -> Result<Admission> {
        futures_executor::block_on(self.admit_async(command))
    }

    async fn retry_on_backpressure(
        &self,
        mut attempt: impl FnMut() -> Result<Admission>,
    ) -> Result<Admission> {
        loop {
            let progress = self.shared.request_progress.notified();
            tokio::pin!(progress);
            progress.as_mut().enable();
            match attempt() {
                Err(error) if error.kind() == ErrorKind::Backpressure => progress.await,
                result => return result,
            }
        }
    }

    fn try_publish(&self, command: PublishCommand) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        validate_mqtt_utf8_string(&command.topic, "publish topic")?;
        match &self.shared.client {
            ProtocolClient::V311(client) => {
                if command.v5_properties.is_some() {
                    return Err(protocol_option_error(
                        "MQTT 5 publish properties require MQTT 5",
                    ));
                }
                let options = v4_publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(map_v4_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v4_publish_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let mut topic_aliases =
                    self.shared.outbound_topic_aliases.lock().map_err(|_| {
                        Error::new(ErrorKind::Internal, "topic alias map mutex poisoned")
                    })?;
                validate_outbound_v5_publish(
                    command.v5_properties.as_ref(),
                    &command.payload,
                    &command.topic,
                    command.qos,
                    command.retain,
                    self.shared.v5_publish_capabilities(),
                    &topic_aliases,
                )?;
                let alias_mapping = command
                    .v5_properties
                    .as_ref()
                    .and_then(|properties| properties.topic_alias)
                    .filter(|_| !command.topic.is_empty())
                    .map(|alias| (alias, command.topic.clone()));
                let options = v5_publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(map_v5_client_error)?;
                if let Some((alias, topic)) = alias_mapping {
                    topic_aliases.insert(alias, topic);
                }
                drop(topic_aliases);
                self.shared.admission(Box::pin(async move {
                    map_v5_publish_notice(notice.wait_async().await)
                }))
            }
        }
    }

    async fn publish(&self, command: PublishCommand) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_publish(command.clone()))
            .await
    }

    fn try_subscribe(&self, command: SubscribeCommand) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        if command.filters.is_empty() {
            return Err(protocol_option_error(
                "subscribe requires at least one filter",
            ));
        }
        match &self.shared.client {
            ProtocolClient::V311(client) => {
                let filters = command
                    .filters
                    .into_iter()
                    .map(|filter| {
                        rumqttc_v4::SubscribeFilterInput::new(filter.filter, to_v4_qos(filter.qos))
                    })
                    .collect::<Vec<_>>();
                let notice = client
                    .try_subscribe_many_tracked(filters)
                    .map_err(map_v4_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v4_subscribe_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let filters = command
                    .filters
                    .into_iter()
                    .map(|filter| {
                        rumqttc_v5::SubscribeFilterInput::new(filter.filter, to_v5_qos(filter.qos))
                    })
                    .collect::<Vec<_>>();
                let notice = client
                    .try_subscribe_many_tracked(filters)
                    .map_err(map_v5_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v5_subscribe_notice(notice.wait_async().await)
                }))
            }
        }
    }

    async fn subscribe(&self, command: SubscribeCommand) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_subscribe(command.clone()))
            .await
    }

    fn try_unsubscribe(&self, filters: Vec<String>) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        if filters.is_empty() {
            return Err(protocol_option_error(
                "unsubscribe requires at least one filter",
            ));
        }
        match &self.shared.client {
            ProtocolClient::V311(client) => {
                let notice = client
                    .try_unsubscribe_many_tracked(filters)
                    .map_err(map_v4_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v4_unsubscribe_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let notice = client
                    .try_unsubscribe_many_tracked(filters)
                    .map_err(map_v5_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v5_unsubscribe_notice(notice.wait_async().await)
                }))
            }
        }
    }

    async fn unsubscribe(&self, filters: Vec<String>) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_unsubscribe(filters.clone()))
            .await
    }

    fn reserve_ack(&self, token: AckToken) -> Result<AckReservation> {
        self.shared.require_running()?;
        if token.client != self.shared.client_identity
            || token.generation != self.shared.connection_generation.load(Ordering::Acquire)
        {
            return Err(protocol_option_error(
                "acknowledgement token is stale or belongs to another client",
            ));
        }
        let mut acknowledgements =
            self.shared.acknowledgements.lock().map_err(|_| {
                Error::new(ErrorKind::Internal, "acknowledgement map mutex poisoned")
            })?;
        let ack = acknowledgements.remove(&token).ok_or_else(|| {
            protocol_option_error("acknowledgement token is unknown, reserved, or already consumed")
        })?;
        drop(acknowledgements);
        Ok(AckReservation {
            shared: Arc::clone(&self.shared),
            token,
            ack: Some(ack),
        })
    }

    fn try_enqueue_ack(&self, ack: &PreparedAck) -> Result<CompletionFuture> {
        let key = ack.key();
        let (sender, receiver) = flume::bounded(1);
        let mut completions = self
            .shared
            .acknowledgement_completions
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "ACK completion map mutex poisoned"))?;
        match completions.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(sender);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "an acknowledgement for this MQTT packet is already pending",
                ));
            }
        }
        let result = match (&self.shared.client, ack) {
            (ProtocolClient::V311(client), PreparedAck::V311(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(map_v4_client_error),
            (ProtocolClient::V5(client), PreparedAck::V5(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(map_v5_client_error),
            _ => Err(Error::new(
                ErrorKind::Internal,
                "acknowledgement protocol mismatch",
            )),
        };
        if result.is_err() {
            completions.remove(&key);
        }
        drop(completions);
        result?;
        Ok(Box::pin(async move {
            receiver.recv_async().await.unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorKind::Shutdown,
                    "driver stopped before acknowledgement transmission was observed",
                )
                .with_delivery(DeliveryStatus::Ambiguous))
            })
        }))
    }

    fn try_acknowledge(&self, token: AckToken) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        let reservation = self.reserve_ack(token)?;
        let completion = self.try_enqueue_ack(reservation.ack())?;
        reservation.commit();
        self.shared.admission(completion)
    }

    async fn acknowledge(&self, token: AckToken) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_acknowledge(token))
            .await
    }

    fn try_close(&self, timeout: Option<Duration>) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        self.shared.transition_to_closing()?;
        let result = match &self.shared.client {
            ProtocolClient::V311(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(map_v4_client_error),
            ProtocolClient::V5(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(map_v5_client_error),
        };
        if let Err(error) = result {
            self.shared.restore_running();
            self.shared
                .shutdown_registration_ready
                .store(true, Ordering::Release);
            return Err(error);
        }
        self.shared.shutdown_kind.store(1, Ordering::Release);
        self.shared.shutdown_admission(Completion::GracefulShutdown)
    }

    fn try_close_now(&self) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        self.shared.transition_to_closing()?;
        let result = match &self.shared.client {
            ProtocolClient::V311(client) => {
                client.try_disconnect_now().map_err(map_v4_client_error)
            }
            ProtocolClient::V5(client) => client.try_disconnect_now().map_err(map_v5_client_error),
        };
        if let Err(error) = result {
            self.shared.restore_running();
            self.shared
                .shutdown_registration_ready
                .store(true, Ordering::Release);
            return Err(error);
        }
        self.shared.shutdown_kind.store(2, Ordering::Release);
        let admission = self
            .shared
            .shutdown_admission(Completion::ImmediateShutdown)?;
        _ = self.shared.immediate_shutdown_tx.send(());
        Ok(admission)
    }

    fn try_diagnostics(&self) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        let operation_id = self.shared.next_operation_id()?;
        let (sender, receiver) = flume::bounded(1);
        self.shared
            .diagnostics_tx
            .try_send(DiagnosticsRequest { sender })
            .map_err(|error| match error {
                flume::TrySendError::Full(_) => Error::new(
                    ErrorKind::Backpressure,
                    "diagnostics request channel is full",
                )
                .with_delivery(DeliveryStatus::NotAdmitted),
                flume::TrySendError::Disconnected(_) => {
                    Error::new(ErrorKind::Shutdown, "driver is not running")
                        .with_delivery(DeliveryStatus::NotAdmitted)
                }
            })?;
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(operation_id, receiver),
        })
    }
}

/// Sole consumer of normalized events. It owns the independent terminal-status receiver.
pub struct EventConsumer {
    events: Receiver<WrapperEvent>,
    terminal: Receiver<TerminalStatus>,
    terminal_seen: bool,
}

impl std::fmt::Debug for EventConsumer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventConsumer").finish_non_exhaustive()
    }
}

impl EventConsumer {
    /// Attempts to receive an event without blocking.
    ///
    /// # Errors
    ///
    /// Reserved for event-consumer failures exposed by future transports. The current in-process
    /// transport does not produce an error here.
    pub fn try_recv(&mut self) -> Result<Option<WrapperEvent>> {
        match self.events.try_recv() {
            Ok(event) => return Ok(Some(event)),
            Err(flume::TryRecvError::Empty | flume::TryRecvError::Disconnected) => {}
        }
        Ok(self.try_terminal())
    }

    /// Waits for at most `timeout` for the next event.
    ///
    /// # Errors
    ///
    /// Reserved for event-consumer failures exposed by future transports. The current in-process
    /// transport does not produce an error here.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<WrapperEvent>> {
        if let Some(event) = self.try_recv()? {
            return Ok(Some(event));
        }
        if self.terminal_seen {
            return Ok(None);
        }

        let started = Instant::now();
        match flume::Selector::new()
            .recv(&self.events, TimedReceive::Event)
            .recv(&self.terminal, TimedReceive::Terminal)
            .wait_timeout(timeout)
        {
            Ok(TimedReceive::Event(Ok(event))) => Ok(Some(event)),
            Ok(TimedReceive::Event(Err(_))) => {
                // The driver drops the ordinary event sender immediately before publishing its
                // terminal status. Preserve the original deadline while covering that small gap.
                let remaining = timeout.saturating_sub(started.elapsed());
                Ok(self.recv_terminal_timeout(remaining))
            }
            Ok(TimedReceive::Terminal(Ok(status))) => {
                self.terminal_seen = true;
                Ok(Some(status.into_event()))
            }
            Ok(TimedReceive::Terminal(Err(_))) => {
                self.terminal_seen = true;
                Ok(None)
            }
            Err(flume::select::SelectError::Timeout) => Ok(None),
        }
    }

    /// Waits asynchronously for the next event or terminal driver status.
    ///
    /// # Errors
    ///
    /// Reserved for event-consumer failures exposed by future transports. The current in-process
    /// transport does not produce an error here.
    pub async fn recv_async(&mut self) -> Result<Option<WrapperEvent>> {
        if let Some(event) = self.try_recv()? {
            return Ok(Some(event));
        }
        if self.terminal_seen {
            return Ok(None);
        }
        tokio::select! {
            biased;
            event = self.events.recv_async() => match event {
                Ok(event) => Ok(Some(event)),
                Err(_) => self.recv_terminal_async().await,
            },
            terminal = self.terminal.recv_async() => {
                self.terminal_seen = true;
                Ok(terminal.ok().map(TerminalStatus::into_event))
            }
        }
    }

    fn try_terminal(&mut self) -> Option<WrapperEvent> {
        if self.terminal_seen {
            return None;
        }
        match self.terminal.try_recv() {
            Ok(status) => {
                self.terminal_seen = true;
                Some(status.into_event())
            }
            Err(flume::TryRecvError::Empty) => None,
            Err(flume::TryRecvError::Disconnected) => {
                self.terminal_seen = true;
                None
            }
        }
    }

    async fn recv_terminal_async(&mut self) -> Result<Option<WrapperEvent>> {
        if self.terminal_seen {
            return Ok(None);
        }
        self.terminal_seen = true;
        Ok(self
            .terminal
            .recv_async()
            .await
            .ok()
            .map(TerminalStatus::into_event))
    }

    fn recv_terminal_timeout(&mut self, timeout: Duration) -> Option<WrapperEvent> {
        if self.terminal_seen {
            return None;
        }
        match self.terminal.recv_timeout(timeout) {
            Ok(status) => {
                self.terminal_seen = true;
                Some(status.into_event())
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                self.terminal_seen = true;
                None
            }
            Err(flume::RecvTimeoutError::Timeout) => None,
        }
    }
}

enum TimedReceive {
    Event(std::result::Result<WrapperEvent, flume::RecvError>),
    Terminal(std::result::Result<TerminalStatus, flume::RecvError>),
}

#[derive(Clone, Debug)]
enum TerminalStatus {
    Closed { graceful: bool },
    Failed(Error),
}

impl TerminalStatus {
    fn into_event(self) -> WrapperEvent {
        match self {
            Self::Closed { graceful: true } => WrapperEvent::GracefulShutdownCompleted,
            Self::Closed { graceful: false } => WrapperEvent::DriverTerminated(Error::new(
                ErrorKind::Shutdown,
                "client was closed immediately",
            )),
            Self::Failed(error) => WrapperEvent::DriverTerminated(error),
        }
    }
}

/// Dedicated native client and its joinable driver-thread ownership.
pub struct NativeClient {
    handle: Option<ClientHandle>,
    events: Option<EventConsumer>,
    join: Mutex<Option<JoinHandle<()>>>,
    done: Receiver<()>,
}

impl std::fmt::Debug for NativeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeClient")
            .field("state", &self.handle.as_ref().map(ClientHandle::state))
            .finish_non_exhaustive()
    }
}

impl NativeClient {
    /// Starts a dedicated MQTT driver thread.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration validation, protocol client construction, TLS setup,
    /// or driver-thread creation fails.
    pub fn start(config: ClientConfig) -> Result<Self> {
        config.validate()?;
        let protocol = config.protocol_version();
        let event_capacity = config.common.event_buffer_capacity;
        let delivery_timeout = config.common.event_delivery_timeout;
        let request_capacity = config.common.request_channel_capacity;
        let emit_outgoing = config.common.emit_outgoing_events;
        let manual_ack = config.common.ack_mode == AckMode::Manual;

        let (completion_tx, completion_rx) = flume::unbounded();
        let (diagnostics_tx, diagnostics_rx) = flume::bounded(request_capacity);
        let (event_tx, event_rx) = flume::bounded(event_capacity);
        let (terminal_tx, terminal_rx) = flume::bounded(1);
        let (done_tx, done_rx) = flume::bounded(1);
        let (immediate_shutdown_tx, immediate_shutdown_rx) = flume::unbounded();

        let client_identity = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
        let (client, driver) = build_protocol(config)?;
        let shared = Arc::new(Shared {
            client,
            client_identity,
            connection_generation: AtomicU64::new(0),
            broker_topic_alias_max: AtomicU16::new(0),
            broker_maximum_qos: AtomicU8::new(QoS::ExactlyOnce as u8),
            broker_retain_available: AtomicBool::new(true),
            broker_capabilities_known: AtomicBool::new(false),
            next_ack_serial: AtomicU64::new(1),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_kind: AtomicU8::new(0),
            shutdown_operation: AtomicU64::new(0),
            shutdown_registration_ready: AtomicBool::new(true),
            handle_count: AtomicUsize::new(1),
            admission_gate: Mutex::new(()),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            outbound_topic_aliases: Mutex::new(HashMap::new()),
            completion_tx,
            diagnostics_tx,
            immediate_shutdown_tx,
            request_progress: Notify::new(),
        });
        let driver_shared = Arc::clone(&shared);
        let context = DriverContext {
            shared: Arc::clone(&driver_shared),
            completion_rx,
            diagnostics_rx,
            events: event_tx,
            delivery_timeout,
            emit_outgoing,
            manual_ack,
            protocol,
            immediate_shutdown_rx,
        };
        let thread_name = format!("rumqtt-wrapper-{client_identity}");
        let join = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let terminal = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run_driver(driver, context)),
                    Err(error) => TerminalStatus::Failed(Error::sourced(
                        ErrorKind::Internal,
                        DeliveryStatus::NotApplicable,
                        error,
                    )),
                };
                publish_terminal_lifecycle(&driver_shared, &terminal);
                _ = terminal_tx.send(terminal);
                _ = done_tx.send(());
            })
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;

        Ok(Self {
            handle: Some(ClientHandle { shared }),
            events: Some(EventConsumer {
                events: event_rx,
                terminal: terminal_rx,
                terminal_seen: false,
            }),
            join: Mutex::new(Some(join)),
            done: done_rx,
        })
    }

    #[must_use]
    /// Returns another handle to the running client.
    ///
    /// # Panics
    ///
    /// Panics only if the internal handle invariant is violated while `NativeClient` is being
    /// destroyed. Safe callers cannot observe that state.
    pub fn handle(&self) -> ClientHandle {
        self.handle
            .as_ref()
            .expect("native client handle retained")
            .clone()
    }

    pub const fn take_events(&mut self) -> Option<EventConsumer> {
        self.events.take()
    }

    /// Waits for the driver to terminate and joins its thread only after termination is observed.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout expires, the join state is poisoned, or the driver thread
    /// panics.
    pub fn join(&self, timeout: Duration) -> Result<()> {
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(flume::RecvTimeoutError::Disconnected) => {}
            Err(flume::RecvTimeoutError::Timeout) => {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "driver did not terminate before join timeout",
                ));
            }
        }
        let join = {
            let mut join_slot = self
                .join
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "join mutex poisoned"))?;
            join_slot.take()
        };
        if let Some(join) = join {
            join.join()
                .map_err(|_| Error::new(ErrorKind::Internal, "driver thread panicked"))?;
        }
        Ok(())
    }
}

fn publish_terminal_lifecycle(shared: &Shared, terminal: &TerminalStatus) {
    let lifecycle = match terminal {
        TerminalStatus::Closed { .. } => LifecycleState::Closed,
        TerminalStatus::Failed(_) => LifecycleState::Failed,
    };
    shared.lifecycle.store(lifecycle as u8, Ordering::Release);
    // Capacity waiters arm this notification before checking the request channel. Publishing the
    // terminal state before waking them therefore cannot lose the transition: every waiter either
    // observes the terminal lifecycle immediately or is registered for this notification.
    shared.request_progress.notify_waiters();
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shared.best_effort_immediate_close();
            drop(handle);
        }
    }
}

enum ProtocolDriver {
    V311(Box<rumqttc_v4::EventLoop>),
    V5(Box<rumqttc_v5::EventLoop>),
}

struct DriverContext {
    shared: Arc<Shared>,
    completion_rx: Receiver<CompletionRegistration>,
    diagnostics_rx: Receiver<DiagnosticsRequest>,
    events: Sender<WrapperEvent>,
    delivery_timeout: Duration,
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
    immediate_shutdown_rx: Receiver<()>,
}

struct ShutdownInputs<'a> {
    shared: &'a Shared,
    completion_rx: &'a Receiver<CompletionRegistration>,
    diagnostics_rx: &'a Receiver<DiagnosticsRequest>,
}

impl<'a> ShutdownInputs<'a> {
    const fn new(
        shared: &'a Shared,
        completion_rx: &'a Receiver<CompletionRegistration>,
        diagnostics_rx: &'a Receiver<DiagnosticsRequest>,
    ) -> Self {
        Self {
            shared,
            completion_rx,
            diagnostics_rx,
        }
    }
}

fn build_protocol(config: ClientConfig) -> Result<(ProtocolClient, ProtocolDriver)> {
    let ClientConfig { common, protocol } = config;
    match protocol {
        ProtocolConfig::V311(protocol) => {
            let mut options = build_v4_options(&common)?;
            options
                .try_set_clean_session(protocol.clean_session)
                .map_err(|error| {
                    Error::sourced(
                        ErrorKind::Configuration,
                        DeliveryStatus::NotApplicable,
                        error,
                    )
                })?;
            options.validate().map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?;
            let (client, mut eventloop) = rumqttc_v4::AsyncClient::builder(options)
                .capacity(common.request_channel_capacity)
                .try_build()
                .map_err(|error| {
                    Error::sourced(
                        ErrorKind::Configuration,
                        DeliveryStatus::NotApplicable,
                        error,
                    )
                })?;
            let mut network = rumqttc_v4::NetworkOptions::new();
            network.set_connection_timeout(common.connection_timeout.as_secs());
            eventloop.network_options = network;
            Ok((
                ProtocolClient::V311(client),
                ProtocolDriver::V311(Box::new(eventloop)),
            ))
        }
        ProtocolConfig::V5(protocol) => {
            let mut options = build_v5_options(&common)?;
            options.set_clean_start(protocol.clean_start);
            options.set_session_expiry_interval(protocol.session_expiry_interval);
            options.validate().map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?;
            let (client, eventloop) = rumqttc_v5::AsyncClient::builder(options)
                .capacity(common.request_channel_capacity)
                .try_build()
                .map_err(|error| {
                    Error::sourced(
                        ErrorKind::Configuration,
                        DeliveryStatus::NotApplicable,
                        error,
                    )
                })?;
            Ok((
                ProtocolClient::V5(client),
                ProtocolDriver::V5(Box::new(eventloop)),
            ))
        }
    }
}

fn build_v4_options(common: &crate::CommonConfig) -> Result<rumqttc_v4::MqttOptions> {
    let tls = match &common.transport {
        TransportConfig::Tls(tls) | TransportConfig::Wss { tls, .. } => Some(build_tls(tls)?),
        _ => None,
    };
    let mut options = match &common.transport {
        TransportConfig::Tcp | TransportConfig::Tls(_) => rumqttc_v4::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v4::Broker::tcp(common.broker_host.clone(), common.broker_port),
        ),
        TransportConfig::WebSocket { url } => rumqttc_v4::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v4::Broker::websocket(url.clone()).map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?,
        ),
        TransportConfig::Wss { url, .. } => rumqttc_v4::MqttOptions::websocket_with_tls_config(
            common.client_id.clone(),
            url.clone(),
            tls.clone().expect("WSS TLS built"),
        )
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?,
    };
    if matches!(common.transport, TransportConfig::Tls(_)) {
        options.set_transport(rumqttc_v4::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
    }
    options.set_keep_alive(duration_to_u16(common.keep_alive, "keep alive")?);
    options.set_max_packet_size(common.incoming_packet_size_limit as usize, usize::MAX);
    options.set_request_channel_capacity(common.request_channel_capacity);
    options.set_ack_mode(match common.ack_mode {
        AckMode::Automatic => rumqttc_v4::AckMode::Automatic,
        AckMode::Manual => rumqttc_v4::AckMode::Manual,
    });
    set_v4_auth(&mut options, common);
    Ok(options)
}

fn build_v5_options(common: &crate::CommonConfig) -> Result<rumqttc_v5::MqttOptions> {
    let tls = match &common.transport {
        TransportConfig::Tls(tls) | TransportConfig::Wss { tls, .. } => Some(build_tls(tls)?),
        _ => None,
    };
    let mut options = match &common.transport {
        TransportConfig::Tcp | TransportConfig::Tls(_) => rumqttc_v5::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v5::Broker::tcp(common.broker_host.clone(), common.broker_port),
        ),
        TransportConfig::WebSocket { url } => rumqttc_v5::MqttOptions::new(
            common.client_id.clone(),
            rumqttc_v5::Broker::websocket(url.clone()).map_err(|error| {
                Error::sourced(
                    ErrorKind::Configuration,
                    DeliveryStatus::NotApplicable,
                    error,
                )
            })?,
        ),
        TransportConfig::Wss { url, .. } => rumqttc_v5::MqttOptions::websocket_with_tls_config(
            common.client_id.clone(),
            url.clone(),
            tls.clone().expect("WSS TLS built"),
        )
        .map_err(|error| {
            Error::sourced(
                ErrorKind::Configuration,
                DeliveryStatus::NotApplicable,
                error,
            )
        })?,
    };
    if matches!(common.transport, TransportConfig::Tls(_)) {
        options.set_transport(rumqttc_v5::Transport::tls_with_config(
            tls.expect("TLS built"),
        ));
    }
    options.set_keep_alive(duration_to_u16(common.keep_alive, "keep alive")?);
    options.set_incoming_packet_size_limit(rumqttc_v5::IncomingPacketSizeLimit::Bytes(
        common.incoming_packet_size_limit,
    ));
    options.set_request_channel_capacity(common.request_channel_capacity);
    options.set_ack_mode(match common.ack_mode {
        AckMode::Automatic => rumqttc_v5::AckMode::Automatic,
        AckMode::Manual => rumqttc_v5::AckMode::Manual,
    });
    let mut network = rumqttc_v5::NetworkOptions::new();
    network.set_connection_timeout(common.connection_timeout.as_secs());
    options.set_network_options(network);
    set_v5_auth(&mut options, common);
    Ok(options)
}

fn build_tls(config: &TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    if let Some(ca) = &config.ca {
        let certificates = CertificateDer::pem_slice_iter(ca)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error)
            })?;
        if certificates.is_empty() {
            return Err(Error::configuration(
                "custom TLS CA contains no PEM certificate",
            ));
        }
    }
    if let (Some(certificate), Some(key)) = (&config.client_certificate, &config.private_key) {
        let certificates = CertificateDer::pem_slice_iter(certificate)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error)
            })?;
        if certificates.is_empty() {
            return Err(Error::configuration(
                "TLS client certificate contains no PEM certificate",
            ));
        }
        PrivateKeyDer::from_pem_slice(key).map_err(|error| {
            Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error)
        })?;
    }
    if let Some(ca) = &config.ca {
        return Ok(rumqttc_v4::TlsConfiguration::Simple {
            ca: ca.to_vec(),
            alpn: None,
            client_auth: config
                .client_certificate
                .as_ref()
                .zip(config.private_key.as_ref())
                .map(|(certificate, key)| (certificate.to_vec(), key.to_vec())),
        });
    }
    if config.client_certificate.is_none() {
        return rumqttc_v4::TlsConfiguration::try_default_rustls()
            .map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error));
    }

    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let builder = RustlsClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))?;
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    if !native.errors.is_empty() {
        return Err(Error::new(
            ErrorKind::Tls,
            format!("failed to load platform certificates: {:?}", native.errors),
        ));
    }
    for certificate in native.certs {
        roots.add(certificate).map_err(|error| {
            Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error)
        })?;
    }
    let certificates = CertificateDer::pem_slice_iter(
        config
            .client_certificate
            .as_ref()
            .expect("validated certificate"),
    )
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))?;
    if certificates.is_empty() {
        return Err(Error::configuration(
            "TLS client certificate contains no PEM certificate",
        ));
    }
    let key = PrivateKeyDer::from_pem_slice(config.private_key.as_ref().expect("validated key"))
        .map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))?;
    let rustls = builder
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates, key)
        .map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))?;
    Ok(rumqttc_v4::TlsConfiguration::Rustls(Arc::new(rustls)))
}

fn set_v4_auth(options: &mut rumqttc_v4::MqttOptions, common: &crate::CommonConfig) {
    match (&common.username, &common.password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username.clone(), password.clone());
        }
        (Some(username), None) => {
            options.set_username(username.clone());
        }
        (None, None | Some(_)) => {}
    }
}

fn set_v5_auth(options: &mut rumqttc_v5::MqttOptions, common: &crate::CommonConfig) {
    match (&common.username, &common.password) {
        (Some(username), Some(password)) => {
            options.set_credentials(username.clone(), password.clone());
        }
        (Some(username), None) => {
            options.set_username(username.clone());
        }
        (None, Some(password)) => {
            options.set_password(password.clone());
        }
        (None, None) => {}
    }
}

async fn run_driver(driver: ProtocolDriver, context: DriverContext) -> TerminalStatus {
    match driver {
        ProtocolDriver::V311(eventloop) => run_v4(eventloop, context).await,
        ProtocolDriver::V5(eventloop) => run_v5(eventloop, context).await,
    }
}

struct EventDelivery<'a> {
    shared: &'a Shared,
    events: &'a Sender<WrapperEvent>,
    timeout: Duration,
    immediate_shutdown: &'a Receiver<()>,
}

// The two explicit loops keep protocol types statically checked and make all translation local.
async fn run_v4(
    mut eventloop: Box<rumqttc_v4::EventLoop>,
    context: DriverContext,
) -> TerminalStatus {
    let DriverContext {
        shared,
        completion_rx,
        diagnostics_rx,
        events,
        delivery_timeout,
        emit_outgoing,
        manual_ack,
        protocol,
        immediate_shutdown_rx,
    } = context;
    let mut pending = FuturesUnordered::<PendingFuture>::new();
    let mut senders = HashMap::<OperationId, PendingSender>::new();
    let mut connected = false;
    let mut diagnostics = snapshot_v4(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
    };
    loop {
        // `EventLoop::poll` can dequeue requests and mutate protocol state before awaiting I/O.
        // Keep the same future alive across wrapper-control wakeups so those side effects cannot
        // be abandoned by `select!` cancellation. Diagnostics use the last completed snapshot
        // while the poll future holds the mutable event-loop borrow.
        let polled = {
            let poll = eventloop.poll();
            tokio::pin!(poll);
            loop {
                // Fair selection prevents sustained wrapper control traffic from starving MQTT
                // network progress while preserving this single, non-cancellable poll future.
                tokio::select! {
                    _ = immediate_shutdown_rx.recv_async(), if !connected => break None,
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        _ = request.sender.send(Ok(Completion::Diagnostics(diagnostics.clone())));
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                    },
                    result = &mut poll => break Some(result),
                }
            }
        };
        let Some(polled) = polled else {
            // There is no established MQTT session to close cleanly. Dropping the event loop is
            // the cancellation boundary for DNS/TCP/TLS/CONNACK work; unlike resuming a cancelled
            // poll, termination cannot lose a dequeued request and then continue with corrupt state.
            return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders).await;
        };
        shared.request_progress.notify_waiters();
        diagnostics = snapshot_v4(&eventloop);
        match polled {
            Ok(event) => {
                if let Some(event) = map_v4_event(
                    &mut eventloop,
                    event,
                    &shared,
                    &mut connected,
                    emit_outgoing,
                    manual_ack,
                    protocol,
                ) && !deliver(&delivery, event).await
                {
                    let error = overflow_error();
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
            Err(rumqttc_v4::ConnectionError::RequestsDone) => {
                let graceful =
                    complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await;
                return TerminalStatus::Closed { graceful };
            }
            Err(error) => {
                let error = map_v4_connection_error(error);
                if let Some(shutdown_kind) = committed_shutdown_kind(&shared) {
                    if shutdown_kind == 2 {
                        return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders)
                            .await;
                    }
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                let phase = if connected {
                    ConnectionPhase::Established
                } else {
                    ConnectionPhase::Attempt
                };
                connected = false;
                invalidate_acks(&shared, &error);
                if !deliver(&delivery, WrapperEvent::Disconnected { phase, error }).await {
                    let error = overflow_error();
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
        }
    }
}

async fn run_v5(
    mut eventloop: Box<rumqttc_v5::EventLoop>,
    context: DriverContext,
) -> TerminalStatus {
    let DriverContext {
        shared,
        completion_rx,
        diagnostics_rx,
        events,
        delivery_timeout,
        emit_outgoing,
        manual_ack,
        protocol,
        immediate_shutdown_rx,
    } = context;
    let mut pending = FuturesUnordered::<PendingFuture>::new();
    let mut senders = HashMap::<OperationId, PendingSender>::new();
    let mut connected = false;
    let mut diagnostics = snapshot_v5(&eventloop);
    let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
    let delivery = EventDelivery {
        shared: &shared,
        events: &events,
        timeout: delivery_timeout,
        immediate_shutdown: &immediate_shutdown_rx,
    };
    loop {
        // See the v4 loop: polling is an indivisible ownership boundary even while wrapper
        // registrations, cached diagnostics, and completed notices remain responsive.
        let polled = {
            let poll = eventloop.poll();
            tokio::pin!(poll);
            loop {
                // Keep parity with the fair v4 control/poll arbitration above.
                tokio::select! {
                    _ = immediate_shutdown_rx.recv_async(), if !connected => break None,
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        _ = request.sender.send(Ok(Completion::Diagnostics(diagnostics.clone())));
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                    },
                    result = &mut poll => break Some(result),
                }
            }
        };
        let Some(polled) = polled else {
            // Keep MQTT 5 connection-establishment cancellation identical to the v4 path.
            return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders).await;
        };
        shared.request_progress.notify_waiters();
        diagnostics = snapshot_v5(&eventloop);
        match polled {
            Ok(event) => {
                if let Some(event) = map_v5_event(
                    &mut eventloop,
                    event,
                    &shared,
                    &mut connected,
                    emit_outgoing,
                    manual_ack,
                    protocol,
                ) && !deliver(&delivery, event).await
                {
                    let error = overflow_error();
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
            Err(rumqttc_v5::ConnectionError::RequestsDone) => {
                let graceful =
                    complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await;
                return TerminalStatus::Closed { graceful };
            }
            Err(error) => {
                let error = map_v5_connection_error(error);
                if let Some(shutdown_kind) = committed_shutdown_kind(&shared) {
                    if shutdown_kind == 2 {
                        return finish_close(&shutdown, &diagnostics, &mut pending, &mut senders)
                            .await;
                    }
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
                let phase = if connected {
                    ConnectionPhase::Established
                } else {
                    ConnectionPhase::Attempt
                };
                connected = false;
                invalidate_v5_connection(&mut eventloop, &shared, &error);
                if !deliver(&delivery, WrapperEvent::Disconnected { phase, error }).await {
                    let error = overflow_error();
                    fail_acknowledgements(&shared, &error);
                    fail_pending(&mut senders, &error);
                    return TerminalStatus::Failed(error);
                }
            }
        }
    }
}

async fn deliver(delivery: &EventDelivery<'_>, event: WrapperEvent) -> bool {
    if delivery.shared.immediate_shutdown_requested() {
        return true;
    }
    tokio::select! {
        biased;
        _ = delivery.immediate_shutdown.recv_async() => true,
        result = tokio::time::timeout(delivery.timeout, delivery.events.send_async(event)) => {
            matches!(result, Ok(Ok(())))
        },
    }
}

fn accept_registration(
    registration: CompletionRegistration,
    pending: &FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    let CompletionRegistration {
        operation_id,
        sender,
        future,
        shutdown_completion,
    } = registration;
    let tracks_notice = shutdown_completion.is_none();
    senders.insert(
        operation_id,
        PendingSender {
            sender,
            shutdown_completion,
        },
    );
    if tracks_notice {
        pending.push(Box::pin(async move { (operation_id, future.await) }));
    }
}

fn resolve_pending(
    (operation_id, result): (OperationId, Result<Completion>),
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    if let Some(pending_sender) = senders.remove(&operation_id) {
        _ = pending_sender.sender.send(result);
    }
}

async fn wait_for_shutdown_operation(shared: &Shared) -> Option<OperationId> {
    while !shared.shutdown_registration_ready.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    NonZeroU64::new(shared.shutdown_operation.load(Ordering::Acquire)).map(OperationId)
}

async fn receive_shutdown_registrations(
    completion_rx: &Receiver<CompletionRegistration>,
    shutdown_operation: Option<OperationId>,
    mut shutdown_registration_received: bool,
) -> Vec<CompletionRegistration> {
    let mut registrations = Vec::new();
    while !shutdown_registration_received {
        let Ok(registration) = completion_rx.recv_async().await else {
            break;
        };
        shutdown_registration_received = shutdown_operation == Some(registration.operation_id);
        registrations.push(registration);
    }

    while let Ok(registration) = completion_rx.try_recv() {
        registrations.push(registration);
    }
    registrations
}

async fn drain_pending(
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    while let Some(result) = pending.next().await {
        resolve_pending(result, senders);
    }
}

async fn complete_shutdown(
    shutdown: &ShutdownInputs<'_>,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> bool {
    let shutdown_operation = wait_for_shutdown_operation(shutdown.shared).await;
    let shutdown_registration_received =
        shutdown_operation.is_none_or(|operation_id| senders.contains_key(&operation_id));
    for registration in receive_shutdown_registrations(
        shutdown.completion_rx,
        shutdown_operation,
        shutdown_registration_received,
    )
    .await
    {
        accept_registration(registration, pending, senders);
    }

    let graceful = shutdown.shared.shutdown_kind.load(Ordering::Acquire) == 1;
    fail_acknowledgements(
        shutdown.shared,
        &Error::new(
            ErrorKind::Shutdown,
            "driver closed before acknowledgement transmission was observed",
        ),
    );
    if graceful {
        complete_queued_diagnostics(shutdown.diagnostics_rx, diagnostics);
        drain_pending(pending, senders).await;
    }
    finish_shutdown(senders, graceful);
    graceful
}

async fn finish_close(
    shutdown: &ShutdownInputs<'_>,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> TerminalStatus {
    let graceful = complete_shutdown(shutdown, diagnostics, pending, senders).await;
    TerminalStatus::Closed { graceful }
}

fn complete_queued_diagnostics(
    diagnostics_rx: &Receiver<DiagnosticsRequest>,
    diagnostics: &DiagnosticsSnapshot,
) {
    while let Ok(request) = diagnostics_rx.try_recv() {
        _ = request
            .sender
            .send(Ok(Completion::Diagnostics(diagnostics.clone())));
    }
}

fn committed_shutdown_kind(shared: &Shared) -> Option<u8> {
    // Shutdown admission holds this gate from the lifecycle transition through request and
    // completion registration. Waiting for it prevents a connection error from observing the
    // transient `Closing` state of an admission that may still fail and restore `Running`.
    let _admission_guard = shared
        .admission_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (shared.state() == LifecycleState::Closing)
        .then(|| shared.shutdown_kind.load(Ordering::Acquire))
}

fn overflow_error() -> Error {
    Error::new(
        ErrorKind::Backpressure,
        "event buffer remained full beyond the delivery timeout",
    )
}

fn invalidate_acks(shared: &Shared, error: &Error) {
    let _admission_guard = shared
        .admission_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    invalidate_connection_state(shared, error);
}

fn invalidate_v5_connection(eventloop: &mut rumqttc_v5::EventLoop, shared: &Shared, error: &Error) {
    let _admission_guard = shared
        .admission_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The event loop has already cleaned every request it could observe before returning the
    // connection error. Repair the small set that producers may have admitted after that drain
    // while the same gate prevents any further old-generation admissions. The event loop seeds
    // repair from the mapping at its cleanup boundary and applies late rebindings in queue order;
    // the wrapper's current map may already contain a later rebinding.
    eventloop.prepare_pending_topic_aliases_for_reconnect();
    invalidate_connection_state(shared, error);
}

fn invalidate_connection_state(shared: &Shared, error: &Error) {
    shared.connection_generation.fetch_add(1, Ordering::AcqRel);
    shared
        .broker_capabilities_known
        .store(false, Ordering::Release);
    shared.broker_topic_alias_max.store(0, Ordering::Release);
    shared
        .broker_maximum_qos
        .store(QoS::ExactlyOnce as u8, Ordering::Release);
    shared
        .broker_retain_available
        .store(true, Ordering::Release);
    shared
        .outbound_topic_aliases
        .lock()
        .expect("topic alias map poisoned")
        .clear();
    shared
        .acknowledgements
        .lock()
        .expect("ack map poisoned")
        .clear();
    fail_acknowledgements(shared, error);
}

fn fail_acknowledgements(shared: &Shared, error: &Error) {
    let completions = std::mem::take(
        &mut *shared
            .acknowledgement_completions
            .lock()
            .expect("ACK completion map poisoned"),
    );
    for sender in completions.into_values() {
        _ = sender.send(Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)));
    }
}

fn fail_pending(senders: &mut HashMap<OperationId, PendingSender>, error: &Error) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (_, pending_sender) in pending {
        _ = pending_sender
            .sender
            .send(Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)));
    }
}

fn finish_shutdown(senders: &mut HashMap<OperationId, PendingSender>, graceful: bool) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (_, pending_sender) in pending {
        let result = match pending_sender.shutdown_completion {
            Some(Completion::GracefulShutdown) if graceful => Ok(Completion::GracefulShutdown),
            Some(Completion::ImmediateShutdown) if !graceful => Ok(Completion::ImmediateShutdown),
            Some(_) => Err(Error::new(
                ErrorKind::Shutdown,
                "the requested graceful shutdown was escalated to immediate shutdown",
            )
            .with_delivery(DeliveryStatus::Ambiguous)),
            None => Err(Error::new(
                ErrorKind::Shutdown,
                "driver closed before the operation reported a terminal MQTT result",
            )
            .with_delivery(DeliveryStatus::Ambiguous)),
        };
        _ = pending_sender.sender.send(result);
    }
}

fn map_v4_event(
    eventloop: &mut rumqttc_v4::EventLoop,
    event: rumqttc_v4::Event,
    shared: &Shared,
    connected: &mut bool,
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
) -> Option<WrapperEvent> {
    match event {
        rumqttc_v4::Event::Incoming(rumqttc_v4::Packet::ConnAck(connack)) => {
            let _admission_guard = shared
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            eventloop.discard_pending_manual_acknowledgements();
            *connected = true;
            shared.connection_generation.fetch_add(1, Ordering::AcqRel);
            shared
                .acknowledgements
                .lock()
                .expect("ack map poisoned")
                .clear();
            Some(WrapperEvent::Connected {
                protocol,
                session_present: connack.session_present,
            })
        }
        rumqttc_v4::Event::Incoming(rumqttc_v4::Packet::Publish(publish)) => {
            let ack_token = if manual_ack {
                match (&shared.client, publish.qos) {
                    (
                        ProtocolClient::V311(client),
                        rumqttc_v4::QoS::AtLeastOnce | rumqttc_v4::QoS::ExactlyOnce,
                    ) => client
                        .prepare_ack(&publish)
                        .and_then(|ack| insert_ack(shared, PreparedAck::V311(ack))),
                    _ => None,
                }
            } else {
                None
            };
            Some(WrapperEvent::IncomingPublish(Box::new(IncomingPublish {
                topic: publish.topic,
                payload: publish.payload,
                qos: from_v4_qos(publish.qos),
                retain: publish.retain,
                duplicate: publish.dup,
                ack_token,
                v5_properties: None,
            })))
        }
        rumqttc_v4::Event::Outgoing(outgoing) => {
            match outgoing {
                rumqttc_v4::Outgoing::PubAck(packet_id) => {
                    complete_acknowledgement(shared, AckKey::V311PubAck(packet_id));
                }
                rumqttc_v4::Outgoing::PubRec(packet_id) => {
                    complete_acknowledgement(shared, AckKey::V311PubRec(packet_id));
                }
                _ => {}
            }
            emit_outgoing.then(|| WrapperEvent::Outgoing(map_v4_outgoing(&outgoing)))
        }
        rumqttc_v4::Event::Incoming(_) => None,
    }
}

fn map_v5_event(
    eventloop: &mut rumqttc_v5::EventLoop,
    event: rumqttc_v5::Event,
    shared: &Shared,
    connected: &mut bool,
    emit_outgoing: bool,
    manual_ack: bool,
    protocol: ProtocolVersion,
) -> Option<WrapperEvent> {
    match event {
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::ConnAck(connack)) => {
            let _admission_guard = shared
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            eventloop.discard_pending_manual_acknowledgements();
            *connected = true;
            shared.connection_generation.fetch_add(1, Ordering::AcqRel);
            shared.broker_topic_alias_max.store(
                connack
                    .properties
                    .as_ref()
                    .and_then(|properties| properties.topic_alias_max)
                    .unwrap_or(0),
                Ordering::Release,
            );
            shared.broker_maximum_qos.store(
                connack
                    .properties
                    .as_ref()
                    .and_then(|properties| properties.max_qos)
                    .unwrap_or(QoS::ExactlyOnce as u8),
                Ordering::Release,
            );
            shared.broker_retain_available.store(
                connack
                    .properties
                    .as_ref()
                    .and_then(|properties| properties.retain_available)
                    .unwrap_or(1)
                    != 0,
                Ordering::Release,
            );
            shared
                .outbound_topic_aliases
                .lock()
                .expect("topic alias map poisoned")
                .clear();
            shared
                .acknowledgements
                .lock()
                .expect("ack map poisoned")
                .clear();
            shared
                .broker_capabilities_known
                .store(true, Ordering::Release);
            shared.request_progress.notify_waiters();
            Some(WrapperEvent::Connected {
                protocol,
                session_present: connack.session_present,
            })
        }
        rumqttc_v5::Event::Incoming(rumqttc_v5::Packet::Publish(publish)) => {
            let ack_token = if manual_ack {
                match (&shared.client, publish.qos) {
                    (
                        ProtocolClient::V5(client),
                        rumqttc_v5::QoS::AtLeastOnce | rumqttc_v5::QoS::ExactlyOnce,
                    ) => client
                        .prepare_ack(&publish)
                        .and_then(|ack| insert_ack(shared, PreparedAck::V5(ack))),
                    _ => None,
                }
            } else {
                None
            };
            Some(WrapperEvent::IncomingPublish(Box::new(IncomingPublish {
                topic: publish.topic,
                payload: publish.payload,
                qos: from_v5_qos(publish.qos),
                retain: publish.retain,
                duplicate: publish.dup,
                ack_token,
                v5_properties: publish.properties.map(from_v5_properties),
            })))
        }
        rumqttc_v5::Event::Outgoing(outgoing) => {
            match outgoing {
                rumqttc_v5::Outgoing::PubAck(packet_id) => {
                    complete_acknowledgement(shared, AckKey::V5PubAck(packet_id));
                }
                rumqttc_v5::Outgoing::PubRec(packet_id) => {
                    complete_acknowledgement(shared, AckKey::V5PubRec(packet_id));
                }
                _ => {}
            }
            emit_outgoing.then(|| WrapperEvent::Outgoing(map_v5_outgoing(&outgoing)))
        }
        _ => None,
    }
}

fn insert_ack(shared: &Shared, ack: PreparedAck) -> Option<AckToken> {
    // Serialize delivery-token creation with acknowledgement admission. Retransmissions received
    // before admission share one token; retransmissions received while that ACK is queued need no
    // new token because the queued packet acknowledges every copy with the same packet identifier.
    let _admission_guard = shared
        .admission_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = ack.key();
    let mut acknowledgements = shared.acknowledgements.lock().expect("ack map poisoned");
    if let Some(token) = acknowledgements.token(key) {
        return Some(token);
    }
    if shared
        .acknowledgement_completions
        .lock()
        .expect("ACK completion map poisoned")
        .contains_key(&key)
    {
        return None;
    }
    let token = AckToken {
        client: shared.client_identity,
        generation: shared.connection_generation.load(Ordering::Acquire),
        serial: shared.next_ack_serial.fetch_add(1, Ordering::Relaxed),
    };
    acknowledgements.insert(token, ack);
    drop(acknowledgements);
    Some(token)
}

fn complete_acknowledgement(shared: &Shared, key: AckKey) {
    let sender = shared
        .acknowledgement_completions
        .lock()
        .expect("ACK completion map poisoned")
        .remove(&key);
    if let Some(sender) = sender {
        _ = sender.send(Ok(Completion::Acknowledged));
    }
}

fn snapshot_v4(eventloop: &rumqttc_v4::EventLoop) -> DiagnosticsSnapshot {
    let diagnostics = eventloop.diagnostics();
    DiagnosticsSnapshot {
        connected: diagnostics.connected,
        disconnecting: diagnostics.disconnecting,
        pending_requests: diagnostics.queues.pending_len,
        queued_requests: diagnostics.queues.requests_rx_len
            + diagnostics.queues.control_requests_rx_len,
        inflight_publishes: diagnostics.outbound.inflight,
        max_inflight_publishes: diagnostics.outbound.max_inflight,
        pending_subscribes: diagnostics.outbound.pending_subscribe,
        pending_unsubscribes: diagnostics.outbound.pending_unsubscribe,
        outbound_drained: diagnostics.outbound.outbound_drained,
    }
}

fn snapshot_v5(eventloop: &rumqttc_v5::EventLoop) -> DiagnosticsSnapshot {
    let diagnostics = eventloop.diagnostics();
    DiagnosticsSnapshot {
        connected: diagnostics.connected,
        disconnecting: diagnostics.disconnecting,
        pending_requests: diagnostics.queues.pending_len,
        queued_requests: diagnostics.queues.requests_rx_len
            + diagnostics.queues.control_requests_rx_len,
        inflight_publishes: diagnostics.outbound.inflight,
        max_inflight_publishes: diagnostics.outbound.max_inflight,
        pending_subscribes: diagnostics.outbound.pending_subscribe,
        pending_unsubscribes: diagnostics.outbound.pending_unsubscribe,
        outbound_drained: diagnostics.outbound.outbound_drained,
    }
}

const fn v4_publish_options(command: &PublishCommand) -> rumqttc_v4::PublishOptions {
    rumqttc_v4::PublishOptions::new(to_v4_qos(command.qos)).retain(command.retain)
}

fn v5_publish_options(command: &PublishCommand) -> rumqttc_v5::PublishOptions {
    let options = rumqttc_v5::PublishOptions::new(to_v5_qos(command.qos)).retain(command.retain);
    if let Some(properties) = command.v5_properties.clone() {
        options.properties(to_v5_properties(properties))
    } else {
        options
    }
}

fn validate_outbound_v5_publish(
    properties: Option<&V5PublishProperties>,
    payload: &[u8],
    topic: &str,
    qos: QoS,
    retain: bool,
    capabilities: V5PublishCapabilities,
    topic_aliases: &HashMap<u16, String>,
) -> Result<()> {
    let topic_alias = properties.and_then(|properties| properties.topic_alias);
    if topic.is_empty() && topic_alias.is_none() {
        return Err(protocol_option_error(
            "an empty MQTT 5 topic requires a nonzero Topic Alias",
        ));
    }

    let Some(properties) = properties else {
        return validate_negotiated_v5_publish(
            qos,
            retain,
            topic_alias,
            topic.is_empty(),
            capabilities,
            topic_aliases,
        );
    };

    // MQTT-3.3.4-6: Subscription Identifier is server-to-client only on PUBLISH packets.
    if !properties.subscription_identifiers.is_empty() {
        return Err(protocol_option_error(
            "client-originated MQTT 5 publishes cannot contain subscription identifiers",
        ));
    }

    match properties.payload_format_indicator {
        None | Some(0) => {}
        Some(1) => {
            std::str::from_utf8(payload).map_err(|_| {
                protocol_option_error(
                    "payload format indicator 1 requires a well-formed UTF-8 payload",
                )
            })?;
        }
        Some(value) => {
            return Err(protocol_option_error(format!(
                "payload format indicator must be 0 or 1, got {value}",
            )));
        }
    }

    if let Some(alias) = topic_alias
        && alias == 0
    {
        return Err(protocol_option_error("MQTT 5 topic alias cannot be zero"));
    }

    if let Some(response_topic) = &properties.response_topic {
        validate_mqtt_utf8_string(response_topic, "response topic")?;
        if response_topic.is_empty() {
            return Err(protocol_option_error("response topic cannot be empty"));
        }
        if response_topic.contains(['+', '#']) {
            return Err(protocol_option_error(
                "response topic cannot contain wildcard characters",
            ));
        }
    }

    if let Some(correlation_data) = &properties.correlation_data {
        validate_mqtt_binary_data(correlation_data, "correlation data")?;
    }
    for (key, value) in &properties.user_properties {
        validate_mqtt_utf8_string(key, "user property key")?;
        validate_mqtt_utf8_string(value, "user property value")?;
    }
    if let Some(content_type) = &properties.content_type {
        validate_mqtt_utf8_string(content_type, "content type")?;
    }

    validate_negotiated_v5_publish(
        qos,
        retain,
        topic_alias,
        topic.is_empty(),
        capabilities,
        topic_aliases,
    )
}

fn validate_negotiated_v5_publish(
    qos: QoS,
    retain: bool,
    topic_alias: Option<u16>,
    topic_is_empty: bool,
    capabilities: V5PublishCapabilities,
    topic_aliases: &HashMap<u16, String>,
) -> Result<()> {
    if !capabilities.known && (qos != QoS::AtMostOnce || retain || topic_alias.is_some()) {
        return Err(Error::new(
            ErrorKind::Backpressure,
            "MQTT 5 broker capabilities are not known until CONNACK",
        )
        .with_delivery(DeliveryStatus::NotAdmitted));
    }
    if qos as u8 > capabilities.maximum_qos {
        return Err(protocol_option_error(format!(
            "publish QoS {} exceeds the broker maximum QoS {}",
            qos as u8, capabilities.maximum_qos,
        )));
    }
    if retain && !capabilities.retain_available {
        return Err(protocol_option_error(
            "the broker does not support retained publishes",
        ));
    }
    if let Some(alias) = topic_alias {
        if alias > capabilities.topic_alias_max {
            return Err(protocol_option_error(format!(
                "MQTT 5 topic alias {alias} exceeds the broker maximum {}",
                capabilities.topic_alias_max,
            )));
        }
        if topic_is_empty && !topic_aliases.contains_key(&alias) {
            return Err(protocol_option_error(format!(
                "MQTT 5 topic alias {alias} has no mapping on the current connection",
            )));
        }
    }
    Ok(())
}

fn validate_mqtt_utf8_string(value: &str, name: &str) -> Result<()> {
    if value.len() > usize::from(u16::MAX) {
        return Err(protocol_option_error(format!(
            "{name} exceeds the MQTT UTF-8 string limit of {} bytes",
            u16::MAX,
        )));
    }
    if value.contains('\0') {
        return Err(protocol_option_error(format!(
            "{name} cannot contain the null character",
        )));
    }
    Ok(())
}

fn validate_mqtt_binary_data(value: &[u8], name: &str) -> Result<()> {
    if value.len() > usize::from(u16::MAX) {
        return Err(protocol_option_error(format!(
            "{name} exceeds the MQTT binary data limit of {} bytes",
            u16::MAX,
        )));
    }
    Ok(())
}

const fn to_v4_qos(qos: QoS) -> rumqttc_v4::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc_v4::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc_v4::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc_v4::QoS::ExactlyOnce,
    }
}
const fn to_v5_qos(qos: QoS) -> rumqttc_v5::QoS {
    match qos {
        QoS::AtMostOnce => rumqttc_v5::QoS::AtMostOnce,
        QoS::AtLeastOnce => rumqttc_v5::QoS::AtLeastOnce,
        QoS::ExactlyOnce => rumqttc_v5::QoS::ExactlyOnce,
    }
}
const fn from_v4_qos(qos: rumqttc_v4::QoS) -> QoS {
    match qos {
        rumqttc_v4::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc_v4::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc_v4::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}
const fn from_v5_qos(qos: rumqttc_v5::QoS) -> QoS {
    match qos {
        rumqttc_v5::QoS::AtMostOnce => QoS::AtMostOnce,
        rumqttc_v5::QoS::AtLeastOnce => QoS::AtLeastOnce,
        rumqttc_v5::QoS::ExactlyOnce => QoS::ExactlyOnce,
    }
}

fn to_v5_properties(properties: V5PublishProperties) -> rumqttc_v5::PublishProperties {
    debug_assert!(properties.subscription_identifiers.is_empty());
    rumqttc_v5::PublishProperties {
        payload_format_indicator: properties.payload_format_indicator,
        message_expiry_interval: properties.message_expiry_interval,
        topic_alias: properties.topic_alias,
        response_topic: properties.response_topic,
        correlation_data: properties.correlation_data,
        user_properties: properties.user_properties,
        // Validation reports this to the caller; keeping the wire conversion empty is a final
        // protocol-safety invariant if another internal call site is added later.
        subscription_identifiers: Vec::new(),
        content_type: properties.content_type,
    }
}

fn from_v5_properties(properties: rumqttc_v5::PublishProperties) -> V5PublishProperties {
    V5PublishProperties {
        response_topic: properties.response_topic,
        correlation_data: properties.correlation_data,
        content_type: properties.content_type,
        payload_format_indicator: properties.payload_format_indicator,
        topic_alias: properties.topic_alias,
        subscription_identifiers: properties.subscription_identifiers,
        message_expiry_interval: properties.message_expiry_interval,
        user_properties: properties.user_properties,
    }
}

fn map_v4_publish_notice(
    result: std::result::Result<rumqttc_v4::PublishResult, rumqttc_v4::PublishNoticeError>,
) -> Result<Completion> {
    result
        .map(|result| {
            Completion::Publish(match result {
                rumqttc_v4::PublishResult::Qos0Flushed => PublishCompletion::Qos0Flushed,
                rumqttc_v4::PublishResult::Qos1(_) => PublishCompletion::Qos1Acknowledged,
                rumqttc_v4::PublishResult::Qos2Completed(_) => PublishCompletion::Qos2Completed,
            })
        })
        .map_err(map_notice_error)
}

fn map_v5_publish_notice(
    result: std::result::Result<rumqttc_v5::PublishResult, rumqttc_v5::PublishNoticeError>,
) -> Result<Completion> {
    match result {
        Ok(rumqttc_v5::PublishResult::Qos0Flushed) => {
            Ok(Completion::Publish(PublishCompletion::Qos0Flushed))
        }
        Ok(rumqttc_v5::PublishResult::Qos1(ack)) if v5_puback_success(ack.reason) => {
            Ok(Completion::Publish(PublishCompletion::Qos1Acknowledged))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Completed(ack)) if v5_pubcomp_success(ack.reason) => {
            Ok(Completion::Publish(PublishCompletion::Qos2Completed))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Recovered(_)) => {
            Ok(Completion::Publish(PublishCompletion::Qos2Completed))
        }
        Ok(rumqttc_v5::PublishResult::Qos1(ack)) => {
            Err(broker_rejection(v5_puback_code(ack.reason)))
        }
        Ok(rumqttc_v5::PublishResult::Qos2Completed(ack)) => {
            Err(broker_rejection(v5_pubcomp_code(ack.reason)))
        }
        Ok(rumqttc_v5::PublishResult::Qos2PubRecRejected(ack)) => {
            Err(broker_rejection(v5_pubrec_code(ack.reason)))
        }
        Err(error) => Err(map_notice_error(error)),
    }
}

fn map_v4_subscribe_notice(
    result: std::result::Result<rumqttc_v4::SubAck, rumqttc_v4::SubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Subscribe(SubscribeCompletion {
                results: ack
                    .return_codes
                    .into_iter()
                    .map(|reason| match reason {
                        rumqttc_v4::SubscribeReasonCode::Success(qos) => {
                            SubscribeResult::Granted(from_v4_qos(qos))
                        }
                        rumqttc_v4::SubscribeReasonCode::Failure => {
                            SubscribeResult::Rejected(BrokerReason { code: 0x80 })
                        }
                    })
                    .collect(),
            })
        })
        .map_err(map_notice_error)
}

fn map_v5_subscribe_notice(
    result: std::result::Result<rumqttc_v5::SubAck, rumqttc_v5::SubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Subscribe(SubscribeCompletion {
                results: ack
                    .return_codes
                    .into_iter()
                    .map(|reason| match reason {
                        rumqttc_v5::SubscribeReasonCode::Success(qos) => {
                            SubscribeResult::Granted(from_v5_qos(qos))
                        }
                        reason => SubscribeResult::Rejected(BrokerReason {
                            code: v5_suback_code(reason),
                        }),
                    })
                    .collect(),
            })
        })
        .map_err(map_notice_error)
}

fn map_v4_unsubscribe_notice(
    result: std::result::Result<rumqttc_v4::UnsubAck, rumqttc_v4::UnsubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|_| Completion::Unsubscribe(UnsubscribeCompletion { results: None }))
        .map_err(map_notice_error)
}

fn map_v5_unsubscribe_notice(
    result: std::result::Result<rumqttc_v5::UnsubAck, rumqttc_v5::UnsubscribeNoticeError>,
) -> Result<Completion> {
    result
        .map(|ack| {
            Completion::Unsubscribe(UnsubscribeCompletion {
                results: Some(
                    ack.reasons
                        .into_iter()
                        .map(|reason| match reason {
                            rumqttc_v5::UnsubAckReason::Success => UnsubscribeResult::Success,
                            rumqttc_v5::UnsubAckReason::NoSubscriptionExisted => {
                                UnsubscribeResult::NoSubscriptionExisted
                            }
                            reason => {
                                UnsubscribeResult::Rejected(BrokerReason { code: reason as u8 })
                            }
                        })
                        .collect(),
                ),
            })
        })
        .map_err(map_notice_error)
}

fn map_notice_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> Error {
    Error::sourced(ErrorKind::Protocol, DeliveryStatus::Ambiguous, error)
}

fn broker_rejection(code: u8) -> Error {
    Error::new(
        ErrorKind::Protocol,
        format!("broker rejected operation with reason code 0x{code:02x}"),
    )
    .with_delivery(DeliveryStatus::Rejected)
    .with_broker_reason(code)
}

fn protocol_option_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Admission, message).with_delivery(DeliveryStatus::NotAdmitted)
}

fn map_v4_client_error(error: rumqttc_v4::ClientError) -> Error {
    let kind = match error {
        rumqttc_v4::ClientError::RequestChannelFull(_) => ErrorKind::Backpressure,
        rumqttc_v4::ClientError::RequestChannelDisconnected(_) => ErrorKind::Shutdown,
        _ => ErrorKind::Admission,
    };
    Error::sourced(kind, DeliveryStatus::NotAdmitted, error)
}

fn map_v5_client_error(error: rumqttc_v5::ClientError) -> Error {
    let kind = match error {
        rumqttc_v5::ClientError::RequestChannelFull(_) => ErrorKind::Backpressure,
        rumqttc_v5::ClientError::RequestChannelDisconnected(_) => ErrorKind::Shutdown,
        _ => ErrorKind::Admission,
    };
    Error::sourced(kind, DeliveryStatus::NotAdmitted, error)
}

fn map_v4_connection_error(error: rumqttc_v4::ConnectionError) -> Error {
    let kind = match error {
        rumqttc_v4::ConnectionError::Tls(_) => ErrorKind::Tls,
        rumqttc_v4::ConnectionError::ConnectionRefused(
            rumqttc_v4::ConnectReturnCode::BadUserNamePassword
            | rumqttc_v4::ConnectReturnCode::NotAuthorized,
        ) => ErrorKind::Authentication,
        rumqttc_v4::ConnectionError::SessionStore(_)
        | rumqttc_v4::ConnectionError::SessionRestore(_) => ErrorKind::Persistence,
        rumqttc_v4::ConnectionError::NetworkTimeout
        | rumqttc_v4::ConnectionError::FlushTimeout
        | rumqttc_v4::ConnectionError::DisconnectTimeout => ErrorKind::Timeout,
        rumqttc_v4::ConnectionError::Io(_)
        | rumqttc_v4::ConnectionError::Websocket(_)
        | rumqttc_v4::ConnectionError::WsConnect(_) => ErrorKind::Network,
        _ => ErrorKind::Protocol,
    };
    Error::sourced(kind, DeliveryStatus::Ambiguous, error)
}

fn map_v5_connection_error(error: rumqttc_v5::ConnectionError) -> Error {
    let kind = match error {
        rumqttc_v5::ConnectionError::Tls(_) => ErrorKind::Tls,
        rumqttc_v5::ConnectionError::ConnectionRefused(
            rumqttc_v5::ConnectReturnCode::BadUserNamePassword
            | rumqttc_v5::ConnectReturnCode::NotAuthorized
            | rumqttc_v5::ConnectReturnCode::BadAuthenticationMethod,
        ) => ErrorKind::Authentication,
        rumqttc_v5::ConnectionError::SessionStore(_)
        | rumqttc_v5::ConnectionError::SessionRestore(_) => ErrorKind::Persistence,
        rumqttc_v5::ConnectionError::Timeout(_)
        | rumqttc_v5::ConnectionError::DisconnectTimeout => ErrorKind::Timeout,
        rumqttc_v5::ConnectionError::Io(_)
        | rumqttc_v5::ConnectionError::Websocket(_)
        | rumqttc_v5::ConnectionError::WsConnect(_) => ErrorKind::Network,
        _ => ErrorKind::Protocol,
    };
    Error::sourced(kind, DeliveryStatus::Ambiguous, error)
}

fn duration_to_u16(duration: Duration, name: &str) -> Result<u16> {
    u16::try_from(duration.as_secs())
        .map_err(|_| Error::configuration(format!("{name} exceeds u16 seconds")))
}

const fn map_v4_outgoing(outgoing: &rumqttc_v4::Outgoing) -> OutgoingActivity {
    match outgoing {
        rumqttc_v4::Outgoing::Publish(_) => OutgoingActivity::Publish,
        rumqttc_v4::Outgoing::Subscribe(_) => OutgoingActivity::Subscribe,
        rumqttc_v4::Outgoing::Unsubscribe(_) => OutgoingActivity::Unsubscribe,
        rumqttc_v4::Outgoing::PubAck(_)
        | rumqttc_v4::Outgoing::PubRec(_)
        | rumqttc_v4::Outgoing::PubRel(_)
        | rumqttc_v4::Outgoing::PubComp(_) => OutgoingActivity::Acknowledgement,
        rumqttc_v4::Outgoing::PingReq | rumqttc_v4::Outgoing::PingResp => OutgoingActivity::Ping,
        rumqttc_v4::Outgoing::Disconnect => OutgoingActivity::Disconnect,
        rumqttc_v4::Outgoing::AwaitAck(_) => OutgoingActivity::AwaitAcknowledgement,
    }
}

const fn map_v5_outgoing(outgoing: &rumqttc_v5::Outgoing) -> OutgoingActivity {
    match outgoing {
        rumqttc_v5::Outgoing::Publish(_) => OutgoingActivity::Publish,
        rumqttc_v5::Outgoing::Subscribe(_) => OutgoingActivity::Subscribe,
        rumqttc_v5::Outgoing::Unsubscribe(_) => OutgoingActivity::Unsubscribe,
        rumqttc_v5::Outgoing::PubAck(_)
        | rumqttc_v5::Outgoing::PubRec(_)
        | rumqttc_v5::Outgoing::PubRel(_)
        | rumqttc_v5::Outgoing::PubComp(_) => OutgoingActivity::Acknowledgement,
        rumqttc_v5::Outgoing::PingReq | rumqttc_v5::Outgoing::PingResp => OutgoingActivity::Ping,
        rumqttc_v5::Outgoing::Disconnect => OutgoingActivity::Disconnect,
        rumqttc_v5::Outgoing::AwaitAck(_) => OutgoingActivity::AwaitAcknowledgement,
        rumqttc_v5::Outgoing::Auth => OutgoingActivity::Other,
    }
}

const fn v5_suback_code(reason: rumqttc_v5::SubscribeReasonCode) -> u8 {
    use rumqttc_v5::SubscribeReasonCode as R;
    match reason {
        R::Success(qos) => from_v5_qos(qos) as u8,
        R::Failure | R::Unspecified => 0x80,
        R::ImplementationSpecific => 0x83,
        R::NotAuthorized => 0x87,
        R::TopicFilterInvalid => 0x8f,
        R::PkidInUse => 0x91,
        R::QuotaExceeded => 0x97,
        R::SharedSubscriptionsNotSupported => 0x9e,
        R::SubscriptionIdNotSupported => 0xa1,
        R::WildcardSubscriptionsNotSupported => 0xa2,
    }
}

const fn v5_puback_success(reason: rumqttc_v5::PubAckReason) -> bool {
    matches!(
        reason,
        rumqttc_v5::PubAckReason::Success | rumqttc_v5::PubAckReason::NoMatchingSubscribers
    )
}
const fn v5_puback_code(reason: rumqttc_v5::PubAckReason) -> u8 {
    reason as u8
}
const fn v5_pubrec_code(reason: rumqttc_v5::PubRecReason) -> u8 {
    reason as u8
}
const fn v5_pubcomp_code(reason: rumqttc_v5::PubCompReason) -> u8 {
    reason as u8
}
fn v5_pubcomp_success(reason: rumqttc_v5::PubCompReason) -> bool {
    reason == rumqttc_v5::PubCompReason::Success
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use futures_util::task::noop_waker_ref;

    use super::*;

    fn idle_v4_shared() -> (Arc<Shared>, ProtocolDriver, Receiver<DiagnosticsRequest>) {
        let mut config = ClientConfig::v311("unit-test", "127.0.0.1", 1883);
        config.common.request_channel_capacity = 1;
        let (client, driver) = build_protocol(config).unwrap();
        let (completion_tx, _) = flume::unbounded();
        let (diagnostics_tx, diagnostics_rx) = flume::unbounded();
        let (immediate_shutdown_tx, _) = flume::unbounded();
        let shared = Arc::new(Shared {
            client,
            client_identity: 1,
            connection_generation: AtomicU64::new(1),
            broker_topic_alias_max: AtomicU16::new(0),
            broker_maximum_qos: AtomicU8::new(QoS::ExactlyOnce as u8),
            broker_retain_available: AtomicBool::new(true),
            broker_capabilities_known: AtomicBool::new(false),
            next_ack_serial: AtomicU64::new(2),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_kind: AtomicU8::new(0),
            shutdown_operation: AtomicU64::new(0),
            shutdown_registration_ready: AtomicBool::new(true),
            handle_count: AtomicUsize::new(1),
            admission_gate: Mutex::new(()),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            outbound_topic_aliases: Mutex::new(HashMap::new()),
            completion_tx,
            diagnostics_tx,
            immediate_shutdown_tx,
            request_progress: Notify::new(),
        });
        (shared, driver, diagnostics_rx)
    }

    fn idle_v5_shared() -> (
        Arc<Shared>,
        ProtocolDriver,
        Receiver<CompletionRegistration>,
    ) {
        let mut config = ClientConfig::v5("unit-test", "127.0.0.1", 1883);
        config.common.request_channel_capacity = 2;
        let (client, driver) = build_protocol(config).unwrap();
        let (completion_tx, completion_rx) = flume::unbounded();
        let (diagnostics_tx, _) = flume::unbounded();
        let (immediate_shutdown_tx, _) = flume::unbounded();
        let shared = Arc::new(Shared {
            client,
            client_identity: 1,
            connection_generation: AtomicU64::new(1),
            broker_topic_alias_max: AtomicU16::new(1),
            broker_maximum_qos: AtomicU8::new(QoS::ExactlyOnce as u8),
            broker_retain_available: AtomicBool::new(true),
            broker_capabilities_known: AtomicBool::new(true),
            next_ack_serial: AtomicU64::new(1),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_kind: AtomicU8::new(0),
            shutdown_operation: AtomicU64::new(0),
            shutdown_registration_ready: AtomicBool::new(true),
            handle_count: AtomicUsize::new(1),
            admission_gate: Mutex::new(()),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            outbound_topic_aliases: Mutex::new(HashMap::from([(1, "mapped/topic".into())])),
            completion_tx,
            diagnostics_tx,
            immediate_shutdown_tx,
            request_progress: Notify::new(),
        });
        (shared, driver, completion_rx)
    }

    #[test]
    fn timed_event_receive_wakes_for_terminal_status() {
        let (event_tx, event_rx) = flume::bounded(1);
        let (terminal_tx, terminal_rx) = flume::bounded(1);
        let mut consumer = EventConsumer {
            events: event_rx,
            terminal: terminal_rx,
            terminal_seen: false,
        };
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            terminal_tx
                .send(TerminalStatus::Closed { graceful: true })
                .unwrap();
        });

        let started = Instant::now();
        assert!(matches!(
            consumer.recv_timeout(Duration::from_secs(2)).unwrap(),
            Some(WrapperEvent::GracefulShutdownCompleted)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));

        let started = Instant::now();
        assert!(
            consumer
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(1));

        drop(event_tx);
        sender.join().unwrap();
    }

    #[tokio::test]
    async fn immediate_shutdown_remains_observable_across_event_deliveries() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        let (events_tx, _events_rx) = flume::bounded(1);
        events_tx
            .send(WrapperEvent::GracefulShutdownCompleted)
            .unwrap();
        let (shutdown_tx, shutdown_rx) = flume::unbounded();
        let delivery = EventDelivery {
            shared: &shared,
            events: &events_tx,
            timeout: Duration::from_secs(1),
            immediate_shutdown: &shutdown_rx,
        };
        let trigger = async {
            tokio::task::yield_now().await;
            shared.shutdown_kind.store(2, Ordering::Release);
            shutdown_tx.send(()).unwrap();
        };

        let (first, ()) = tokio::join!(
            deliver(&delivery, WrapperEvent::GracefulShutdownCompleted),
            trigger,
        );
        assert!(first);
        assert!(
            shutdown_rx.is_empty(),
            "the wake-up edge should be consumed"
        );

        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                deliver(&delivery, WrapperEvent::GracefulShutdownCompleted),
            )
            .await
            .expect("persistent shutdown state must bypass event backpressure")
        );
    }

    #[test]
    fn capability_dependent_v5_publishes_wait_for_connack() {
        let (shared, driver, _completion_rx) = idle_v5_shared();
        shared
            .broker_capabilities_known
            .store(false, Ordering::Release);
        shared.outbound_topic_aliases.lock().unwrap().clear();
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };

        for command in [
            PublishCommand {
                topic: "qos".into(),
                payload: Bytes::new(),
                qos: QoS::AtLeastOnce,
                retain: false,
                v5_properties: None,
            },
            PublishCommand {
                topic: "retain".into(),
                payload: Bytes::new(),
                qos: QoS::AtMostOnce,
                retain: true,
                v5_properties: None,
            },
            PublishCommand {
                topic: "alias".into(),
                payload: Bytes::new(),
                qos: QoS::AtMostOnce,
                retain: false,
                v5_properties: Some(V5PublishProperties {
                    topic_alias: Some(1),
                    ..V5PublishProperties::default()
                }),
            },
        ] {
            let error = handle.try_publish(command).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Backpressure);
            assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
        }

        handle
            .try_publish(PublishCommand {
                topic: "always/supported".into(),
                payload: Bytes::new(),
                qos: QoS::AtMostOnce,
                retain: false,
                v5_properties: None,
            })
            .unwrap();
        let ProtocolDriver::V5(eventloop) = driver else {
            unreachable!();
        };
        assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 1);
    }

    #[test]
    fn malformed_publish_topics_are_rejected_before_admission() {
        for topic in [
            "invalid\0topic".to_owned(),
            "a".repeat(usize::from(u16::MAX) + 1),
        ] {
            let (shared, driver, _completion_rx) = idle_v4_shared();
            let handle = ClientHandle {
                shared: Arc::clone(&shared),
            };
            let error = handle
                .try_publish(PublishCommand {
                    topic: topic.clone(),
                    payload: Bytes::new(),
                    qos: QoS::AtMostOnce,
                    retain: false,
                    v5_properties: None,
                })
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Admission);
            assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
            let ProtocolDriver::V311(eventloop) = driver else {
                unreachable!();
            };
            assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 0);

            let (shared, driver, _completion_rx) = idle_v5_shared();
            let handle = ClientHandle {
                shared: Arc::clone(&shared),
            };
            let error = handle
                .try_publish(PublishCommand {
                    topic,
                    payload: Bytes::new(),
                    qos: QoS::AtMostOnce,
                    retain: false,
                    v5_properties: None,
                })
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Admission);
            assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
            let ProtocolDriver::V5(eventloop) = driver else {
                unreachable!();
            };
            assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 0);
        }
    }

    #[tokio::test]
    async fn async_v5_publish_resumes_when_connack_capabilities_arrive() {
        let (shared, driver, _completion_rx) = idle_v5_shared();
        shared
            .broker_capabilities_known
            .store(false, Ordering::Release);
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let admission = handle.publish(PublishCommand {
            topic: "after/connack".into(),
            payload: Bytes::new(),
            qos: QoS::AtLeastOnce,
            retain: false,
            v5_properties: None,
        });
        tokio::pin!(admission);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), admission.as_mut())
                .await
                .is_err()
        );

        {
            let _admission_guard = shared.admission_gate.lock().unwrap();
            shared.broker_maximum_qos.store(1, Ordering::Release);
            shared
                .broker_retain_available
                .store(false, Ordering::Release);
            shared
                .broker_capabilities_known
                .store(true, Ordering::Release);
        }
        shared.request_progress.notify_waiters();

        tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .expect("CONNACK should wake capability-dependent admission")
            .unwrap();
        let ProtocolDriver::V5(eventloop) = driver else {
            unreachable!();
        };
        assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 1);
    }

    #[tokio::test]
    async fn terminal_lifecycle_wakes_capacity_waiting_admission() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        let client = match &shared.client {
            ProtocolClient::V311(client) => client,
            ProtocolClient::V5(_) => unreachable!(),
        };
        client
            .try_publish(
                "fill/request/channel",
                Bytes::from_static(b"payload"),
                rumqttc_v4::PublishOptions::new(rumqttc_v4::QoS::AtMostOnce),
            )
            .unwrap();
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let admission = handle.admit_async(Command::Publish(PublishCommand {
            topic: "waiting/for/capacity".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            v5_properties: None,
        }));
        tokio::pin!(admission);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), admission.as_mut())
                .await
                .is_err(),
            "the admission should initially wait for request capacity"
        );

        let terminal = TerminalStatus::Failed(Error::new(ErrorKind::Network, "driver failed"));
        publish_terminal_lifecycle(&shared, &terminal);

        let error = tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .expect("terminal lifecycle should wake the admission")
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Shutdown);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }

    #[tokio::test]
    async fn graceful_shutdown_completes_diagnostics_admitted_before_barrier() {
        let (shared, driver, diagnostics_rx) = idle_v4_shared();
        let ProtocolDriver::V311(eventloop) = driver else {
            unreachable!();
        };
        let diagnostics = snapshot_v4(&eventloop);
        let admission = ClientHandle {
            shared: Arc::clone(&shared),
        }
        .try_diagnostics()
        .unwrap();
        shared
            .lifecycle
            .store(LifecycleState::Closing as u8, Ordering::Release);
        shared.shutdown_kind.store(1, Ordering::Release);
        let (_completion_tx, completion_rx) = flume::unbounded();
        let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
        let mut pending = FuturesUnordered::new();
        let mut senders = HashMap::new();

        assert!(complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await);
        assert_eq!(
            admission.completion.wait().unwrap(),
            Completion::Diagnostics(diagnostics)
        );
    }

    #[tokio::test]
    async fn immediate_shutdown_does_not_claim_queued_diagnostics_completed() {
        let (shared, driver, diagnostics_rx) = idle_v4_shared();
        let ProtocolDriver::V311(eventloop) = driver else {
            unreachable!();
        };
        let diagnostics = snapshot_v4(&eventloop);
        let admission = ClientHandle {
            shared: Arc::clone(&shared),
        }
        .try_diagnostics()
        .unwrap();
        shared
            .lifecycle
            .store(LifecycleState::Closing as u8, Ordering::Release);
        shared.shutdown_kind.store(2, Ordering::Release);
        let (_completion_tx, completion_rx) = flume::unbounded();
        let shutdown = ShutdownInputs::new(&shared, &completion_rx, &diagnostics_rx);
        let mut pending = FuturesUnordered::new();
        let mut senders = HashMap::new();

        assert!(!complete_shutdown(&shutdown, &diagnostics, &mut pending, &mut senders).await);
        assert_eq!(diagnostics_rx.len(), 1);
        let error = admission
            .completion
            .wait_timeout(Duration::ZERO)
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Timeout);
    }

    #[test]
    fn v5_connection_invalidation_repairs_publish_admitted_after_cleanup() {
        let (shared, driver, _completion_rx) = idle_v5_shared();
        let ProtocolDriver::V5(mut eventloop) = driver else {
            unreachable!();
        };
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let _mapping_admission = handle
            .try_publish(PublishCommand {
                topic: "mapped/topic".into(),
                payload: Bytes::from_static(b"mapping"),
                qos: QoS::AtLeastOnce,
                retain: false,
                v5_properties: Some(V5PublishProperties {
                    topic_alias: Some(1),
                    ..V5PublishProperties::default()
                }),
            })
            .unwrap();
        eventloop.clean();
        let _admission = handle
            .try_publish(PublishCommand {
                topic: String::new(),
                payload: Bytes::from_static(b"payload"),
                qos: QoS::AtLeastOnce,
                retain: false,
                v5_properties: Some(V5PublishProperties {
                    topic_alias: Some(1),
                    ..V5PublishProperties::default()
                }),
            })
            .unwrap();
        assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 1);

        invalidate_v5_connection(
            &mut eventloop,
            &shared,
            &Error::new(ErrorKind::Network, "connection lost"),
        );

        assert_eq!(eventloop.diagnostics().queues.requests_rx_len, 0);
        assert_eq!(eventloop.pending_len(), 2);
        assert_eq!(shared.broker_topic_alias_max.load(Ordering::Acquire), 0);
        assert!(shared.outbound_topic_aliases.lock().unwrap().is_empty());
    }

    #[test]
    fn unsupported_v5_publish_does_not_establish_topic_alias() {
        for (maximum_qos, retain_available, qos, retain) in [
            (QoS::AtMostOnce as u8, true, QoS::AtLeastOnce, false),
            (QoS::ExactlyOnce as u8, false, QoS::AtMostOnce, true),
        ] {
            let (shared, _driver, _completion_rx) = idle_v5_shared();
            shared.outbound_topic_aliases.lock().unwrap().clear();
            shared
                .broker_maximum_qos
                .store(maximum_qos, Ordering::Release);
            shared
                .broker_retain_available
                .store(retain_available, Ordering::Release);
            let handle = ClientHandle {
                shared: Arc::clone(&shared),
            };

            let binding_error = handle
                .try_publish(PublishCommand {
                    topic: "binding/topic".into(),
                    payload: Bytes::from_static(b"binding"),
                    qos,
                    retain,
                    v5_properties: Some(V5PublishProperties {
                        topic_alias: Some(1),
                        ..V5PublishProperties::default()
                    }),
                })
                .unwrap_err();
            assert_eq!(binding_error.kind(), ErrorKind::Admission);
            assert_eq!(binding_error.delivery_status(), DeliveryStatus::NotAdmitted);
            assert!(shared.outbound_topic_aliases.lock().unwrap().is_empty());

            let alias_only_error = handle
                .try_publish(PublishCommand {
                    topic: String::new(),
                    payload: Bytes::from_static(b"alias-only"),
                    qos: QoS::AtMostOnce,
                    retain: false,
                    v5_properties: Some(V5PublishProperties {
                        topic_alias: Some(1),
                        ..V5PublishProperties::default()
                    }),
                })
                .unwrap_err();
            assert_eq!(alias_only_error.kind(), ErrorKind::Admission);
            assert_eq!(
                alias_only_error.delivery_status(),
                DeliveryStatus::NotAdmitted
            );
        }
    }

    #[test]
    fn v5_password_only_auth_is_preserved_in_protocol_options() {
        let mut common = crate::CommonConfig::new("client", "localhost", 1883);
        common.password = Some(Bytes::from_static(b"secret"));

        let options = build_v5_options(&common).unwrap();
        assert_eq!(
            options.auth(),
            &rumqttc_v5::ConnectAuth::Password {
                password: Bytes::from_static(b"secret"),
            }
        );
    }

    #[test]
    fn cancelling_pending_async_ack_restores_its_token() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        let client = match &shared.client {
            ProtocolClient::V311(client) => client,
            ProtocolClient::V5(_) => unreachable!(),
        };
        client
            .try_manual_ack(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(99)))
            .unwrap();
        let token = AckToken {
            client: 1,
            generation: 1,
            serial: 1,
        };
        shared.acknowledgements.lock().unwrap().insert(
            token,
            PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7))),
        );
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let mut future = Box::pin(handle.admit_async(Command::Acknowledge(token)));
        let mut context = Context::from_waker(noop_waker_ref());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));

        drop(future);
        let reservation = handle.reserve_ack(token).unwrap();
        reservation.commit();
    }

    #[test]
    fn retransmitted_publish_tokens_are_coalesced_and_consumed_together() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        let first_ack =
            PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7)));
        let duplicate_ack =
            PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7)));
        let first_token = insert_ack(&shared, first_ack).unwrap();
        let duplicate_token = insert_ack(&shared, duplicate_ack).unwrap();
        assert_eq!(duplicate_token, first_token);
        assert_eq!(shared.acknowledgements.lock().unwrap().by_token.len(), 1);

        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let reservation = handle.reserve_ack(first_token).unwrap();
        let completion = handle.try_enqueue_ack(reservation.ack()).unwrap();
        reservation.commit();

        assert!(
            insert_ack(
                &shared,
                PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7))),
            )
            .is_none(),
            "a retransmission must not create a token while its ACK is queued"
        );
        complete_acknowledgement(&shared, AckKey::V311PubAck(7));
        assert_eq!(
            futures_executor::block_on(completion).unwrap(),
            Completion::Acknowledged
        );

        let next_token = insert_ack(
            &shared,
            PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7))),
        )
        .unwrap();
        assert_ne!(next_token, first_token);
        assert!(handle.reserve_ack(first_token).is_err());
        let reservation = handle.reserve_ack(next_token).unwrap();
        reservation.commit();
    }

    #[test]
    fn connack_boundary_discards_ack_admitted_after_connection_cleanup() {
        let (shared, driver, _diagnostics_rx) = idle_v4_shared();
        let ProtocolDriver::V311(mut eventloop) = driver else {
            unreachable!();
        };
        let token = insert_ack(
            &shared,
            PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7))),
        )
        .unwrap();
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let reservation = handle.reserve_ack(token).unwrap();
        let completion = handle.try_enqueue_ack(reservation.ack()).unwrap();
        reservation.commit();
        assert_eq!(eventloop.diagnostics().queues.control_requests_rx_len, 1);

        invalidate_acks(&shared, &Error::new(ErrorKind::Network, "connection lost"));
        assert!(futures_executor::block_on(completion).is_err());
        let mut connected = false;
        assert!(matches!(
            map_v4_event(
                &mut eventloop,
                rumqttc_v4::Event::Incoming(rumqttc_v4::Packet::ConnAck(rumqttc_v4::ConnAck::new(
                    rumqttc_v4::ConnectReturnCode::Success,
                    false
                ),)),
                &shared,
                &mut connected,
                false,
                true,
                ProtocolVersion::V311,
            ),
            Some(WrapperEvent::Connected { .. })
        ));
        assert!(connected);
        assert_eq!(eventloop.diagnostics().queues.control_requests_rx_len, 0);
    }

    #[test]
    fn pending_async_publish_cannot_cross_shutdown_transition() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        let client = match &shared.client {
            ProtocolClient::V311(client) => client,
            ProtocolClient::V5(_) => unreachable!(),
        };
        client
            .try_publish(
                "fill/request/channel",
                Bytes::from_static(b"payload"),
                rumqttc_v4::PublishOptions::new(rumqttc_v4::QoS::AtMostOnce),
            )
            .unwrap();
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let command = Command::Publish(PublishCommand {
            topic: "cannot/cross/shutdown".into(),
            payload: Bytes::from_static(b"payload"),
            qos: QoS::AtMostOnce,
            retain: false,
            v5_properties: None,
        });
        let mut future = Box::pin(handle.admit_async(command));
        let mut context = Context::from_waker(noop_waker_ref());
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));

        {
            let _admission_guard = shared.admission_gate.lock().unwrap();
            shared.transition_to_closing().unwrap();
        }

        let Poll::Ready(Err(error)) = future.as_mut().poll(&mut context) else {
            panic!("pending admission did not stop at the shutdown transition");
        };
        assert_eq!(error.kind(), ErrorKind::Shutdown);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }

    #[test]
    fn topic_alias_mappings_reset_on_disconnect() {
        let (shared, _driver, _diagnostics_rx) = idle_v4_shared();
        shared.broker_topic_alias_max.store(1, Ordering::Release);
        shared.broker_maximum_qos.store(0, Ordering::Release);
        shared
            .broker_retain_available
            .store(false, Ordering::Release);
        shared
            .broker_capabilities_known
            .store(true, Ordering::Release);
        shared
            .outbound_topic_aliases
            .lock()
            .unwrap()
            .insert(1, "mapped/topic".into());

        invalidate_acks(&shared, &Error::new(ErrorKind::Network, "connection lost"));

        assert_eq!(shared.broker_topic_alias_max.load(Ordering::Acquire), 0);
        assert_eq!(
            shared.broker_maximum_qos.load(Ordering::Acquire),
            QoS::ExactlyOnce as u8
        );
        assert!(shared.broker_retain_available.load(Ordering::Acquire));
        assert!(!shared.broker_capabilities_known.load(Ordering::Acquire));
        assert!(shared.outbound_topic_aliases.lock().unwrap().is_empty());
    }

    #[test]
    fn outgoing_ack_progress_resolves_ack_completion() {
        let (shared, driver, _diagnostics_rx) = idle_v4_shared();
        let ProtocolDriver::V311(mut eventloop) = driver else {
            unreachable!();
        };
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let ack = PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7)));
        let completion = handle.try_enqueue_ack(&ack).unwrap();
        let mut completion = Box::pin(completion);
        let mut context = Context::from_waker(noop_waker_ref());
        assert!(matches!(
            completion.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let mut connected = true;
        assert!(
            map_v4_event(
                &mut eventloop,
                rumqttc_v4::Event::Outgoing(rumqttc_v4::Outgoing::PubAck(7)),
                &shared,
                &mut connected,
                false,
                true,
                ProtocolVersion::V311,
            )
            .is_none()
        );
        assert_eq!(
            futures_executor::block_on(completion).unwrap(),
            Completion::Acknowledged
        );
    }

    #[test]
    fn v5_recovery_pubcomp_is_completed_without_weakening_non_recovery_rejection() {
        let mut pubcomp = rumqttc_v5::PubComp::new(1, None);
        pubcomp.reason = rumqttc_v5::PubCompReason::PacketIdentifierNotFound;

        assert_eq!(
            map_v5_publish_notice(Ok(rumqttc_v5::PublishResult::Qos2Recovered(
                pubcomp.clone(),
            )))
            .unwrap(),
            Completion::Publish(PublishCompletion::Qos2Completed)
        );

        let error = map_v5_publish_notice(Ok(rumqttc_v5::PublishResult::Qos2Completed(pubcomp)))
            .unwrap_err();
        assert_eq!(error.delivery_status(), DeliveryStatus::Rejected);
        assert_eq!(error.broker_reason(), Some(0x92));
    }
}
