use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use flume::{Receiver, Sender};
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Notify;

use crate::acknowledgement::{AckKey, AcknowledgementRegistry, PreparedAck};
use crate::adapter::{v4 as adapter_v4, v5 as adapter_v5};
use crate::completion::{
    BrokerReason, CompletionCell, SubscribeCompletion, SubscribeResult, UnsubscribeCompletion,
    UnsubscribeResult,
};
use crate::handle::AdmissionGate;
use crate::operations::OperationRegistry;
use crate::runtime::ThreadOwner;
use crate::shutdown::ShutdownRecord;
use crate::{
    AckMode, AckToken, Admission, ClientConfig, Command, Completion, CompletionHandle,
    ConnectionPhase, DeliveryStatus, DiagnosticsSnapshot, Error, ErrorKind, IncomingPublish,
    LifecycleState, OperationId, ProtocolConfig, ProtocolVersion, PublishCommand,
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

struct CompletionRegistration {
    operation_id: OperationId,
    registry: OperationRegistry,
    future: CompletionFuture,
}

struct DiagnosticsRequest {
    operation_id: OperationId,
    registry: OperationRegistry,
}

struct PendingSender {
    registry: OperationRegistry,
}

struct Shared {
    client: ProtocolClient,
    client_identity: u64,
    connection_generation: AtomicU64,
    next_ack_serial: AtomicU64,
    next_operation: AtomicU64,
    lifecycle: AtomicU8,
    shutdown_phase: AtomicU8,
    shutdown: Mutex<ShutdownRecord>,
    handle_count: AtomicUsize,
    admission_gate: AdmissionGate,
    acknowledgements: Mutex<AcknowledgementRegistry>,
    acknowledgement_completions: Mutex<HashMap<AckKey, OperationId>>,
    operations: OperationRegistry,
    completion_tx: Sender<CompletionRegistration>,
    diagnostics_tx: Sender<DiagnosticsRequest>,
    immediate_shutdown_tx: Sender<()>,
    request_progress: Notify,
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
    fn immediate_shutdown_requested(&self) -> bool {
        self.shutdown_phase.load(Ordering::Acquire) == 2
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
        self.register_admission(future)
    }

    fn register_admission(&self, future: CompletionFuture) -> Result<Admission> {
        let operation_id = self.next_operation_id()?;
        let cell = CompletionCell::new(operation_id);
        self.operations.insert(Arc::clone(&cell));
        self.completion_tx
            .send(CompletionRegistration {
                operation_id,
                registry: self.operations.clone(),
                future,
            })
            .map_err(|_| {
                self.operations.complete(
                    operation_id,
                    Err(
                        Error::new(ErrorKind::Shutdown, "driver stopped during admission")
                            .with_delivery(DeliveryStatus::Ambiguous),
                    ),
                );
                Error::new(ErrorKind::Shutdown, "driver stopped during admission")
                    .with_delivery(DeliveryStatus::Ambiguous)
            })?;
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(cell),
        })
    }

    fn shutdown_admission(&self) -> Result<Admission> {
        let operation_id = self.next_operation_id()?;
        let cell = CompletionCell::new(operation_id);
        self.operations.insert(Arc::clone(&cell));
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(cell),
        })
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
        let previous = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if matches!(
            previous,
            ShutdownRecord::Immediate { .. } | ShutdownRecord::Closed | ShutdownRecord::Failed
        ) {
            return;
        }
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
        let mut shutdown = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let ShutdownRecord::Graceful { operation_id, .. } = &previous {
            self.operations.complete(
                *operation_id,
                Err(Error::new(
                    ErrorKind::Shutdown,
                    "the requested graceful shutdown was escalated to immediate shutdown",
                )
                .with_delivery(DeliveryStatus::Ambiguous)),
            );
        }
        *shutdown = ShutdownRecord::Immediate {
            operation_id: None,
            cell: None,
            escalated: !newly_closing,
        };
        self.shutdown_phase.store(2, Ordering::Release);
        drop(shutdown);
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
                    .map_err(adapter_v4::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v4_publish_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let options = v5_publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(adapter_v5::map_client_error)?;
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
                    .map_err(adapter_v4::map_client_error)?;
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
                    .map_err(adapter_v5::map_client_error)?;
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
                    .map_err(adapter_v4::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    map_v4_unsubscribe_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let notice = client
                    .try_unsubscribe_many_tracked(filters)
                    .map_err(adapter_v5::map_client_error)?;
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

    fn try_enqueue_ack(&self, ack: &PreparedAck) -> Result<Admission> {
        let key = ack.key();
        let operation_id = self.shared.next_operation_id()?;
        let cell = CompletionCell::new(operation_id);
        self.shared.operations.insert(Arc::clone(&cell));
        let mut completions = self
            .shared
            .acknowledgement_completions
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "ACK completion map mutex poisoned"))?;
        match completions.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(operation_id);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                self.shared.operations.cancel(operation_id);
                return Err(Error::new(
                    ErrorKind::Internal,
                    "an acknowledgement for this MQTT packet is already pending",
                ));
            }
        }
        let result = match (&self.shared.client, ack) {
            (ProtocolClient::V311(client), PreparedAck::V311(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(adapter_v4::map_client_error),
            (ProtocolClient::V5(client), PreparedAck::V5(ack)) => client
                .try_manual_ack(ack.clone())
                .map_err(adapter_v5::map_client_error),
            _ => Err(Error::new(
                ErrorKind::Internal,
                "acknowledgement protocol mismatch",
            )),
        };
        if result.is_err() {
            completions.remove(&key);
            self.shared.operations.cancel(operation_id);
        }
        drop(completions);
        result?;
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(cell),
        })
    }

    fn try_acknowledge(&self, token: AckToken) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        let reservation = self.reserve_ack(token)?;
        let admission = self.try_enqueue_ack(reservation.ack())?;
        reservation.commit();
        Ok(admission)
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
        if !matches!(
            *self
                .shared
                .shutdown
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "shutdown state mutex poisoned"))?,
            ShutdownRecord::Running
        ) {
            return Err(
                Error::new(ErrorKind::Shutdown, "client is already closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted),
            );
        }
        let admission = self.shared.shutdown_admission()?;
        if let Err(error) = self.shared.transition_to_closing() {
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        let result = match &self.shared.client {
            ProtocolClient::V311(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(adapter_v4::map_client_error),
            ProtocolClient::V5(client) => timeout
                .map_or_else(
                    || client.try_disconnect(),
                    |timeout| client.try_disconnect_with_timeout(timeout),
                )
                .map_err(adapter_v5::map_client_error),
        };
        if let Err(error) = result {
            self.shared.restore_running();
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        *self
            .shared
            .shutdown
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown state mutex poisoned"))? =
            ShutdownRecord::Graceful {
                operation_id: admission.operation_id,
                cell: admission.completion.cell(),
                timeout,
            };
        self.shared.shutdown_phase.store(1, Ordering::Release);
        Ok(admission)
    }

    fn try_close_now(&self) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        let previous = self
            .shared
            .shutdown
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown state mutex poisoned"))?
            .clone();
        let newly_closing = matches!(previous, ShutdownRecord::Running);
        if !matches!(
            previous,
            ShutdownRecord::Running | ShutdownRecord::Graceful { .. }
        ) {
            return Err(
                Error::new(ErrorKind::Shutdown, "client is already closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted),
            );
        }
        let admission = self.shared.shutdown_admission()?;
        if newly_closing && let Err(error) = self.shared.transition_to_closing() {
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        let result = match &self.shared.client {
            ProtocolClient::V311(client) => client
                .try_disconnect_now()
                .map_err(adapter_v4::map_client_error),
            ProtocolClient::V5(client) => client
                .try_disconnect_now()
                .map_err(adapter_v5::map_client_error),
        };
        if let Err(error) = result {
            if newly_closing {
                self.shared.restore_running();
            }
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        if let ShutdownRecord::Graceful { operation_id, .. } = &previous {
            self.shared.operations.complete(
                *operation_id,
                Err(Error::new(
                    ErrorKind::Shutdown,
                    "the requested graceful shutdown was escalated to immediate shutdown",
                )
                .with_delivery(DeliveryStatus::Ambiguous)),
            );
        }
        *self
            .shared
            .shutdown
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown state mutex poisoned"))? =
            ShutdownRecord::Immediate {
                operation_id: Some(admission.operation_id),
                cell: Some(admission.completion.cell()),
                escalated: !newly_closing,
            };
        self.shared.shutdown_phase.store(2, Ordering::Release);
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
        let cell = CompletionCell::new(operation_id);
        self.shared.operations.insert(Arc::clone(&cell));
        self.shared
            .diagnostics_tx
            .try_send(DiagnosticsRequest {
                operation_id,
                registry: self.shared.operations.clone(),
            })
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
            })
            .inspect_err(|error| {
                self.shared
                    .operations
                    .complete(operation_id, Err(error.clone()));
            })?;
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(cell),
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

enum NativeCloseState {
    Open,
    Graceful(CompletionHandle),
    GracefullyClosed,
    Immediate,
}

/// Cloneable, host-neutral ownership for idempotent close and bounded driver joining.
#[derive(Clone)]
pub struct NativeClientCloser {
    handle: ClientHandle,
    thread: Arc<ThreadOwner>,
    state: Arc<Mutex<NativeCloseState>>,
}

impl std::fmt::Debug for NativeClientCloser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeClientCloser")
            .field("state", &self.handle.state())
            .finish_non_exhaustive()
    }
}

impl NativeClientCloser {
    pub fn close(&self, timeout: Duration) -> Result<Completion> {
        let started = Instant::now();
        let completion = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "native close mutex poisoned"))?;
            match &*state {
                NativeCloseState::Open => {
                    let admission = self.handle.try_admit(Command::GracefulDisconnect {
                        timeout: Some(timeout.saturating_sub(started.elapsed())),
                    })?;
                    let completion = admission.completion;
                    *state = NativeCloseState::Graceful(completion.clone());
                    completion
                }
                NativeCloseState::Graceful(completion) => completion.clone(),
                NativeCloseState::GracefullyClosed => {
                    return Ok(Completion::GracefulShutdown);
                }
                NativeCloseState::Immediate => {
                    return Err(Error::new(
                        ErrorKind::Shutdown,
                        "client was already closed immediately",
                    ));
                }
            }
        };

        let completion = completion.wait_timeout(timeout.saturating_sub(started.elapsed()))?;
        self.thread
            .join(timeout.saturating_sub(started.elapsed()))?;
        if completion == Completion::GracefulShutdown {
            let mut state = self
                .state
                .lock()
                .map_err(|_| Error::new(ErrorKind::Internal, "native close mutex poisoned"))?;
            if matches!(*state, NativeCloseState::Graceful(_)) {
                *state = NativeCloseState::GracefullyClosed;
            }
        }
        Ok(completion)
    }

    pub fn close_now(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "native close mutex poisoned"))?;
        match &*state {
            NativeCloseState::GracefullyClosed => {}
            NativeCloseState::Graceful(completion)
                if matches!(
                    completion.try_wait(),
                    Ok(Some(Completion::GracefulShutdown))
                ) =>
            {
                *state = NativeCloseState::GracefullyClosed;
            }
            NativeCloseState::Immediate => {}
            NativeCloseState::Open | NativeCloseState::Graceful(_) => {
                self.handle.close_now_idempotent();
                *state = NativeCloseState::Immediate;
            }
        }
        drop(state);
        self.thread.join(timeout.saturating_sub(started.elapsed()))
    }
}

