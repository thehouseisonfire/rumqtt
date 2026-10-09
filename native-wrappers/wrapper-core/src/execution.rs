//! Explicit execution ownership; protocol drivers never own or destroy the runtime.
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinSet;

use crate::{ClientHandle, DeliveryStatus, Error, ErrorKind, Result};

thread_local! {
    static MANAGED_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
tokio::task_local! {
    pub(crate) static DRIVER_WORK: Arc<DriverWork>;
}

/// Whether this thread may wait for native progress. Polling remains allowed.
#[must_use]
pub fn blocking_wait_allowed() -> bool {
    !MANAGED_THREAD.get() && !crate::runtime::in_host_callback()
}

pub fn check_wait(timeout: Duration) -> Result<()> {
    if !timeout.is_zero() && !blocking_wait_allowed() {
        return Err(state_error(
            "blocking native waits are forbidden on execution workers and in callbacks",
        ));
    }
    Ok(())
}

fn state_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Shutdown, message).with_code(crate::ErrorCode::InvalidState)
}

/// Resource limits for a library-owned shared runtime. Blocking queues are not a thread limit.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionOptions {
    pub client_capacity: usize,
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self {
            client_capacity: 1024,
            worker_threads: 2,
            max_blocking_threads: 32,
        }
    }
}

/// Quiescent means runtime destruction and management-thread joining have completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ExecutionState {
    Open = 0,
    Closing = 1,
    Quiescent = 2,
}

type DriverFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

struct ActiveClient {
    handle: ClientHandle,
    done: flume::Sender<()>,
    work: Arc<DriverWork>,
}

struct Lifecycle {
    state: ExecutionState,
    reserved: usize,
    clients: HashMap<u64, ActiveClient>,
}

struct Shared {
    lifecycle: Mutex<Lifecycle>,
    capacity: usize,
    changed: Notify,
}

enum Administrative {
    Start(u64, DriverFuture),
    Shutdown,
    #[cfg(test)]
    Panic,
}

struct Owner {
    shared: Arc<Shared>,
    commands: flume::Sender<Administrative>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    stopped: flume::Receiver<()>,
    failure: Mutex<Option<Error>>,
}

