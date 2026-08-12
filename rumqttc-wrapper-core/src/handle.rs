use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::acknowledgement::{AckReservation, AcknowledgementCoordinator, PreparedAck};
use crate::adapter::{v4 as adapter_v4, v5 as adapter_v5};
use crate::operations::OperationRegistry;
use crate::shutdown::{ShutdownCoordinator, ShutdownIntent};
use crate::{
    AckToken, Admission, Command, DeliveryStatus, Error, ErrorKind, LifecycleState, PublishCommand,
    Result, SubscribeCommand,
};

/// Serializes admission with connection invalidation and shutdown commitment.
#[derive(Default)]
pub(crate) struct AdmissionGate(Mutex<()>);

impl AdmissionGate {
    pub(crate) fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, ()>> {
        self.0.lock()
    }
}

pub(crate) static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) enum ProtocolClient {
    V311(rumqttc_v4::AsyncClient),
    V5(rumqttc_v5::AsyncClient),
}

pub(crate) struct Shared {
    pub(crate) client: ProtocolClient,
    pub(crate) handle_count: AtomicUsize,
    pub(crate) admission_gate: AdmissionGate,
    pub(crate) acknowledgements: Arc<AcknowledgementCoordinator>,
    pub(crate) operations: OperationRegistry,
    pub(crate) shutdown: Arc<ShutdownCoordinator>,
}

impl Shared {
    pub(crate) fn immediate_shutdown_requested(&self) -> bool {
        self.shutdown.immediate_requested()
    }

    fn state(&self) -> LifecycleState {
        self.shutdown.state()
    }

    fn require_running(&self) -> Result<()> {
        self.shutdown.require_running()
    }

    fn admission(&self, future: crate::operations::CompletionFuture) -> Result<Admission> {
        self.operations.register(future)
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
        let previous = self.shutdown.intent();
        if matches!(
            previous,
            ShutdownIntent::Immediate | ShutdownIntent::Terminal
        ) {
            return;
        }
        match self.state() {
            LifecycleState::Running => self.shutdown.transition_to_closing().is_ok(),
            LifecycleState::Closing => false,
            LifecycleState::Closed | LifecycleState::Failed => return,
        };
        match &self.client {
            ProtocolClient::V311(client) => _ = client.try_disconnect_now(),
            ProtocolClient::V5(client) => _ = client.try_disconnect_now(),
        }
        self.shutdown.commit_immediate(None);
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
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

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
        match &self.shared.client {
            ProtocolClient::V311(client) => {
                if command.v5_properties.is_some() {
                    return Err(protocol_option_error(
                        "MQTT 5 publish properties require MQTT 5",
                    ));
                }
                let options = adapter_v4::publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(adapter_v4::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    adapter_v4::map_publish_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let options = adapter_v5::publish_options(&command);
                let notice = client
                    .try_publish_tracked(command.topic, command.payload, options)
                    .map_err(adapter_v5::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    adapter_v5::map_publish_notice(notice.wait_async().await)
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
                        rumqttc_v4::SubscribeFilterInput::new(
                            filter.filter,
                            adapter_v4::to_qos(filter.qos),
                        )
                    })
                    .collect::<Vec<_>>();
                let notice = client
                    .try_subscribe_many_tracked(filters)
                    .map_err(adapter_v4::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    adapter_v4::map_subscribe_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let filters = command
                    .filters
                    .into_iter()
                    .map(|filter| {
                        rumqttc_v5::SubscribeFilterInput::new(
                            filter.filter,
                            adapter_v5::to_qos(filter.qos),
                        )
                    })
                    .collect::<Vec<_>>();
                let notice = client
                    .try_subscribe_many_tracked(filters)
                    .map_err(adapter_v5::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    adapter_v5::map_subscribe_notice(notice.wait_async().await)
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
                    adapter_v4::map_unsubscribe_notice(notice.wait_async().await)
                }))
            }
            ProtocolClient::V5(client) => {
                let notice = client
                    .try_unsubscribe_many_tracked(filters)
                    .map_err(adapter_v5::map_client_error)?;
                self.shared.admission(Box::pin(async move {
                    adapter_v5::map_unsubscribe_notice(notice.wait_async().await)
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
        self.shared.acknowledgements.reserve(token)
    }

    fn try_enqueue_ack(&self, ack: &PreparedAck) -> Result<Admission> {
        let key = ack.key();
        let admission = self.shared.acknowledgements.track(key)?;
        let operation_id = admission.operation_id;
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

    fn try_close(&self, timeout: Option<Duration>) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        if self.shared.shutdown.intent() != ShutdownIntent::Running {
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
        self.shared.shutdown.commit_graceful(&admission);
        Ok(admission)
    }

    fn try_close_now(&self) -> Result<Admission> {
        let _shutdown_guard = self
            .shared
            .admission_gate
            .lock()
            .map_err(|_| Error::new(ErrorKind::Internal, "shutdown mutex poisoned"))?;
        let previous = self.shared.shutdown.intent();
        let newly_closing = previous == ShutdownIntent::Running;
        if !matches!(previous, ShutdownIntent::Running | ShutdownIntent::Graceful) {
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

pub(crate) fn validate_mqtt_utf8_string(value: &str, name: &str) -> Result<()> {
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

pub(crate) fn protocol_option_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Admission, message).with_delivery(DeliveryStatus::NotAdmitted)
}

pub(crate) fn duration_to_u16(duration: Duration, name: &str) -> Result<u16> {
    u16::try_from(duration.as_secs())
        .map_err(|_| Error::configuration(format!("{name} exceeds u16 seconds")))
}