/// Dedicated native client and its joinable driver-thread ownership.
pub struct NativeClient {
    handle: Option<ClientHandle>,
    events: Option<EventConsumer>,
    closer: NativeClientCloser,
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
            next_ack_serial: AtomicU64::new(1),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_phase: AtomicU8::new(0),
            shutdown: Mutex::new(ShutdownRecord::Running),
            handle_count: AtomicUsize::new(1),
            admission_gate: AdmissionGate::default(),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            operations: OperationRegistry::default(),
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
                let unresolved = match &terminal {
                    TerminalStatus::Closed { graceful } => Error::new(
                        ErrorKind::Shutdown,
                        if *graceful {
                            "driver closed before the operation reported a terminal MQTT result"
                        } else {
                            "driver closed immediately before the operation completed"
                        },
                    )
                    .with_delivery(DeliveryStatus::Ambiguous),
                    TerminalStatus::Failed(error) => {
                        error.clone().with_delivery(DeliveryStatus::Ambiguous)
                    }
                };
                driver_shared.operations.fail_all(unresolved);
                publish_terminal_lifecycle(&driver_shared, &terminal);
                _ = terminal_tx.send(terminal);
                _ = done_tx.send(());
            })
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;

        let handle = ClientHandle { shared };
        let thread = Arc::new(ThreadOwner {
            join: Mutex::new(Some(join)),
            done: done_rx,
        });
        let closer = NativeClientCloser {
            handle: handle.clone(),
            thread,
            state: Arc::new(Mutex::new(NativeCloseState::Open)),
        };
        Ok(Self {
            handle: Some(handle),
            events: Some(EventConsumer {
                events: event_rx,
                terminal: terminal_rx,
                terminal_seen: false,
            }),
            closer,
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

    #[must_use]
    pub fn closer(&self) -> NativeClientCloser {
        self.closer.clone()
    }

    /// Waits for the driver to terminate and joins its thread only after termination is observed.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout expires, the join state is poisoned, or the driver thread
    /// panics.
    pub fn join(&self, timeout: Duration) -> Result<()> {
        self.closer.thread.join(timeout)
    }
}

fn publish_terminal_lifecycle(shared: &Shared, terminal: &TerminalStatus) {
    let (lifecycle, shutdown) = match terminal {
        TerminalStatus::Closed { .. } => (LifecycleState::Closed, ShutdownRecord::Closed),
        TerminalStatus::Failed(_) => (LifecycleState::Failed, ShutdownRecord::Failed),
    };
    *shared
        .shutdown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = shutdown;
    shared.lifecycle.store(lifecycle as u8, Ordering::Release);
    // Capacity waiters arm this notification before checking the request channel. Publishing the
    // terminal state before waking them therefore cannot lose the transition: every waiter either
    // observes the terminal lifecycle immediately or is registered for this notification.
    shared.request_progress.notify_waiters();
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        // The native owner, rather than any cloneable command handle, owns the driver thread.
        // Finalization must therefore remain nonblocking while still interrupting an unbounded
        // graceful close. `NativeClientCloser` keeps the join handle available to hosts that need
        // a bounded join after this cleanup signal.
        self.closer.handle.close_now_idempotent();
        if let Some(handle) = self.handle.take() {
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
                .publish_admission_policy(
                    rumqttc_v5::PublishAdmissionPolicy::RequireNegotiatedCapabilities,
                )
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
    let client_auth = config
        .client_certificate
        .as_ref()
        .zip(config.private_key.as_ref())
        .map(|(certificate, key)| (certificate.to_vec(), key.to_vec()));
    let result = if let Some(ca) = &config.ca {
        rumqttc_v4::TlsConfiguration::try_rustls_with_pem_roots(ca.to_vec(), client_auth)
    } else {
        rumqttc_v4::TlsConfiguration::try_rustls_with_native_roots(client_auth)
    };
    result.map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))
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
                // Fair selection arbitrates among ready branches. Yield after synchronously
                // handled wrapper work as well so a continuously ready flume channel cannot keep
                // this current-thread runtime from driving the MQTT socket I/O reactor.
                tokio::select! {
                    _ = immediate_shutdown_rx.recv_async(), if !connected => break None,
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        request.registry.complete(
                            request.operation_id,
                            Ok(Completion::Diagnostics(diagnostics.clone())),
                        );
                        tokio::task::yield_now().await;
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                        tokio::task::yield_now().await;
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
                let error = adapter_v4::map_connection_error(error);
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
                // Keep parity with the fair and cooperative v4 arbitration above.
                tokio::select! {
                    _ = immediate_shutdown_rx.recv_async(), if !connected => break None,
                    registration = completion_rx.recv_async() => if let Ok(registration) = registration {
                        accept_registration(registration, &pending, &mut senders);
                        tokio::task::yield_now().await;
                    },
                    request = diagnostics_rx.recv_async() => if let Ok(request) = request {
                        request.registry.complete(
                            request.operation_id,
                            Ok(Completion::Diagnostics(diagnostics.clone())),
                        );
                        tokio::task::yield_now().await;
                    },
                    result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                        resolve_pending(result, &mut senders);
                        tokio::task::yield_now().await;
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
                let error = adapter_v5::map_connection_error(error);
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
        registry,
        future,
    } = registration;
    senders.insert(operation_id, PendingSender { registry });
    pending.push(Box::pin(async move { (operation_id, future.await) }));
}

fn resolve_pending(
    (operation_id, result): (OperationId, Result<Completion>),
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    if let Some(pending_sender) = senders.remove(&operation_id) {
        pending_sender.registry.complete(operation_id, result);
    }
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
    let committed = {
        let _admission_guard = shutdown
            .shared
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shutdown
            .shared
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    };
    while let Ok(registration) = shutdown.completion_rx.try_recv() {
        accept_registration(registration, pending, senders);
    }

    let (graceful, shutdown_completion) = match committed {
        ShutdownRecord::Graceful {
            operation_id,
            cell,
            timeout,
        } => {
            let _ = (cell, timeout);
            (true, Some((operation_id, Completion::GracefulShutdown)))
        }
        ShutdownRecord::Immediate {
            operation_id,
            cell,
            escalated,
        } => {
            let _ = (cell, escalated);
            (
                false,
                operation_id.map(|operation_id| (operation_id, Completion::ImmediateShutdown)),
            )
        }
        ShutdownRecord::Running | ShutdownRecord::Closed | ShutdownRecord::Failed => (false, None),
    };
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
    fail_unfinished_operations(senders);
    if let Some((operation_id, completion)) = shutdown_completion {
        shutdown
            .shared
            .operations
            .complete(operation_id, Ok(completion));
    }
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
        request.registry.complete(
            request.operation_id,
            Ok(Completion::Diagnostics(diagnostics.clone())),
        );
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
    match &*shared
        .shutdown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        ShutdownRecord::Graceful { .. } => Some(1),
        ShutdownRecord::Immediate { .. } => Some(2),
        ShutdownRecord::Running | ShutdownRecord::Closed | ShutdownRecord::Failed => None,
    }
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

fn invalidate_connection_state(shared: &Shared, error: &Error) {
    shared.connection_generation.fetch_add(1, Ordering::AcqRel);
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
    for operation_id in completions.into_values() {
        shared.operations.complete(
            operation_id,
            Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)),
        );
    }
}