impl Owner {
    fn shutdown(&self) {
        let handles = {
            let mut lifecycle = self.shared.lifecycle.lock();
            if lifecycle.state != ExecutionState::Open {
                return;
            }
            lifecycle.state = ExecutionState::Closing;
            lifecycle
                .clients
                .values()
                .map(|client| client.handle.clone())
                .collect::<Vec<_>>()
        };
        for handle in handles {
            handle.close_now_idempotent();
        }
        let _ = self.commands.send(Administrative::Shutdown);
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Cloneable explicit context ownership. Releasing a reference does not stop attached clients.
#[derive(Clone)]
pub struct ExecutionContext(Arc<Owner>);

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionContext")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl ExecutionContext {
    /// Constructs the runtime on its management thread and waits for startup.
    ///
    /// # Errors
    /// Rejects zero limits, forbidden blocking startup, or runtime/thread creation failure.
    pub fn new(options: ExecutionOptions) -> Result<Self> {
        check_wait(Duration::from_secs(1))?;
        if options.client_capacity == 0
            || options.worker_threads == 0
            || options.max_blocking_threads == 0
        {
            return Err(Error::new(
                ErrorKind::Configuration,
                "execution limits must be nonzero",
            ));
        }
        let shared = Arc::new(Shared {
            lifecycle: Mutex::new(Lifecycle {
                state: ExecutionState::Open,
                reserved: 0,
                clients: HashMap::new(),
            }),
            capacity: options.client_capacity,
            changed: Notify::new(),
        });
        let (commands, receive) = flume::unbounded();
        let (ready, startup) = flume::bounded(1);
        let (stopped, stopped_rx) = flume::bounded(1);
        let management_shared = shared.clone();
        let join = thread::Builder::new()
            .name("rumqtt-context-owner".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(options.worker_threads)
                    .max_blocking_threads(options.max_blocking_threads)
                    .thread_name("rumqtt-context-worker")
                    .on_thread_start(|| MANAGED_THREAD.set(true))
                    .on_thread_stop(|| MANAGED_THREAD.set(false))
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        if ready.send(Ok(())).is_ok() {
                            MANAGED_THREAD.set(true);
                            let outcome =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    runtime.block_on(manage(receive, management_shared.clone()));
                                }));
                            if outcome.is_err() {
                                management_shared.lifecycle.lock().state = ExecutionState::Closing;
                            }
                            // Blocking work, including cancelled DNS, must finish before successful join.
                            drop(runtime);
                            // Runtime loss drops terminal guards before releasing client teardown observers.
                            let clients =
                                std::mem::take(&mut management_shared.lifecycle.lock().clients);
                            drop(clients);
                            MANAGED_THREAD.set(false);
                            if let Err(panic) = outcome {
                                std::panic::resume_unwind(panic);
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(Error::sourced(
                            ErrorKind::Internal,
                            DeliveryStatus::NotApplicable,
                            error,
                        )));
                    }
                }
                drop(stopped);
            })
            .map_err(|error| {
                Error::sourced(ErrorKind::Internal, DeliveryStatus::NotApplicable, error)
            })?;
        match startup.recv() {
            Ok(Ok(())) => Ok(Self(Arc::new(Owner {
                shared,
                commands,
                join: Mutex::new(Some(join)),
                stopped: stopped_rx,
                failure: Mutex::new(None),
            }))),
            outcome => {
                let _ = join.join();
                Err(outcome
                    .unwrap_or_else(|_| {
                        Err(Error::new(ErrorKind::Internal, "execution startup failed"))
                    })
                    .unwrap_err())
            }
        }
    }

    /// Requests immediate cleanup. Repeated requests coalesce and never wait for drivers.
    pub fn request_shutdown(&self) {
        self.0.shutdown();
    }

    /// Observes lifecycle state; joining is required before Quiescent can be observed.
    #[must_use]
    pub fn state(&self) -> ExecutionState {
        self.0.shared.lifecycle.lock().state
    }

    /// Joins only when execution has completely stopped. Returns false while pending.
    ///
    /// # Errors
    /// Returns invalid state before shutdown, or an internal management-thread failure.
    pub fn try_join(&self) -> Result<bool> {
        if self.state() == ExecutionState::Open {
            return Err(state_error("request context shutdown before joining"));
        }
        let Some(mut join) = self.0.join.try_lock() else {
            return Ok(false);
        };
        if let Some(error) = self.0.failure.lock().as_ref() {
            return Err(error.clone());
        }
        if let Some(thread) = join.as_ref()
            && !thread.is_finished()
        {
            return Ok(false);
        }
        if let Some(thread) = join.take()
            && thread.join().is_err()
        {
            let error = Error::new(ErrorKind::Internal, "execution management thread panicked");
            *self.0.failure.lock() = Some(error.clone());
            return Err(error);
        }
        self.0.shared.lifecycle.lock().state = ExecutionState::Quiescent;
        Ok(true)
    }

    /// Waits for execution teardown. The timeout only cancels this observation.
    ///
    /// # Errors
    /// Returns invalid state on workers/callbacks or before shutdown, timeout, or management failure.
    pub fn join(&self, timeout: Duration) -> Result<()> {
        check_wait(timeout)?;
        let started = Instant::now();
        loop {
            if self.try_join()? {
                return Ok(());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "context teardown did not complete before timeout",
                ));
            }
            // The signal is disconnected after runtime destruction. is_finished covers thread epilogue.
            let _ = self.0.stopped.recv_timeout(remaining);
            thread::yield_now();
        }
    }

    pub(crate) fn reserve(&self) -> Result<Reservation> {
        let mut lifecycle = self.0.shared.lifecycle.lock();
        if lifecycle.state != ExecutionState::Open {
            return Err(state_error("execution context is closing")
                .with_delivery(DeliveryStatus::NotAdmitted));
        }
        if lifecycle.reserved + lifecycle.clients.len() >= self.0.shared.capacity {
            return Err(Error::new(
                ErrorKind::Backpressure,
                "execution context client capacity exhausted",
            )
            .with_delivery(DeliveryStatus::NotAdmitted));
        }
        lifecycle.reserved += 1;
        drop(lifecycle);
        Ok(Reservation {
            owner: self.clone(),
            reserved: true,
        })
    }
}

