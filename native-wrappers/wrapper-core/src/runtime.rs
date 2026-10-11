use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use flume::{Receiver, Sender};
use futures_util::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use parking_lot::{Mutex as ParkingMutex, MutexGuard as ParkingMutexGuard};

use crate::acknowledgement::AcknowledgementCoordinator;
use crate::backend::{self, BackendDriver};
use crate::execution::{DRIVER_WORK, DriverWork, ExecutionContext, check_wait};
use crate::handle::{ClientHandle, NEXT_CLIENT_ID, Shared};
use crate::operations::OperationRegistry;
use crate::operations::{
    CompletionRegistration, DiagnosticsRequest, PendingFuture, PendingSender, accept_registration,
    complete_queued_diagnostics, drain_pending, fail_unfinished, resolve_pending,
};

use crate::shutdown::{ClosedOutcome, ShutdownCoordinator};
use crate::{
    AckMode, ClientConfig, Command, Completion, CompletionHandle, ConnectionHandle, DeliveryStatus,
    DiagnosticsSnapshot, Error, ErrorCode, ErrorKind, OperationId, ProtocolVersion, Result,
    WrapperEvent,
};

struct BoundaryTerminationPanic;

thread_local! {
    static IN_HOST_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn in_host_callback() -> bool {
    IN_HOST_CALLBACK.get()
}

pub fn with_host_callback<T>(call: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_HOST_CALLBACK.set(self.0);
        }
    }
    let _restore = Restore(IN_HOST_CALLBACK.replace(true));
    call()
}

fn install_boundary_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info.payload().is::<BoundaryTerminationPanic>() && !IN_HOST_CALLBACK.get() {
                previous(info);
            }
        }));
    });
}

pub fn terminate_driver_for_boundary_panic() -> ! {
    std::panic::panic_any(BoundaryTerminationPanic)
}

/// Join ownership shared by the native owner and close coordinator.
pub struct ThreadOwner {
    join: ParkingMutex<Option<thread::JoinHandle<()>>>,
    done: Receiver<()>,
}

impl ThreadOwner {
    fn join(&self, timeout: Duration) -> Result<()> {
        check_wait(timeout)?;
        let started = Instant::now();
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(flume::RecvTimeoutError::Disconnected) => {}
            Err(flume::RecvTimeoutError::Timeout) => {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "driver did not terminate before join timeout",
                ));
            }
        }
        let mut join = self
            .join
            .try_lock_for(timeout.saturating_sub(started.elapsed()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Timeout,
                    "driver join coordination did not complete before timeout",
                )
            })?;
        if let Some(thread) = join.take() {
            thread
                .join()
                .map_err(|_| Error::new(ErrorKind::Internal, "driver thread panicked"))?;
        }
        drop(join);
        Ok(())
    }
}

enum ExecutionOwner {
    Thread(ThreadOwner),
    Task {
        done: Receiver<()>,
        _context: ExecutionContext,
    },
}

impl ExecutionOwner {
    fn join(&self, timeout: Duration) -> Result<()> {
        check_wait(timeout)?;
        match self {
            Self::Thread(thread) => thread.join(timeout),
            Self::Task { done, .. } => match done.recv_timeout(timeout) {
                Ok(()) | Err(flume::RecvTimeoutError::Disconnected) => Ok(()),
                Err(flume::RecvTimeoutError::Timeout) => Err(Error::new(
                    ErrorKind::Timeout,
                    "driver task teardown did not complete before timeout",
                )),
            },
        }
    }
}

struct PreparedClient {
    identity: u64,
    handle: ClientHandle,
    events: EventConsumer,
    driver: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    work: Arc<DriverWork>,
}

fn driver_panic() -> TerminalStatus {
    TerminalStatus::Failed(
        Error::new(ErrorKind::Internal, "driver panicked").with_code(ErrorCode::InternalPanic),
    )
}

/// This guard also covers task cancellation before its first poll.
struct DriverTerminal {
    shared: Arc<Shared>,
    sender: Option<Sender<TerminalStatus>>,
}

impl DriverTerminal {
    fn finish(&mut self, terminal: TerminalStatus) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        let recovery_interrupted = self.shared.recovery_shutdown_committed();
        let terminal = match terminal {
            TerminalStatus::Failed(error) => {
                TerminalStatus::Failed(self.shared.contextualize(error))
            }
            other @ TerminalStatus::Closed { .. } => other,
        };
        self.shared.reconnect.terminate(match &terminal {
            TerminalStatus::Failed(error) => Some(error),
            TerminalStatus::Closed { .. } => None,
        });
        let unresolved = match &terminal {
            TerminalStatus::Closed { graceful } => Error::new(
                ErrorKind::Shutdown,
                if *graceful {
                    "driver closed before the operation reported a terminal MQTT result"
                } else {
                    "driver closed immediately before the operation completed"
                },
            ),
            TerminalStatus::Failed(error) => error.clone(),
        }
        .with_delivery(DeliveryStatus::Ambiguous);
        if matches!(terminal, TerminalStatus::Failed(_)) {
            self.shared.finalize_terminal_failure(unresolved.clone());
        }
        self.shared
            .terminate_connection_observation(match &terminal {
                TerminalStatus::Closed { .. } => Error::new(
                    ErrorKind::Shutdown,
                    "client closed before the first successful connection",
                ),
                TerminalStatus::Failed(error) => error.clone(),
            });
        self.shared.fail_recovery(
            &unresolved,
            recovery_interrupted || matches!(terminal, TerminalStatus::Closed { .. }),
        );
        self.shared.fail_all_operations(&unresolved);
        self.shared.terminate_diagnostics();
        let _ = sender.send(terminal);
    }
}