fn fail_pending(senders: &mut HashMap<OperationId, PendingSender>, error: &Error) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (operation_id, pending_sender) in pending {
        pending_sender.registry.complete(
            operation_id,
            Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)),
        );
    }
}

fn fail_unfinished_operations(senders: &mut HashMap<OperationId, PendingSender>) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (operation_id, pending_sender) in pending {
        pending_sender.registry.complete(
            operation_id,
            Err(Error::new(
                ErrorKind::Shutdown,
                "driver closed before the operation reported a terminal MQTT result",
            )
            .with_delivery(DeliveryStatus::Ambiguous)),
        );
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
            emit_outgoing.then(|| WrapperEvent::Outgoing(adapter_v4::map_outgoing(&outgoing)))
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
            shared
                .acknowledgements
                .lock()
                .expect("ack map poisoned")
                .clear();
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
            emit_outgoing.then(|| WrapperEvent::Outgoing(adapter_v5::map_outgoing(&outgoing)))
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
    let operation_id = shared
        .acknowledgement_completions
        .lock()
        .expect("ACK completion map poisoned")
        .remove(&key);
    if let Some(operation_id) = operation_id {
        shared
            .operations
            .complete(operation_id, Ok(Completion::Acknowledged));
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
    rumqttc_v5::PublishProperties {
        payload_format_indicator: properties.payload_format_indicator,
        message_expiry_interval: properties.message_expiry_interval,
        topic_alias: properties.topic_alias,
        response_topic: properties.response_topic,
        correlation_data: properties.correlation_data,
        user_properties: properties.user_properties,
        subscription_identifiers: properties.subscription_identifiers,
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

fn duration_to_u16(duration: Duration, name: &str) -> Result<u16> {
    u16::try_from(duration.as_secs())
        .map_err(|_| Error::configuration(format!("{name} exceeds u16 seconds")))
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
            next_ack_serial: AtomicU64::new(2),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_phase: AtomicU8::new(0),
            shutdown: Mutex::new(ShutdownRecord::Running),
            handle_count: AtomicUsize::new(1),
            admission_gate: AdmissionGate::default(),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            operations: OperationRegistry::default(),
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
            next_ack_serial: AtomicU64::new(1),
            next_operation: AtomicU64::new(1),
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            shutdown_phase: AtomicU8::new(0),
            shutdown: Mutex::new(ShutdownRecord::Running),
            handle_count: AtomicUsize::new(1),
            admission_gate: AdmissionGate::default(),
            acknowledgements: Mutex::new(AcknowledgementRegistry::default()),
            acknowledgement_completions: Mutex::new(HashMap::new()),
            operations: OperationRegistry::default(),
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
            shared.shutdown_phase.store(2, Ordering::Release);
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
        let operation_id = OperationId(NonZeroU64::new(99).unwrap());
        *shared.shutdown.lock().unwrap() = ShutdownRecord::Graceful {
            operation_id,
            cell: CompletionCell::new(operation_id),
            timeout: None,
        };
        shared.shutdown_phase.store(1, Ordering::Release);
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
        *shared.shutdown.lock().unwrap() = ShutdownRecord::Immediate {
            operation_id: None,
            cell: None,
            escalated: false,
        };
        shared.shutdown_phase.store(2, Ordering::Release);
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
        assert_eq!(shared.acknowledgements.lock().unwrap().len(), 1);

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
            futures_executor::block_on(completion.completion.wait_async()).unwrap(),
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
        assert!(futures_executor::block_on(completion.completion.wait_async()).is_err());
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
    fn outgoing_ack_progress_resolves_ack_completion() {
        let (shared, driver, _diagnostics_rx) = idle_v4_shared();
        let ProtocolDriver::V311(mut eventloop) = driver else {
            unreachable!();
        };
        let handle = ClientHandle {
            shared: Arc::clone(&shared),
        };
        let ack = PreparedAck::V311(rumqttc_v4::ManualAck::PubAck(rumqttc_v4::PubAck::new(7)));
        let admission = handle.try_enqueue_ack(&ack).unwrap();
        let mut completion = Box::pin(admission.completion.wait_async());
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