pub struct Reservation {
    owner: ExecutionContext,
    reserved: bool,
}

impl Reservation {
    pub(crate) fn start(
        mut self,
        id: u64,
        handle: ClientHandle,
        work: Arc<DriverWork>,
        driver: DriverFuture,
    ) -> Result<flume::Receiver<()>> {
        let (done, observe) = flume::bounded(1);
        let mut lifecycle = self.owner.0.shared.lifecycle.lock();
        if lifecycle.state != ExecutionState::Open {
            return Err(state_error("execution context closed during startup")
                .with_delivery(DeliveryStatus::NotAdmitted));
        }
        lifecycle
            .clients
            .insert(id, ActiveClient { handle, done, work });
        lifecycle.reserved -= 1;
        self.reserved = false;
        if let Err(error) = self
            .owner
            .0
            .commands
            .send(Administrative::Start(id, driver))
        {
            let client = lifecycle.clients.remove(&id);
            drop(lifecycle);
            drop(client);
            drop(error);
            return Err(Error::new(
                ErrorKind::Internal,
                "execution management stopped",
            ));
        }
        drop(lifecycle);
        Ok(observe)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.reserved {
            self.owner.0.shared.lifecycle.lock().reserved -= 1;
            self.owner.0.shared.changed.notify_one();
        }
    }
}

async fn manage(receive: flume::Receiver<Administrative>, shared: Arc<Shared>) {
    let mut tasks = JoinSet::new();
    let mut identities = HashMap::new();
    let mut cleanup = FuturesUnordered::<Pin<Box<dyn Future<Output = u64> + Send>>>::new();
    let mut closing = false;
    loop {
        if closing
            && tasks.is_empty()
            && cleanup.is_empty()
            && shared.lifecycle.lock().reserved == 0
        {
            break;
        }
        tokio::select! {
            () = shared.changed.notified() => {},
            command = receive.recv_async(), if !closing => match command {
                Ok(Administrative::Start(id, driver)) => {
                    let task = tasks.spawn(async move { driver.await; id });
                    identities.insert(task.id(), id);
                }
                Ok(Administrative::Shutdown) | Err(_) => { closing = true; },
                #[cfg(test)]
                Ok(Administrative::Panic) => panic!("injected management failure"),
            },
            outcome = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let id = match outcome.expect("nonempty task set") {
                    Ok((task, id)) => { identities.remove(&task); id },
                    Err(error) => identities.remove(&error.id()).expect("registered task"),
                };
                let work = shared.lifecycle.lock().clients[&id].work.clone();
                cleanup.push(Box::pin(async move { work.wait().await; id }));
            },
            id = cleanup.next(), if !cleanup.is_empty() => {
                let client = shared.lifecycle.lock().clients.remove(&id.expect("nonempty cleanup set"));
                if let Some(client) = client {
                    let ActiveClient { handle, done, work } = client;
                    drop(handle);
                    drop(work);
                    drop(done);
                }
            },
        }
    }
}

/// Per-client auxiliary work must finish before that client's teardown observer is released.
#[derive(Default)]
pub struct DriverWork {
    pending: AtomicUsize,
    changed: Notify,
}
impl DriverWork {
    pub(crate) async fn wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.pending.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}