impl Drop for DriverTerminal {
    fn drop(&mut self) {
        self.finish(TerminalStatus::Failed(Error::new(
            ErrorKind::Internal,
            "driver execution cancelled before terminal reconciliation",
        )));
    }
}

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
    /// Returns an error for nonzero waits on execution workers or in host callbacks.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<WrapperEvent>> {
        check_wait(timeout)?;
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
pub enum TerminalStatus {
    Closed { graceful: bool },
    Failed(Error),
}

impl TerminalStatus {
    fn into_event(self) -> WrapperEvent {
        match self {
            Self::Closed { graceful: true } => WrapperEvent::GracefulShutdownCompleted,
            Self::Closed { graceful: false } => WrapperEvent::ImmediateShutdownCompleted,
            Self::Failed(error) => WrapperEvent::DriverTerminated(error),
        }
    }
}

enum NativeCloseState {
    Open,
    Graceful(CompletionHandle),
    GracefullyClosed,
    Immediate(Option<CompletionHandle>),
}

/// Cloneable, host-neutral ownership for idempotent close and bounded driver joining.
#[derive(Clone)]
pub struct NativeClientCloser {
    handle: ClientHandle,
    execution: Arc<ExecutionOwner>,
    state: Arc<ParkingMutex<NativeCloseState>>,
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
    fn lock_state_until(
        &self,
        started: Instant,
        timeout: Duration,
    ) -> Result<ParkingMutexGuard<'_, NativeCloseState>> {
        self.state
            .try_lock_for(timeout.saturating_sub(started.elapsed()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Timeout,
                    "native close coordination did not complete before timeout",
                )
                .with_delivery(DeliveryStatus::Ambiguous)
            })
    }

    /// Closes the client and waits for driver teardown.
    ///
    /// # Errors
    ///
    /// Returns the terminal disconnect error, or a timeout before completion and driver teardown.
    pub fn close(&self, timeout: Duration) -> Result<Completion> {
        self.close_with_options(timeout, crate::DisconnectProtocolOptions::VersionNeutral)
    }

    /// Coalesces matching close callers. The first admitted payload wins;
    /// conflicting later payloads fail even after the driver closes.
    /// Closes the client and waits for driver teardown.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid disconnect options, immediate closure, a failed disconnect, or a teardown timeout.
    pub fn close_with_options(
        &self,
        timeout: Duration,
        protocol: crate::DisconnectProtocolOptions,
    ) -> Result<Completion> {
        check_wait(timeout)?;
        if self.handle.has_ordered_close() {
            return Err(Error::new(
                ErrorKind::Shutdown,
                "ordinary close cannot replace an ordered fence",
            )
            .with_delivery(DeliveryStatus::NotAdmitted));
        }
        let started = Instant::now();
        let completion = {
            let mut state = self.lock_state_until(started, timeout)?;
            self.handle.check_disconnect_payload(&protocol)?;
            match &*state {
                NativeCloseState::Open => {
                    let admission =
                        self.handle
                            .try_admit(Command::GracefulDisconnectWithOptions {
                                timeout: Some(timeout.saturating_sub(started.elapsed())),
                                protocol,
                            })?;
                    let completion = admission.completion;
                    *state = NativeCloseState::Graceful(completion.clone());
                    drop(state);
                    completion
                }
                NativeCloseState::Graceful(completion) => completion.clone(),
                NativeCloseState::GracefullyClosed => {
                    return Ok(Completion::GracefulShutdown);
                }
                NativeCloseState::Immediate(_) => {
                    return Err(Error::new(
                        ErrorKind::Shutdown,
                        "client was already closed immediately",
                    ));
                }
            }
        };

        let completion = completion.wait_timeout(timeout.saturating_sub(started.elapsed()))?;
        self.execution
            .join(timeout.saturating_sub(started.elapsed()))?;
        if completion == Completion::GracefulShutdown {
            let mut state = self.lock_state_until(started, timeout)?;
            if matches!(*state, NativeCloseState::Graceful(_)) {
                *state = NativeCloseState::GracefullyClosed;
            }
        }
        Ok(completion)
    }

    /// Admit or observe an ordered publish fence, then join within one caller budget.
    /// The first admitted operation deadline is retained by subsequent callers.
    ///
    /// # Errors
    /// Returns admission/backpressure, native fence failure, or observer/join timeout.
    pub fn close_after_queued(&self, timeout: Duration) -> Result<Completion> {
        self.close_after_queued_with_options(
            timeout,
            crate::DisconnectProtocolOptions::VersionNeutral,
        )
    }

    /// Ordered close with owned MQTT 5 DISCONNECT contents and bounded joining.
    ///
    /// # Errors
    /// Returns an error for disabled support, conflicting policies, admission, native failure, or wait timeout.
    pub fn close_after_queued_with_options(
        &self,
        timeout: Duration,
        protocol: crate::DisconnectProtocolOptions,
    ) -> Result<Completion> {
        check_wait(timeout)?;
        let started = Instant::now();
        // The shared admission coordinator also observes raw fences, so it is the authority here.
        let completion = self.handle.ordered_close_observer(timeout, protocol)?;
        let outcome = completion.wait_timeout_outcome(timeout.saturating_sub(started.elapsed()));
        match outcome {
            crate::CompletionWaitOutcome::Completed(result) => {
                // A terminal operation error does not imply its cleanup/driver has finished.
                let joined = self
                    .execution
                    .join(timeout.saturating_sub(started.elapsed()));
                match result {
                    Err(error) => Err(error),
                    Ok(completion) => {
                        joined?;
                        Ok(completion)
                    }
                }
            }
            crate::CompletionWaitOutcome::ObservationRejected(error) => Err(error),
            crate::CompletionWaitOutcome::DeadlineElapsed => Err(Error::new(
                ErrorKind::Timeout,
                "ordered close observer timed out",
            )
            .with_delivery(DeliveryStatus::Ambiguous)),
        }
    }

    /// Closes the client and waits for driver teardown.
    ///
    /// # Errors
    ///
    /// Returns a shutdown error or a timeout before driver teardown.
    pub fn close_now(&self, timeout: Duration) -> Result<()> {
        self.close_now_with_options(timeout, crate::DisconnectProtocolOptions::VersionNeutral)
    }

    /// Closes the client and waits for driver teardown.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid disconnect options, failed shutdown, or a teardown timeout.
    pub fn close_now_with_options(
        &self,
        timeout: Duration,
        protocol: crate::DisconnectProtocolOptions,
    ) -> Result<()> {
        check_wait(timeout)?;
        let started = Instant::now();
        let mut state = self.lock_state_until(started, timeout)?;
        self.handle.check_disconnect_payload(&protocol)?;
        let completion = match &*state {
            NativeCloseState::Graceful(completion)
                if matches!(
                    completion.try_wait(),
                    Ok(Some(Completion::GracefulShutdown))
                ) =>
            {
                *state = NativeCloseState::GracefullyClosed;
                None
            }
            NativeCloseState::GracefullyClosed => None,
            NativeCloseState::Immediate(completion) => completion.clone(),
            NativeCloseState::Open | NativeCloseState::Graceful(_) => {
                let completion = match self
                    .handle
                    .try_admit(Command::ImmediateDisconnectWithOptions { protocol })
                {
                    Ok(admission) => Some(admission.completion),
                    Err(error)
                        if error.kind() == ErrorKind::Shutdown
                            && self.handle.state() != crate::LifecycleState::Running =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };
                *state = NativeCloseState::Immediate(completion.clone());
                completion
            }
        };
        drop(state);
        self.execution
            .join(timeout.saturating_sub(started.elapsed()))?;
        // Joining reports teardown, not the shutdown result. Preserve the
        // admitted completion for this caller and concurrent/idempotent callers.
        completion.map_or(Ok(()), |completion| {
            completion
                .wait_timeout(timeout.saturating_sub(started.elapsed()))
                .map(|_| ())
        })
    }
}

