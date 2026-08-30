use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use flume::{Receiver, Sender};
use futures_util::stream::FuturesUnordered;
use parking_lot::{Mutex as ParkingMutex, MutexGuard as ParkingMutexGuard};

use crate::acknowledgement::AcknowledgementCoordinator;
use crate::backend::{self, BackendDriver};
use crate::handle::{ClientHandle, NEXT_CLIENT_ID, Shared};
use crate::operations::OperationRegistry;
use crate::operations::{
    CompletionRegistration, DiagnosticsRequest, PendingFuture, PendingSender, accept_registration,
    complete_queued_diagnostics, drain_pending, fail_unfinished,
};

use crate::shutdown::{ClosedOutcome, ShutdownCoordinator};
use crate::{
    AckMode, ClientConfig, Command, Completion, CompletionHandle, ConnectionHandle, DeliveryStatus,
    DiagnosticsSnapshot, Error, ErrorCode, ErrorKind, OperationId, ProtocolVersion, Result,
    WrapperEvent,
};

struct BoundaryTerminationPanic;

fn install_boundary_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info.payload().is::<BoundaryTerminationPanic>() {
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
        let join = self
            .join
            .try_lock_for(timeout.saturating_sub(started.elapsed()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Timeout,
                    "driver join coordination did not complete before timeout",
                )
            })?
            .take();
        if let Some(join) = join {
            join.join()
                .map_err(|_| Error::new(ErrorKind::Internal, "driver thread panicked"))?;
        }
        Ok(())
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
    Immediate,
}

/// Cloneable, host-neutral ownership for idempotent close and bounded driver joining.
#[derive(Clone)]
pub struct NativeClientCloser {
    handle: ClientHandle,
    thread: Arc<ThreadOwner>,
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

    pub fn close(&self, timeout: Duration) -> Result<Completion> {
        let started = Instant::now();
        let completion = {
            let mut state = self.lock_state_until(started, timeout)?;
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
            let mut state = self.lock_state_until(started, timeout)?;
            if matches!(*state, NativeCloseState::Graceful(_)) {
                *state = NativeCloseState::GracefullyClosed;
            }
        }
        Ok(completion)
    }

    pub fn close_now(&self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        let mut state = self.lock_state_until(started, timeout)?;
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
        install_boundary_panic_hook();
        config.validate()?;
        let protocol = config.protocol_version();
        let event_capacity = config.common.event_buffer_capacity;
        let delivery_timeout = config.common.event_delivery_timeout;
        let request_capacity = config.common.request_channel_capacity;
        let emit_outgoing = config.common.emit_outgoing_events;
        let manual_ack = config.common.ack_mode == AckMode::Manual;

        let (operations, operation_receivers) = OperationRegistry::new(request_capacity);
        let (completion_rx, diagnostics_rx) = operation_receivers.into_parts();
        let (event_tx, event_rx) = flume::bounded(event_capacity);
        let (terminal_tx, terminal_rx) = flume::bounded(1);
        let (done_tx, done_rx) = flume::bounded(1);
        let (immediate_shutdown_tx, immediate_shutdown_rx) = flume::unbounded();
        let (panic_tx, panic_rx) = flume::unbounded();

        let client_identity = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
        let (client, driver) = backend::build(config)?;
        let acknowledgements = AcknowledgementCoordinator::new(client_identity, operations.clone());
        let shutdown = ShutdownCoordinator::new(operations.clone(), immediate_shutdown_tx);
        let connection = ConnectionHandle::new();
        let shared = Shared::new(
            client,
            acknowledgements,
            connection,
            operations,
            shutdown,
            panic_tx,
        );
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
            panic_rx,
        };
        let thread_name = format!("rumqtt-wrapper-{client_identity}");
        let join = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let terminal = match catch_unwind(AssertUnwindSafe(|| {
                    match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime.block_on(run_driver(driver, context)),
                        Err(error) => TerminalStatus::Failed(Error::sourced(
                            ErrorKind::Internal,
                            DeliveryStatus::NotApplicable,
                            error,
                        )),
                    }
                })) {
                    Ok(terminal) => terminal,
                    Err(_) => TerminalStatus::Failed(
                        Error::new(ErrorKind::Internal, "driver thread panicked")
                            .with_code(ErrorCode::InternalPanic),
                    ),
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
                if matches!(terminal, TerminalStatus::Failed(_)) {
                    driver_shared.reconcile_failed(unresolved.clone());
                }
                driver_shared.terminate_connection_observation(match &terminal {
                    TerminalStatus::Closed { .. } => Error::new(
                        ErrorKind::Shutdown,
                        "client closed before the first successful connection",
                    ),
                    TerminalStatus::Failed(error) => error.clone(),
                });
                driver_shared.fail_all_operations(unresolved);
                _ = terminal_tx.send(terminal);
                _ = done_tx.send(());
            })
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;

        let handle = ClientHandle::new(shared);
        let thread = Arc::new(ThreadOwner {
            join: parking_lot::Mutex::new(Some(join)),
            done: done_rx,
        });
        let closer = NativeClientCloser {
            handle: handle.clone(),
            thread,
            state: Arc::new(ParkingMutex::new(NativeCloseState::Open)),
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

    #[must_use]
    pub fn connection(&self) -> ConnectionHandle {
        self.handle().connection()
    }

    /// Waits for the driver to terminate and joins its thread only after termination is observed.
    ///
    /// # Errors
    ///
    /// Returns an error when driver termination or concurrent join coordination exceeds the shared
    /// timeout budget, or when the driver thread panics.
    pub fn join(&self, timeout: Duration) -> Result<()> {
        self.closer.thread.join(timeout)
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

async fn run_driver(driver: BackendDriver, context: DriverContext) -> TerminalStatus {
    driver.run(context).await
}

pub struct EventDelivery<'a> {
    pub(crate) shared: &'a Shared,
    pub(crate) events: &'a Sender<WrapperEvent>,
    pub(crate) timeout: Duration,
    pub(crate) immediate_shutdown: &'a Receiver<()>,
    pub(crate) panic: &'a Receiver<()>,
}

// The two explicit loops keep protocol types statically checked and make all translation local.
pub async fn deliver(delivery: &EventDelivery<'_>, event: WrapperEvent) -> bool {
    if delivery.shared.immediate_shutdown_requested() {
        return true;
    }
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
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> bool {
    while let Ok(registration) = shutdown.completion_rx.try_recv() {
        accept_registration(registration, pending, senders);
    }

    shutdown.shared.fail_acknowledgements(&Error::new(
        ErrorKind::Shutdown,
        "driver closed before acknowledgement transmission was observed",
    ));
    if shutdown.shared.should_drain_admitted_work() {
        complete_queued_diagnostics(shutdown.diagnostics_rx, diagnostics);
        drain_pending(pending, senders).await;
    }
    fail_unfinished(senders);
    shutdown.shared.reconcile_closed() == ClosedOutcome::Graceful
}

pub async fn finish_close(
    shutdown: &ShutdownInputs<'_>,
    diagnostics: &DiagnosticsSnapshot,
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) -> TerminalStatus {
    let graceful = complete_shutdown(shutdown, diagnostics, pending, senders).await;
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