pub struct BlockingWork(Arc<DriverWork>);
impl Drop for BlockingWork {
    fn drop(&mut self) {
        if self.0.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.changed.notify_waiters();
        }
    }
}
pub fn register_blocking_work() -> Option<BlockingWork> {
    DRIVER_WORK
        .try_with(|work| {
            work.pending.fetch_add(1, Ordering::AcqRel);
            BlockingWork(work.clone())
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_waits_for_startup_reservations_and_timeout_is_retryable() {
        let context = ExecutionContext::new(ExecutionOptions::default()).unwrap();
        let reservation = context.reserve().unwrap();
        context.request_shutdown();
        assert_eq!(
            context.join(Duration::from_millis(10)).unwrap_err().kind(),
            ErrorKind::Timeout
        );
        assert_eq!(context.state(), ExecutionState::Closing);
        drop(reservation);
        context.join(Duration::from_secs(5)).unwrap();
        assert_eq!(context.state(), ExecutionState::Quiescent);
    }

    #[test]
    fn final_owner_release_stops_the_management_thread() {
        let context = ExecutionContext::new(ExecutionOptions::default()).unwrap();
        let stopped = context.0.stopped.clone();
        drop(context);
        assert!(matches!(
            stopped.recv_timeout(Duration::from_secs(5)),
            Err(flume::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn callback_boundaries_reject_blocking_waits_and_allow_polling() {
        crate::runtime::with_host_callback(|| {
            assert!(!blocking_wait_allowed());
            assert!(check_wait(Duration::ZERO).is_ok());
            assert!(check_wait(Duration::from_millis(1)).is_err());
        });
        assert!(blocking_wait_allowed());
    }

    #[test]
    fn management_loss_reconciles_clients_and_never_reports_successful_quiescence() {
        let context = ExecutionContext::new(ExecutionOptions::default()).unwrap();
        let client = crate::NativeClient::start_in(
            crate::ClientConfig::v4("runtime-loss", "127.0.0.1", 65535),
            &context,
        )
        .unwrap();
        context.0.commands.send(Administrative::Panic).unwrap();
        client.join(Duration::from_secs(5)).unwrap();
        assert_eq!(
            context.join(Duration::from_secs(5)).unwrap_err().kind(),
            ErrorKind::Internal
        );
        assert_eq!(context.try_join().unwrap_err().kind(), ErrorKind::Internal);
        assert_eq!(context.state(), ExecutionState::Closing);
    }

    #[test]
    fn client_tracks_auxiliary_work_and_context_also_waits_for_untracked_runtime_work() {
        for tracked in [false, true] {
            let context = ExecutionContext::new(ExecutionOptions {
                worker_threads: 1,
                ..ExecutionOptions::default()
            })
            .unwrap();
            let (entered, observed) = flume::bounded(1);
            let (finish, gate) = flume::bounded(1);
            let reservation = context.reserve().unwrap();
            let client = crate::NativeClient::start(crate::ClientConfig::v4(
                "blocking-test",
                "127.0.0.1",
                65535,
            ))
            .unwrap();
            client.closer().close_now(Duration::from_secs(5)).unwrap();
            let work = Arc::new(DriverWork::default());
            let task_work = work.clone();
            let driver = Box::pin(DRIVER_WORK.scope(task_work, async move {
                let pending = tracked.then(register_blocking_work).flatten();
                tokio::task::spawn_blocking(move || {
                    let _pending = pending;
                    assert!(!blocking_wait_allowed());
                    entered.send(()).unwrap();
                    gate.recv().unwrap();
                });
            }));
            let done = reservation.start(1, client.handle(), work, driver).unwrap();
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            let observation = done.recv_timeout(Duration::from_millis(10)).unwrap_err();
            assert_eq!(
                observation,
                if tracked {
                    flume::RecvTimeoutError::Timeout
                } else {
                    flume::RecvTimeoutError::Disconnected
                }
            );
            context.request_shutdown();
            assert_eq!(
                context.join(Duration::from_millis(10)).unwrap_err().kind(),
                ErrorKind::Timeout
            );
            finish.send(()).unwrap();
            context.join(Duration::from_secs(5)).unwrap();
            assert_eq!(
                done.try_recv().unwrap_err(),
                flume::TryRecvError::Disconnected
            );
        }
    }
}