// Until the driver thread takes ownership, startup errors (including a failed
// thread spawn dropping its closure) may dispose of this runtime in an async caller.
struct StartupRuntime(Option<tokio::runtime::Runtime>);

impl StartupRuntime {
    fn into_runtime(mut self) -> tokio::runtime::Runtime {
        self.0.take().expect("startup runtime is present")
    }
}

impl Drop for StartupRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
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
    #[cfg_attr(feature = "tracing", tracing::instrument(name = "mqtt.wrapper.start", skip_all, fields(protocol = ?config.protocol_version())))]
    #[allow(
        clippy::too_many_lines,
        reason = "Build the channels, driver thread, and lifetime owners as one startup transaction"
    )]
    pub fn start(config: ClientConfig) -> Result<Self> {
        config.validate()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;
        let runtime = StartupRuntime(Some(runtime));
        let prepared = Self::prepare(config)?;
        let (done, observe) = flume::bounded(1);
        let PreparedClient {
            identity,
            handle,
            events,
            driver,
            work,
        } = prepared;
        let join = thread::Builder::new()
            .name(format!("rumqtt-wrapper-{identity}"))
            .spawn(move || {
                let runtime = runtime.into_runtime();
                runtime.block_on(async {
                    driver.await;
                    work.wait().await;
                });
                // Joining dedicated execution includes blocking work and runtime teardown.
                drop(runtime);
                drop(done);
            })
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;
        Ok(Self::from_prepared(
            handle,
            events,
            ExecutionOwner::Thread(ThreadOwner {
                join: ParkingMutex::new(Some(join)),
                done: observe,
            }),
        ))
    }

    /// Starts a client on an explicitly retained shared execution context.
    ///
    /// # Errors
    /// Returns configuration/startup errors, context backpressure, or a closing-context error.
    #[cfg_attr(feature = "tracing", tracing::instrument(name = "mqtt.wrapper.start", skip_all, fields(protocol = ?config.protocol_version())))]
    pub fn start_in(config: ClientConfig, execution: &ExecutionContext) -> Result<Self> {
        let reservation = execution.reserve()?;
        let PreparedClient {
            identity,
            handle,
            events,
            driver,
            work,
        } = Self::prepare(config)?;
        let done = reservation.start(identity, handle.clone(), work, driver)?;
        Ok(Self::from_prepared(
            handle,
            events,
            ExecutionOwner::Task {
                done,
                _context: execution.clone(),
            },
        ))
    }

    fn from_prepared(
        handle: ClientHandle,
        events: EventConsumer,
        execution: ExecutionOwner,
    ) -> Self {
        let closer = NativeClientCloser {
            handle: handle.clone(),
            execution: Arc::new(execution),
            state: Arc::new(ParkingMutex::new(NativeCloseState::Open)),
        };
        Self {
            handle: Some(handle),
            events: Some(events),
            closer,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Build client resources as one startup transaction"
    )]
    fn prepare(mut config: ClientConfig) -> Result<PreparedClient> {
        install_boundary_panic_hook();
        config.validate()?;
        let reconnect = crate::reconnect::Controller::new(config.common.reconnect.clone());
        config.common.password = config
            .common
            .password
            .as_ref()
            .map(|password| crate::SecretBytes::new(password.to_vec()).into_bytes());

        let protocol = config.protocol_version();
        let reauthentication_enabled = matches!(&config.protocol, crate::ProtocolConfig::V5(v5)
            if v5.authenticator.is_some() || v5.async_authenticator.is_some() || v5.scram.is_some());
        let event_capacity = config.common.event_buffer_capacity;
        let delivery_timeout = config.common.event_delivery_timeout;
        let request_capacity = config.common.request_channel_capacity;
        let emit_outgoing = config.common.emit_outgoing_events;
        let manual_ack = config.common.ack_mode == AckMode::Manual;
        let session_expiry_zero = match &config.protocol {
            crate::ProtocolConfig::V4(_) => true,
            crate::ProtocolConfig::V5(v5) => {
                v5.connect_properties.session_expiry_interval.unwrap_or(0) == 0
            }
        };

        let (operations, operation_receivers) = OperationRegistry::new(request_capacity);
        let (completion_rx, diagnostics_rx) = operation_receivers.into_parts();
        let (event_tx, event_rx) = flume::bounded(event_capacity);
        let (terminal_tx, terminal_rx) = flume::bounded(1);
        let (immediate_shutdown_tx, immediate_shutdown_rx) = flume::unbounded();
        let (panic_tx, panic_rx) = flume::unbounded();

        let client_identity = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
        let (client, mut driver) = backend::build(config.clone())?;
        let observation = rumqttc_core::ConnectionObservation::default();
        driver.set_connection_observation(observation.clone());
        let configuration = crate::configuration_update::ConfigurationDriver::new(
            config,
            &observation,
            driver.tls_callback_monitor(),
        );
        let acknowledgements = AcknowledgementCoordinator::new(client_identity, operations.clone());
        let shutdown = ShutdownCoordinator::new(operations.clone(), immediate_shutdown_tx);
        let connection = ConnectionHandle::new();
        let shared = Shared::new(
            (client, driver.recovery_gate()),
            acknowledgements,
            connection,
            operations,
            shutdown,
            panic_tx,
            reconnect,
        );
        shared.set_configuration(configuration.control.clone());
        shared.set_protocol_admission_state(session_expiry_zero, reauthentication_enabled);
        shared.publish_native_diagnostics(driver.capture_diagnostics());
        let context = DriverContext {
            shared: Arc::clone(&shared),
            completion_rx,
            diagnostics_rx,
            configuration: Box::new(configuration),
            events: event_tx,
            delivery_timeout,
            emit_outgoing,
            manual_ack,
            protocol,
            immediate_shutdown_rx,
            panic_rx,
        };
        let work = Arc::new(DriverWork::default());
        // Construct the guard before the future: dropping an unpolled task must reconcile too.
        let terminal = DriverTerminal {
            shared: shared.clone(),
            sender: Some(terminal_tx),
        };
        let task_work = work.clone();
        let driver = Box::pin(async move {
            let mut terminal = terminal;
            let outcome = DRIVER_WORK
                .scope(
                    task_work,
                    AssertUnwindSafe(run_driver(driver, context)).catch_unwind(),
                )
                .await;
            terminal.finish(outcome.unwrap_or_else(|_| driver_panic()));
        });
        Ok(PreparedClient {
            identity: client_identity,
            handle: ClientHandle::new(shared),
            work,
            driver,
            events: EventConsumer {
                events: event_rx,
                terminal: terminal_rx,
                terminal_seen: false,
            },
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

    #[must_use]
    pub fn connection(&self) -> ConnectionHandle {
        self.handle().connection()
    }

    /// Waits for driver and per-client auxiliary work to stop. Dedicated execution also joins
    /// its runtime thread; shared execution leaves peer clients and context workers running.
    ///
    /// # Errors
    ///
    /// Returns an error when driver termination or concurrent join coordination exceeds the shared
    /// timeout budget, or when the driver thread panics.
    pub fn join(&self, timeout: Duration) -> Result<()> {
        self.closer.execution.join(timeout)
    }
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

pub struct DriverContext {
    pub(crate) shared: Arc<Shared>,
    pub(crate) completion_rx: Receiver<CompletionRegistration>,
    pub(crate) diagnostics_rx: Receiver<DiagnosticsRequest>,
    pub(crate) configuration: Box<crate::configuration_update::ConfigurationDriver>,
    pub(crate) events: Sender<WrapperEvent>,
    pub(crate) delivery_timeout: Duration,
    pub(crate) emit_outgoing: bool,
    pub(crate) manual_ack: bool,
    pub(crate) protocol: ProtocolVersion,
    pub(crate) immediate_shutdown_rx: Receiver<()>,
    pub(crate) panic_rx: Receiver<()>,
}

pub struct ShutdownInputs<'a> {
    shared: &'a Shared,
    completion_rx: &'a Receiver<CompletionRegistration>,
    diagnostics_rx: &'a Receiver<DiagnosticsRequest>,
}

pub enum RetryReady {
    Poll,
    Recovery,
    /// Expired ordered shutdown must be polled for native terminal cleanup only.
    #[cfg(feature = "ordered-shutdown")]
    Cleanup,
    Terminal(TerminalStatus),
}

#[allow(
    clippy::too_many_arguments,
    reason = "Reuse existing driver-owned observation and shutdown resources"
)]
pub async fn wait_reconnect(
    shutdown: &ShutdownInputs<'_>,
    configuration: &mut crate::configuration_update::ConfigurationDriver,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
    immediate: &Receiver<()>,
    panic: &Receiver<()>,
) -> RetryReady {
    let shared = shutdown.shared;
    let configuration_rx = configuration.receiver.clone();
    // An in-progress cycle can yield buffered AUTH/redirect events. It remains
    // open across subsequent polls, without another budget debit or backoff.
    if shared.recovery_cleanup_pending() {
        return RetryReady::Recovery;
    }
    if shared.reconnect.snapshot().phase == crate::ReconnectPhase::Attempting {
        return RetryReady::Poll;
    }
    let snapshot = shared.reconnect.snapshot();
    let due = snapshot
        .captured_at
        .checked_add(snapshot.remaining_delay.unwrap_or_default());
    loop {
        let progress = shared.shutdown_progress();
        tokio::pin!(progress);
        progress.as_mut().enable();
        match shared.reconnect_action(snapshot.phase == crate::ReconnectPhase::Initial) {
            crate::shutdown::PollErrorAction::CompleteImmediateClose => {
                return RetryReady::Terminal(
                    finish_close(shutdown, configuration, diagnostics, pending, senders).await,
                );
            }
            crate::shutdown::PollErrorAction::Fail => {
                let error = shared.reconnect.snapshot().last_failure.unwrap_or_else(|| {
                    Error::new(
                        ErrorKind::Shutdown,
                        "client closed before connection establishment",
                    )
                });
                return RetryReady::Terminal(TerminalStatus::Failed(error));
            }
            crate::shutdown::PollErrorAction::Reconnect => {}
        }
        if shared.recovery_cleanup_pending() {
            return RetryReady::Recovery;
        }
        // Observe an expired fence before exhaustion, so native deadline/checkpoint
        // processing stays authoritative. This branch must never establish a socket.
        #[cfg(feature = "ordered-shutdown")]
        if shared.ordered_deadline_expired() {
            return RetryReady::Cleanup;
        }
        if let Err(error) = shared.reconnect.check_budget() {
            return RetryReady::Terminal(TerminalStatus::Failed(error));
        }
        if due.is_none_or(|due| due <= tokio::time::Instant::now().into_std()) {
            return match shared.reconnect.start() {
                Ok(()) => RetryReady::Poll,
                Err(error) => RetryReady::Terminal(TerminalStatus::Failed(error)),
            };
        }
        tokio::select! {
            () = shared.recovery_changed() => {},
            () = &mut progress => {},
            _ = panic.recv_async() => terminate_driver_for_boundary_panic(),
            _ = immediate.recv_async() => {},
            () = shared.wait_retry_shutdown() => {},
            () = async {
                #[cfg(feature = "ordered-shutdown")]
                shared.wait_ordered_deadline().await;
                #[cfg(not(feature = "ordered-shutdown"))]
                std::future::pending::<()>().await;
            } => {},
            update = configuration_rx.recv_async(), if !configuration.is_preparing() => if let Ok(update) = update {
                configuration.start(update);
            },
            () = configuration.complete_preparation(shared), if configuration.is_preparing() => {},
            registration = shutdown.completion_rx.recv_async() => if let Ok(registration) = registration {
                accept_registration(registration, pending, senders);
            },
            request = shutdown.diagnostics_rx.recv_async() => if let Ok(request) = request {
                let mut snapshot = diagnostics.clone();
                shared.ordered_diagnostics(&mut snapshot);
                request.resolve(snapshot);
            },
            result = pending.next(), if !pending.is_empty() => if let Some(result) = result {
                resolve_pending(result, senders);
            },
            () = tokio::time::sleep_until(due.expect("checked retry deadline").into()) => {},
        }
        tokio::task::yield_now().await;
    }
}

