use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use flume::Sender;

use crate::acknowledgement::{AckReservation, AcknowledgementCoordinator};
use crate::backend::{AckKey, BackendClient, PreparedAck};
use crate::operations::OperationRegistry;
use crate::shutdown::{ClosedOutcome, ImmediateAdmission, PollErrorAction, ShutdownCoordinator};
use crate::validation::{protocol_option_error, validate_mqtt_utf8_string};
use crate::{
    AckToken, Admission, Command, ConnectionHandle, ConnectionResult, DeliveryStatus, Error,
    ErrorKind, LifecycleState, ProtocolVersion, PublishCommand, Result, SubscribeCommand,
    UnsubscribeCommand,
};

/// Serializes admission with connection invalidation and shutdown commitment.
#[derive(Default)]
struct AdmissionGate(Mutex<()>);

impl AdmissionGate {
    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, ()>> {
        self.0.lock()
    }
}

pub static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Shared {
    error_context: Mutex<crate::ErrorContext>,
    backend: BackendClient,
    handle_count: AtomicUsize,
    admission_gate: AdmissionGate,
    acknowledgements: Arc<AcknowledgementCoordinator>,
    connection: ConnectionHandle,
    operations: OperationRegistry,
    shutdown: Arc<ShutdownCoordinator>,
    panic_tx: Sender<()>,
    session_expiry_zero: std::sync::atomic::AtomicBool,
    reauthentication_enabled: std::sync::atomic::AtomicBool,
}