impl<'a> ShutdownInputs<'a> {
    pub(crate) const fn new(
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

#[cfg_attr(feature = "tracing", tracing::instrument(name = "mqtt.wrapper.driver", skip_all, fields(protocol = ?context.protocol)))]
async fn run_driver(driver: BackendDriver, context: DriverContext) -> TerminalStatus {
    let shared = Arc::clone(&context.shared);
    let tls_callbacks = driver.tls_callback_monitor();
    #[cfg(not(feature = "ordered-shutdown"))]
    let terminal = tokio::select! {
        terminal = driver.run(context) => Some(terminal),
        () = shared.wait_graceful_timeout() => None,
    };
    #[cfg(feature = "ordered-shutdown")]
    let terminal = {
        let execution = driver.run(context);
        let observation = shared.observe_ordered();
        tokio::pin!(execution, observation);
        let mut observed = false;
        let terminal = loop {
            tokio::select! {
                biased;
                () = &mut observation, if !observed => { observed = true; },
                () = shared.wait_ordered_abort() => {
                    // Explicit abort is the boundary allowed to cancel pending native cleanup.
                    break None;
                },
                terminal = &mut execution => break Some(terminal),
                () = shared.wait_graceful_timeout() => break None,
            }
        };
        // The native sender may finish in the same poll that terminates execution.
        if !observed {
            let _ = observation.as_mut().now_or_never();
        }
        terminal
    };
    // The backend future has been dropped. Its inner cancellation checks cannot
    // run, so inspect destruction failures before reconciling a cancelled close.
    let terminal = terminal.unwrap_or_else(|| {
        tls_callbacks.failure().map_or_else(
            || {
                shared.reconcile_closed();
                TerminalStatus::Closed { graceful: false }
            },
            |failure| {
                TerminalStatus::Failed(
                    Error::tls_callback(failure).with_delivery(DeliveryStatus::Ambiguous),
                )
            },
        )
    });
    #[cfg(not(feature = "ordered-shutdown"))]
    {
        terminal
    }
    #[cfg(feature = "ordered-shutdown")]
    {
        if shared.immediate_shutdown_requested() {
            return terminal;
        }
        match (&terminal, shared.ordered_result()) {
            (TerminalStatus::Failed(_), _) | (_, None) => terminal,
            (_, Some(Ok(_))) => TerminalStatus::Closed { graceful: true },
            (_, Some(Err(error))) => TerminalStatus::Failed(error),
        }
    }
}

pub struct EventDelivery<'a> {
    pub(crate) shared: &'a Shared,
    pub(crate) events: &'a Sender<WrapperEvent>,
    pub(crate) timeout: Duration,
    pub(crate) immediate_shutdown: &'a Receiver<()>,
    pub(crate) panic: &'a Receiver<()>,
    #[cfg(feature = "ordered-shutdown")]
    pub(crate) staged: std::sync::Mutex<Option<WrapperEvent>>,
}

// The two explicit loops keep protocol types statically checked and make all translation local.
pub async fn deliver(delivery: &EventDelivery<'_>, event: WrapperEvent) -> bool {
    if delivery.shared.immediate_shutdown_requested() {
        return true;
    }
    #[cfg(feature = "ordered-shutdown")]
    {
        // Watch admission even if this send was blocked before the fence existed.
        // The fast path avoids cloning events when the application keeps up.
        let event = match delivery.events.try_send(event) {
            Ok(()) => return true,
            Err(flume::TrySendError::Disconnected(_)) => return false,
            Err(flume::TrySendError::Full(event)) => event,
        };
        let send = delivery.events.send_async(event.clone());
        tokio::pin!(send);
        tokio::select! {
            biased;
            _ = delivery.panic.recv_async() => terminate_driver_for_boundary_panic(),
            _ = delivery.immediate_shutdown.recv_async() => true,
            () = delivery.shared.wait_ordered_deadline() => {
                // Retain one blocked event until teardown while native polling resumes.
                *delivery.staged.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(event);
                true
            },
            result = tokio::time::timeout(delivery.timeout, &mut send) => matches!(result, Ok(Ok(()))),
        }
    }
    #[cfg(not(feature = "ordered-shutdown"))]
    tokio::select! {
        biased;
        _ = delivery.panic.recv_async() => terminate_driver_for_boundary_panic(),
        _ = delivery.immediate_shutdown.recv_async() => true,
        result = tokio::time::timeout(delivery.timeout, delivery.events.send_async(event)) => {
            matches!(result, Ok(Ok(())))
        },
    }
}

pub async fn complete_shutdown(
    shutdown: &ShutdownInputs<'_>,
    configuration: &mut crate::configuration_update::ConfigurationDriver,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> bool {
    // Synchronize with shutdown admission before collecting its final registrations and configuration requests.
    let drain = shutdown.shared.should_drain_admitted_work();
    configuration.close();
    while let Ok(registration) = shutdown.completion_rx.try_recv() {
        accept_registration(registration, pending, senders);
    }

    shutdown.shared.fail_acknowledgements(&Error::new(
        ErrorKind::Shutdown,
        "driver closed before acknowledgement transmission was observed",
    ));
    if drain {
        let mut diagnostics = diagnostics.clone();
        shutdown.shared.ordered_diagnostics(&mut diagnostics);
        complete_queued_diagnostics(shutdown.diagnostics_rx, &diagnostics);
        drain_pending(pending, senders).await;
    }
    fail_unfinished(senders);
    shutdown.shared.reconcile_closed() == ClosedOutcome::Graceful
}

pub async fn finish_close(
    shutdown: &ShutdownInputs<'_>,
    configuration: &mut crate::configuration_update::ConfigurationDriver,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> TerminalStatus {
    let graceful = complete_shutdown(shutdown, configuration, diagnostics, pending, senders).await;
    TerminalStatus::Closed { graceful }
}

pub fn overflow_error() -> Error {
    Error::new(
        ErrorKind::Backpressure,
        "event buffer remained full beyond the delivery timeout",
    )
    .with_code(ErrorCode::EventBufferOverflow)
    .with_retryable(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpolled_driver_cancellation_retains_initial_diagnostic_capture() {
        for config in [
            ClientConfig::v4("unpolled", "localhost", 1883),
            ClientConfig::v5("unpolled", "localhost", 1883),
        ] {
            let prepared = NativeClient::prepare(config).unwrap();
            let before = prepared.handle.diagnostics_snapshot();
            let native = before.native.unwrap();
            assert_eq!(native.generation, 1);
            assert!(!before.terminated);
            let handle = prepared.handle;
            drop(prepared.driver);
            let after = handle.diagnostics_snapshot();
            assert!(after.terminated);
            assert_eq!(after.lifecycle, crate::LifecycleState::Failed);
            assert!(Arc::ptr_eq(&native, after.native.as_ref().unwrap()));
            assert!(!native.disconnect_complete);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "Keep native shutdown, queued preparation and completion-drain barriers in one regression"
    )]
    fn shutdown_resolves_configuration_requests_before_draining_completions() {
        use crate::{
            ActivationState, BrokerCredentials, FieldUpdate, RuntimeConfigUpdate, SecretBytes,
        };
        use std::io::{Read, Write};
        use std::net::TcpListener;

        for mqtt5 in [false, true] {
            for ordered in [false, true] {
                if ordered && !cfg!(feature = "ordered-shutdown") {
                    continue;
                }
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .max_blocking_threads(1)
                    .build()
                    .unwrap();
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let (disconnected, disconnected_rx) = std::sync::mpsc::channel();
                let broker = thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut header = [0; 2];
                    socket.read_exact(&mut header).unwrap();
                    assert_eq!(header[0], 0x10);
                    assert!(header[1] < 128);
                    socket
                        .read_exact(&mut vec![0; usize::from(header[1])])
                        .unwrap();
                    socket
                        .write_all(if mqtt5 {
                            &[0x20, 3, 0, 0, 0]
                        } else {
                            &[0x20, 2, 0, 0]
                        })
                        .unwrap();
                    socket.read_exact(&mut header).unwrap();
                    assert_eq!(header[0] >> 4, 14);
                    disconnected.send(()).unwrap();
                });
                let mut config = if mqtt5 {
                    ClientConfig::v5("shutdown", "127.0.0.1", port)
                } else {
                    ClientConfig::v4("shutdown", "127.0.0.1", port)
                };
                config.common.keep_alive = Duration::ZERO;
                let PreparedClient {
                    handle,
                    mut events,
                    driver,
                    work,
                    ..
                } = NativeClient::prepare(config).unwrap();
                let mut driver = runtime.spawn(driver);
                runtime.block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while !matches!(
                            events.recv_async().await.unwrap(),
                            Some(WrapperEvent::Connected { .. })
                        ) {}
                    })
                    .await
                    .unwrap();
                });
                let proposal = RuntimeConfigUpdate {
                    credentials: FieldUpdate::Replace(BrokerCredentials {
                        username: Some("user".into()),
                        password: Some(SecretBytes::new(b"next-attempt".to_vec())),
                    }),
                    ..Default::default()
                };
                let first = handle.try_configuration_update(proposal.clone()).unwrap();
                let Completion::ConfigurationStaged(receipt) =
                    runtime.block_on(first.completion.wait_async()).unwrap()
                else {
                    panic!("expected baseline staging");
                };
                assert_eq!(receipt.activation().1, ActivationState::Staged);
                let (busy, busy_rx) = std::sync::mpsc::channel();
                let (release, release_rx) = std::sync::mpsc::channel();
                let blocker = runtime.spawn_blocking(move || {
                    busy.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                });
                busy_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut updates = vec![handle.try_configuration_update(proposal.clone()).unwrap()];
                runtime.block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        // The pool blocker is untracked; only configuration preparation can reserve work here.
                        while work.wait().now_or_never().is_some() {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
                });
                for _ in 1..crate::MAX_PENDING_CONFIGURATION_UPDATES {
                    updates.push(handle.try_configuration_update(proposal.clone()).unwrap());
                }
                let close = handle
                    .try_admit(if ordered {
                        Command::OrderedDisconnect { timeout: None }
                    } else {
                        Command::GracefulDisconnect { timeout: None }
                    })
                    .unwrap();
                let finished = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(200), &mut driver).await
                });
                let completed = matches!(finished, Ok(Ok(())));
                if !completed {
                    // Cancellation allows a failing regression to release its outstanding owners safely.
                    driver.abort();
                    let _ = runtime.block_on(driver);
                }
                let tracked = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(20), work.wait())
                        .await
                        .is_err()
                });
                release.send(()).unwrap();
                runtime.block_on(async {
                    blocker.await.unwrap();
                    tokio::time::timeout(Duration::from_secs(5), work.wait())
                        .await
                        .unwrap();
                });
                disconnected_rx
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                broker.join().unwrap();
                assert!(
                    completed,
                    "shutdown waited on its own configuration senders (v5={mqtt5}, ordered={ordered})"
                );
                assert!(
                    tracked,
                    "cancelled preparation must remain part of client teardown"
                );
                assert_eq!(
                    runtime.block_on(close.completion.wait_async()).unwrap(),
                    if ordered {
                        Completion::OrderedShutdown
                    } else {
                        Completion::GracefulShutdown
                    }
                );
                assert_eq!(
                    receipt.activation().1,
                    ActivationState::ClosedBeforeActivation
                );
                for update in updates {
                    assert_eq!(
                        runtime
                            .block_on(update.completion.wait_async())
                            .unwrap_err()
                            .kind(),
                        ErrorKind::Shutdown
                    );
                }
                let snapshot = handle.configuration_snapshot().unwrap();
                assert!(snapshot.closed);
                assert_eq!(snapshot.revision, 1);
            }
        }
    }

    #[test]
    fn tuning_updates_stage_while_the_blocking_pool_is_busy() {
        use crate::{FieldUpdate, RuntimeConfigUpdate};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let blocker = runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let result = runtime.block_on(async {
            let prepared =
                NativeClient::prepare(ClientConfig::v4("tuning", "127.0.0.1", 65535)).unwrap();
            let admission = prepared
                .handle
                .try_configuration_update(RuntimeConfigUpdate {
                    read_batch_size: FieldUpdate::Replace(16),
                    ..Default::default()
                })
                .unwrap();
            let driver = tokio::spawn(prepared.driver);
            let result =
                tokio::time::timeout(Duration::from_secs(2), admission.completion.wait_async())
                    .await;
            driver.abort();
            let _ = driver.await;
            result
        });
        release.send(()).unwrap();
        runtime.block_on(blocker).unwrap();
        assert!(matches!(
            result.unwrap().unwrap(),
            Completion::ConfigurationStaged(_)
        ));
    }

    #[test]
    fn rejected_update_owners_can_reenter_admission_after_the_gate_is_released() {
        use crate::{FieldUpdate, RuntimeConfigUpdate};
        use std::sync::atomic::AtomicBool;

        struct Owner {
            handle: ClientHandle,
            dropped: Arc<AtomicBool>,
        }
        impl crate::TlsVerifier for Owner {
            fn verify(
                &self,
                _: &crate::TlsVerificationRequest<'_>,
            ) -> std::result::Result<(), crate::TlsCallbackReason> {
                unreachable!("a rejected profile must not perform network verification")
            }
        }
        impl Drop for Owner {
            fn drop(&mut self) {
                assert_eq!(
                    self.handle
                        .try_configuration_update(RuntimeConfigUpdate {
                            read_batch_size: FieldUpdate::Clear,
                            ..Default::default()
                        })
                        .unwrap_err()
                        .kind(),
                    ErrorKind::Backpressure
                );
                self.dropped.store(true, Ordering::Release);
            }
        }
        // An unpolled driver deterministically keeps all admission reservations queued.
        let prepared =
            NativeClient::prepare(ClientConfig::v4("owners", "127.0.0.1", 65535)).unwrap();
        for _ in 0..crate::MAX_PENDING_CONFIGURATION_UPDATES {
            let _admission = prepared
                .handle
                .try_configuration_update(RuntimeConfigUpdate {
                    read_batch_size: FieldUpdate::Clear,
                    ..Default::default()
                })
                .unwrap();
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let profile = crate::TlsConfig {
            verifier: Some(crate::TlsVerifierConfig(Arc::new(Owner {
                handle: prepared.handle.clone(),
                dropped: dropped.clone(),
            }))),
            ..Default::default()
        };
        assert_eq!(
            prepared
                .handle
                .try_configuration_update(RuntimeConfigUpdate {
                    broker_tls: FieldUpdate::Replace(profile),
                    ..Default::default()
                })
                .unwrap_err()
                .kind(),
            ErrorKind::Backpressure
        );
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_an_unpolled_driver_reconciles_operations_and_terminal_status() {
        let mut prepared =
            NativeClient::prepare(ClientConfig::v5("cancelled", "127.0.0.1", 65535)).unwrap();
        let pending = prepared
            .handle
            .try_admit(Command::Diagnostics)
            .unwrap()
            .completion;
        let task = tokio::spawn(prepared.driver);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(pending.try_wait().unwrap_err().kind(), ErrorKind::Internal);
        assert!(matches!(
            prepared.events.try_recv().unwrap(),
            Some(WrapperEvent::DriverTerminated(_))
        ));
        assert!(prepared.events.try_recv().unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unstarted_driver_closure_can_drop_runtime_in_async_context() {
        let runtime = StartupRuntime(Some(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        ));
        // A failed thread spawn drops the closure without calling it.
        let driver = move || drop(runtime.into_runtime());
        drop(driver);
    }

    #[test]
    fn join_coordination_honors_the_timeout_budget() {
        let (done_tx, done) = flume::bounded(1);
        let join = thread::spawn(move || drop(done_tx));
        let owner = ThreadOwner {
            join: ParkingMutex::new(Some(join)),
            done,
        };
        owner.join(Duration::from_secs(1)).unwrap();
        owner.join(Duration::ZERO).unwrap();
    }

    #[test]
    fn overflow_has_stable_non_retryable_classification() {
        let error = overflow_error();
        assert_eq!(error.code(), ErrorCode::EventBufferOverflow);
        assert!(!error.retryable());
    }
}