impl Shared {
    pub(crate) fn new(
        backend: BackendClient,
        acknowledgements: Arc<AcknowledgementCoordinator>,
        connection: ConnectionHandle,
        operations: OperationRegistry,
        shutdown: Arc<ShutdownCoordinator>,
        panic_tx: Sender<()>,
    ) -> Arc<Self> {
        let protocol = match backend {
            BackendClient::V4(_) => ProtocolVersion::V4,
            BackendClient::V5(_) => ProtocolVersion::V5,
        };
        Arc::new(Self {
            error_context: Mutex::new(crate::ErrorContext {
                protocol: Some(protocol),
                phase: Some(crate::ConnectionPhase::Attempt),
                ..Default::default()
            }),
            backend,
            handle_count: AtomicUsize::new(1),
            admission_gate: AdmissionGate::default(),
            acknowledgements,
            connection,
            operations,
            shutdown,
            panic_tx,
            session_expiry_zero: std::sync::atomic::AtomicBool::new(true),
            reauthentication_enabled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn retain_handle(&self) {
        self.handle_count.fetch_add(1, Ordering::Relaxed);
    }

    fn release_handle(&self) -> bool {
        self.handle_count.fetch_sub(1, Ordering::AcqRel) == 1
    }

    pub(crate) fn immediate_shutdown_requested(&self) -> bool {
        self.shutdown.immediate_requested()
    }

    pub(crate) fn set_protocol_admission_state(
        &self,
        session_expiry_zero: bool,
        reauthentication_enabled: bool,
    ) {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.session_expiry_zero
            .store(session_expiry_zero, Ordering::Release);
        self.reauthentication_enabled
            .store(reauthentication_enabled, Ordering::Release);
    }

    fn validate_disconnect(&self, payload: &crate::DisconnectProtocolOptions) -> Result<()> {
        self.shutdown.check_payload(payload)?;
        if let crate::DisconnectProtocolOptions::V5(properties) = payload
            && self.session_expiry_zero.load(Ordering::Acquire)
            && properties
                .session_expiry_interval
                .is_some_and(|expiry| expiry > 0)
        {
            return Err(protocol_option_error(
                "DISCONNECT cannot increase a zero CONNECT session expiry",
            ));
        }
        Ok(())
    }

    pub(crate) fn timeout_graceful_shutdown(&self, error: Error) -> bool {
        self.shutdown.timeout_graceful(error)
    }

    pub(crate) fn notify_progress(&self) {
        self.shutdown.notify_progress();
    }

    pub(crate) fn begin_connection(
        &self,
        protocol: ProtocolVersion,
        session_present: bool,
        discard_pending_acknowledgements: impl FnOnce(),
    ) {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        discard_pending_acknowledgements();
        {
            let mut context = self
                .error_context
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            context.phase = Some(crate::ConnectionPhase::Established);
            context.generation = Some(context.generation.unwrap_or(0).saturating_add(1));
        }
        self.acknowledgements.begin_connection();
        self.connection.connected(ConnectionResult {
            protocol,
            session_present,
        });
        self.shutdown.notify_progress();
    }

    pub(crate) fn terminate_connection_observation(&self, error: Error) {
        self.connection.terminate(error);
    }

    pub(crate) const fn backend(&self) -> &BackendClient {
        &self.backend
    }

    pub(crate) fn prepare_ack(&self, ack: PreparedAck) -> Option<AckToken> {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.acknowledgements.insert(ack)
    }

    pub(crate) fn complete_v4_puback(&self, packet_id: u16) {
        self.acknowledgements.complete(AckKey::V4PubAck(packet_id));
    }

    pub(crate) fn complete_v4_pubrec(&self, packet_id: u16) {
        self.acknowledgements.complete(AckKey::V4PubRec(packet_id));
    }

    pub(crate) fn complete_v5_puback(&self, packet_id: u16) {
        self.acknowledgements.complete(AckKey::V5PubAck(packet_id));
    }

    pub(crate) fn complete_v5_pubrec(&self, packet_id: u16) {
        self.acknowledgements.complete(AckKey::V5PubRec(packet_id));
    }

    pub(crate) fn invalidate_connection(&self, error: &Error) {
        self.error_context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase = Some(crate::ConnectionPhase::Attempt);
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.acknowledgements.invalidate(error);
    }

    pub(crate) fn fail_acknowledgements(&self, error: &Error) {
        self.acknowledgements.invalidate(error);
    }

    pub(crate) fn fail_all_operations(&self, error: Error) {
        self.operations.fail_all(error);
    }

    pub(crate) fn poll_error_action(&self) -> PollErrorAction {
        // Shutdown admission holds this gate from the lifecycle transition through request and
        // completion registration. Waiting here prevents the driver from observing a transient
        // `Closing` state whose admission may still restore `Running`.
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.shutdown.poll_error_action()
    }

    pub(crate) fn should_drain_admitted_work(&self) -> bool {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.shutdown.should_drain_admitted_work()
    }

    pub(crate) fn reconcile_closed(&self) -> ClosedOutcome {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.shutdown.reconcile_closed()
    }

    pub(crate) fn reconcile_failed(&self, error: Error) {
        let _admission_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.shutdown.reconcile_failed(error);
    }

    fn state(&self) -> LifecycleState {
        self.shutdown.state()
    }

    fn require_running(&self) -> Result<()> {
        self.shutdown.require_running()
    }

    fn admission(&self, future: crate::operations::CompletionFuture) -> Result<Admission> {
        let context = *self
            .error_context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.operations.register(Box::pin(async move {
            future.await.map_err(|error| error.with_context(context))
        }))
    }

    pub(crate) fn contextualize(&self, error: Error) -> Error {
        error.with_context(
            *self
                .error_context
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn shutdown_admission(&self) -> Result<Admission> {
        self.operations.allocate()
    }

    fn transition_to_closing(&self) -> Result<()> {
        self.shutdown.transition_to_closing()
    }

    fn restore_running(&self) {
        self.shutdown.restore_running();
    }

    fn best_effort_immediate_close(&self) {
        let _shutdown_guard = self
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(admission) = self.shutdown.immediate_admission() else {
            return;
        };
        if admission == ImmediateAdmission::StartClosing
            && self.shutdown.transition_to_closing().is_err()
        {
            return;
        }
        self.backend
            .best_effort_disconnect_now(&self.shutdown.payload());
        self.shutdown.commit_immediate(None);
    }
}

/// Cloneable command handle containing only thread-safe client/control senders and shared status.
pub struct ClientHandle {
    shared: Arc<Shared>,
}

impl Clone for ClientHandle {
    fn clone(&self) -> Self {
        self.shared.retain_handle();
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
        if self.shared.release_handle() {
            self.shared.best_effort_immediate_close();
        }
    }
}

impl ClientHandle {
    pub(crate) fn check_disconnect_payload(
        &self,
        payload: &crate::DisconnectProtocolOptions,
    ) -> Result<()> {
        self.shared.validate_disconnect(payload)
    }
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    #[must_use]
    pub fn state(&self) -> LifecycleState {
        self.shared.state()
    }

    #[must_use]
    pub fn connection(&self) -> ConnectionHandle {
        self.shared.connection.clone()
    }

    /// Idempotently requests immediate shutdown, including escalation from an
    /// in-progress graceful shutdown.
    ///
    /// This control path is intended for native-wrapper cleanup and finalizers.
    /// It makes no delivery claim for unfinished work and does not wait for the
    /// driver thread to terminate; the owning [`crate::NativeClient`] can subsequently
    /// use [`crate::NativeClient::join`] for bounded cleanup.
    pub fn close_now_idempotent(&self) {
        self.shared.best_effort_immediate_close();
    }

    /// Terminates the driver through its panic-containment boundary after a host-boundary panic.
    pub fn terminate_for_internal_panic(&self) {
        _ = self.shared.panic_tx.send(());
    }

    /// Nonblocking admission into the underlying bounded MQTT request channel.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, the request channel is full or closed, or the
    /// client is shutting down.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(name = "mqtt.wrapper.try_admit", skip_all)
    )]
    pub fn try_admit(&self, command: Command) -> Result<Admission> {
        match command {
            Command::Publish(command) => self.try_publish(command),
            Command::Subscribe(command) => self.try_subscribe(command),
            Command::Unsubscribe(filters) => self.try_unsubscribe(filters),
            Command::Acknowledge(token) => self.try_acknowledge(token),
            Command::Reauthenticate(properties) => self.try_reauthenticate(properties),
            Command::GracefulDisconnect { timeout } => {
                self.try_close(timeout, crate::DisconnectProtocolOptions::VersionNeutral)
            }
            Command::ImmediateDisconnect => {
                self.try_close_now(crate::DisconnectProtocolOptions::VersionNeutral)
            }
            Command::GracefulDisconnectWithOptions { timeout, protocol } => {
                self.try_close(timeout, protocol)
            }
            Command::ImmediateDisconnectWithOptions { protocol } => self.try_close_now(protocol),
            Command::Diagnostics => self.try_diagnostics(),
        }
        .map_err(|error| self.shared.contextualize(error))
    }

    /// Waits asynchronously for bounded request-channel capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, the request channel closes, or the client is
    /// shutting down.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(name = "mqtt.wrapper.admit", skip_all)
    )]
    pub async fn admit_async(&self, command: Command) -> Result<Admission> {
        match command {
            Command::Publish(command) => self.publish(command).await,
            Command::Subscribe(command) => self.subscribe(command).await,
            Command::Unsubscribe(filters) => self.unsubscribe(filters).await,
            Command::Acknowledge(token) => self.acknowledge(token).await,
            Command::Reauthenticate(properties) => {
                self.retry_on_backpressure(|| self.try_reauthenticate(properties.clone()))
                    .await
            }
            // Shutdown and diagnostics use priority/control paths and never wait for the publish queue.
            other => self.try_admit(other),
        }
        .map_err(|error| self.shared.contextualize(error))
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
            let progress = self.shared.shutdown.notified();
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
        let completion = self.shared.backend.try_publish(command)?;
        self.shared.admission(completion)
    }

    fn try_reauthenticate(&self, properties: Option<crate::AuthProperties>) -> Result<Admission> {
        let _guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        if matches!(self.shared.backend, BackendClient::V4(_)) {
            return Err(protocol_option_error("reauthentication requires MQTT 5"));
        }
        if !self.shared.reauthentication_enabled.load(Ordering::Acquire) {
            return Err(
                Error::auth(crate::AuthFailure::Method).with_delivery(DeliveryStatus::NotAdmitted)
            );
        }
        let completion = self.shared.backend.try_reauthenticate(properties)?;
        self.shared.admission(completion)
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
        let completion = self.shared.backend.try_subscribe(command)?;
        self.shared.admission(completion)
    }

    async fn subscribe(&self, command: SubscribeCommand) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_subscribe(command.clone()))
            .await
    }

    fn try_unsubscribe(&self, command: UnsubscribeCommand) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        if command.filters.is_empty() {
            return Err(protocol_option_error(
                "unsubscribe requires at least one filter",
            ));
        }
        let completion = self.shared.backend.try_unsubscribe(command)?;
        self.shared.admission(completion)
    }

    async fn unsubscribe(&self, command: UnsubscribeCommand) -> Result<Admission> {
        self.retry_on_backpressure(|| self.try_unsubscribe(command.clone()))
            .await
    }

    fn reserve_ack(&self, token: AckToken) -> Result<AckReservation> {
        self.shared.require_running()?;
        self.shared.acknowledgements.reserve(token)
    }

    fn try_enqueue_ack(&self, ack: &PreparedAck) -> Result<Admission> {
        let key = ack.key();
        let admission = self.shared.acknowledgements.track(key)?;
        let operation_id = admission.operation_id;
        let result = self.shared.backend.try_manual_ack(ack);
        if result.is_err() {
            self.shared
                .acknowledgements
                .rollback_tracking(key, operation_id);
        }
        result?;
        Ok(admission)
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

    fn try_close(
        &self,
        timeout: Option<Duration>,
        protocol: crate::DisconnectProtocolOptions,
    ) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        self.shared.validate_disconnect(&protocol)?;
        if !self.shared.shutdown.graceful_admission_allowed() {
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
        let result = self.shared.backend.try_disconnect(timeout, &protocol);
        if let Err(error) = result {
            self.shared.restore_running();
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        self.shared.shutdown.commit_payload(protocol);
        self.shared.shutdown.commit_graceful(&admission);
        Ok(admission)
    }

    fn try_close_now(&self, protocol: crate::DisconnectProtocolOptions) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        self.shared.validate_disconnect(&protocol)?;
        let Some(immediate_admission) = self.shared.shutdown.immediate_admission() else {
            return Err(
                Error::new(ErrorKind::Shutdown, "client is already closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted),
            );
        };
        let newly_closing = immediate_admission == ImmediateAdmission::StartClosing;
        let admission = self.shared.shutdown_admission()?;
        if newly_closing && let Err(error) = self.shared.transition_to_closing() {
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        let result = self.shared.backend.try_disconnect_now(&protocol);
        if let Err(error) = result {
            if newly_closing {
                self.shared.restore_running();
            }
            self.shared.operations.cancel(admission.operation_id);
            return Err(error);
        }
        self.shared.shutdown.commit_payload(protocol);
        self.shared.shutdown.commit_immediate(Some(&admission));
        Ok(admission)
    }

    fn try_diagnostics(&self) -> Result<Admission> {
        let _admission_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "admission mutex poisoned"))?;
        self.shared.require_running()?;
        self.shared.operations.register_diagnostics()
    }
}

pub fn duration_to_u16(duration: Duration, name: &str) -> Result<u16> {
    u16::try_from(duration.as_secs())
        .map_err(|_| Error::configuration(format!("{name} exceeds u16 seconds")))
}
