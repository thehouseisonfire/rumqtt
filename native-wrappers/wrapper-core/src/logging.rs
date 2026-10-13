//! Owned, bounded logging execution independent of MQTT execution.
use crate::{Error, ErrorKind, Result};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
#[cfg(feature = "tracing")]
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_LOG_FIELDS: usize = 16;
pub const MAX_LOG_RECORD_BYTES: usize = 4096;
pub const MAX_LOG_TEXT_BYTES: usize = 256;
pub const MAX_LOG_MESSAGE_BYTES: usize = 1024;

/// Increasing verbosity; zero disables all records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LogValue {
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Text(String),
}
#[derive(Clone, Debug, PartialEq)]
pub struct LogField {
    pub name: String,
    pub value: LogValue,
}
/// Owned storage. Callback views and independently copied records refer to this value.
#[derive(Clone, Debug, PartialEq)]
pub struct LogRecord {
    pub name: String,
    pub target: String,
    pub level: LogLevel,
    pub client_id: u64,
    pub fields: Vec<LogField>,
    pub message: Option<String>,
    pub truncated: bool,
}
impl LogRecord {
    fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            + self.name.capacity()
            + self.target.capacity()
            + self.message.as_ref().map_or(0, String::capacity)
            + self.fields.capacity() * size_of::<LogField>()
            + self
                .fields
                .iter()
                .map(|f| {
                    f.name.capacity()
                        + match &f.value {
                            LogValue::Text(s) => s.capacity(),
                            _ => 0,
                        }
                })
                .sum::<usize>()
    }
}
#[derive(Clone, Debug)]
pub struct LogFilter {
    pub level: LogLevel,
    pub targets: Vec<String>,
}
impl Default for LogFilter {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            targets: Vec::new(),
        }
    }
}
impl LogFilter {
    fn validate(&self) -> Result<()> {
        if self.targets.len() > 16 || self.targets.iter().any(|s| s.len() > 128 || s.is_empty()) {
            return Err(Error::configuration(
                "logging target filter exceeds its limits",
            ));
        }
        Ok(())
    }
    #[cfg(feature = "tracing")]
    fn enabled(&self, level: LogLevel, target: &str) -> bool {
        level != LogLevel::Off
            && level as u32 <= self.level as u32
            && (self.targets.is_empty() || self.targets.iter().any(|s| s == target))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct LoggingOptions {
    pub record_capacity: usize,
    pub byte_capacity: usize,
    pub registration_capacity: usize,
}
impl Default for LoggingOptions {
    fn default() -> Self {
        Self {
            record_capacity: 1024,
            byte_capacity: 4 * 1024 * 1024,
            registration_capacity: 64,
        }
    }
}
/// Callbacks/destructors execute serially on the context thread, without queue locks.
/// Return false to disable this registration. Implementations must not unwind across foreign ABIs.
pub trait LogSink: Send + Sync + 'static {
    fn record(&self, record: &LogRecord) -> bool;
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoggingStatistics {
    pub accepted: u64,
    pub delivered: u64,
    pub overload: u64,
    pub rejected: u64,
    pub suppressed: u64,
    pub failures: u64,
    pub discarded: u64,
}
#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    delivered: AtomicU64,
    overload: AtomicU64,
    rejected: AtomicU64,
    suppressed: AtomicU64,
    failures: AtomicU64,
    discarded: AtomicU64,
}
impl Counters {
    fn snapshot(&self) -> LoggingStatistics {
        LoggingStatistics {
            accepted: self.accepted.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            overload: self.overload.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            suppressed: self.suppressed.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            discarded: self.discarded.load(Ordering::Relaxed),
        }
    }
}
fn increment(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_add(1))
    });
}
struct Slot {
    sink: Option<Box<dyn LogSink>>,
    lease: Weak<Lease>,
}
// A factory runs outside the queue lock. Its lease keeps this bounded slot alive;
// unwind releases the reservation before the lease can notify retirement.
struct SlotReservation {
    shared: Arc<Shared>,
    slot: usize,
    committed: bool,
}
impl Drop for SlotReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.shared.queue.lock().slots[self.slot] = None;
            self.shared.changed.notify_all();
        }
    }
}

struct Queued {
    registration: LoggingRegistration,
    record: LogRecord,
    sequence: u64,
}
struct Queue {
    open: bool,
    accepted: u64,
    bytes: usize,
    records: VecDeque<Queued>,
    slots: Vec<Option<Slot>>,
}
struct Shared {
    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    options: LoggingOptions,
    queue: Mutex<Queue>,
    changed: Condvar,
    processed: AtomicU64,
    progress: tokio::sync::Notify,
    counters: Counters,
    accepting: AtomicBool,
    exited: AtomicBool,
    failed: AtomicBool,
}
struct Owner {
    shared: Arc<Shared>,
    monitor: Mutex<Option<thread::JoinHandle<()>>>,
    stopped: flume::Receiver<()>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.shared.accepting.store(false, Ordering::Release);
        self.shared.queue.lock().open = false;
        self.shared.changed.notify_all();
    }
}
#[derive(Clone)]
pub struct LoggingContext {
    owner: Arc<Owner>,
}
impl std::fmt::Debug for LoggingContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingContext").finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LoggingState {
    Open = 0,
    Closing = 1,
    Quiescent = 2,
}
struct Lease {
    context: LoggingContext,
    slot: usize,
    #[cfg_attr(not(feature = "tracing"), allow(dead_code))]
    filter: LogFilter,
    counters: Counters,
    failed: AtomicBool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Pair retirement notification with the worker's condition-variable predicate.
        let _queue = self.context.owner.shared.queue.lock();
        self.context.owner.shared.changed.notify_all();
    }
}
#[derive(Clone)]
pub struct LoggingRegistration {
    lease: Arc<Lease>,
}
impl std::fmt::Debug for LoggingRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoggingRegistration")
            .finish_non_exhaustive()
    }
}
#[derive(Clone)]
pub struct LoggingFlush {
    context: LoggingContext,
    sequence: u64,
}
fn invalid_state(message: &'static str) -> Error {
    Error::new(ErrorKind::Shutdown, message).with_code(crate::ErrorCode::InvalidState)
}
fn spawn_delivery(shared: &Arc<Shared>) -> Result<(thread::JoinHandle<()>, flume::Receiver<()>)> {
    let (finished, stopped) = flume::bounded::<()>(0);
    let (send_worker, receive_worker) =
        std::sync::mpsc::sync_channel::<thread::JoinHandle<bool>>(1);
    let observed = shared.clone();
    // Only the delivery thread invokes sinks or destroys their owners/panic payloads.
    // This observer can wait for its complete exit, including foreign/Rust TLS teardown,
    // without making a timed caller perform an unbounded delivery-thread join.
    let monitor = thread::Builder::new()
        .name("rumqtt-logging-join".into())
        .spawn(move || {
            let _notify_exit = finished;
            let Ok(worker) = receive_worker.recv() else {
                return;
            };
            let failed = !worker.join().unwrap_or(false);
            if failed {
                observed.accepting.store(false, Ordering::Release);
                observed.queue.lock().open = false;
            }
            observed.failed.store(failed, Ordering::Relaxed);
            observed.exited.store(true, Ordering::Release);
        })
        .map_err(|_| {
            Error::new(
                ErrorKind::Internal,
                "could not start logging teardown observer",
            )
        })?;
    let worker = shared.clone();
    let Ok(thread) = thread::Builder::new()
        .name("rumqtt-logging".into())
        .spawn(move || run_guarded(&worker))
    else {
        drop(send_worker);
        let _ = monitor.join();
        return Err(Error::new(
            ErrorKind::Internal,
            "could not start logging thread",
        ));
    };
    if let Err(std::sync::mpsc::SendError(thread)) = send_worker.send(thread) {
        shared.accepting.store(false, Ordering::Release);
        shared.queue.lock().open = false;
        shared.changed.notify_all();
        let _ = thread.join();
        let _ = monitor.join();
        return Err(Error::new(
            ErrorKind::Internal,
            "logging teardown observer failed during startup",
        ));
    }
    Ok((monitor, stopped))
}
impl LoggingContext {
    /// No logging resources exist until this explicit constructor succeeds.
    /// # Errors
    /// Returns a configuration error for disabled tracing/invalid bounds, or a thread-start error.
    pub fn new(options: LoggingOptions) -> Result<Self> {
        if !cfg!(feature = "tracing") {
            return Err(Error::configuration(
                "C logging requires the tracing feature",
            ));
        }
        if options.record_capacity == 0
            || options.byte_capacity == 0
            || options.registration_capacity == 0
            || options
                .record_capacity
                .checked_mul(size_of::<Queued>())
                .is_none()
            || options
                .registration_capacity
                .checked_mul(size_of::<Option<Slot>>())
                .is_none()
            || options.byte_capacity > isize::MAX as usize
        {
            return Err(Error::configuration("invalid logging context bounds"));
        }
        let shared = Arc::new(Shared {
            options,
            counters: Counters::default(),
            accepting: AtomicBool::new(true),
            exited: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            changed: Condvar::new(),
            processed: AtomicU64::new(0),
            progress: tokio::sync::Notify::new(),
            queue: Mutex::new(Queue {
                open: true,
                accepted: 0,
                bytes: 0,
                records: VecDeque::new(),
                slots: Vec::new(),
            }),
        });
        let (monitor, stopped) = spawn_delivery(&shared)?;
        Ok(Self {
            owner: Arc::new(Owner {
                shared,
                monitor: Mutex::new(Some(monitor)),
                stopped,
            }),
        })
    }
    /// The factory is called only after all fallible validation/capacity checks.
    /// C adapters use it to transfer callback ownership transactionally.
    /// The factory runs without internal locks; reservation precedes concurrent shutdown.
    /// # Panics
    /// Propagates factory panics and releases the reserved slot before unwinding.
    /// # Errors
    /// Returns invalid-filter, closing-context, or registration-capacity errors before calling the factory.
    pub fn register_with(
        &self,
        filter: LogFilter,
        make_sink: impl FnOnce() -> Box<dyn LogSink>,
    ) -> Result<LoggingRegistration> {
        filter.validate()?;
        let shared = &self.owner.shared;
        let mut queue = shared.queue.lock();
        if !queue.open {
            return Err(invalid_state("logging context is closing"));
        }
        let slot = if let Some(slot) = queue.slots.iter().position(Option::is_none) {
            slot
        } else {
            if queue.slots.len() == shared.options.registration_capacity {
                return Err(Error::new(
                    ErrorKind::Backpressure,
                    "logging registration capacity exhausted",
                ));
            }
            queue.slots.try_reserve(1).map_err(|_| {
                Error::new(
                    ErrorKind::Internal,
                    "could not allocate logging registration",
                )
            })?;
            let slot = queue.slots.len();
            queue.slots.push(None);
            slot
        };
        let lease = Arc::new(Lease {
            context: self.clone(),
            slot,
            filter,
            counters: Counters::default(),
            failed: AtomicBool::new(false),
        });
        queue.slots[slot] = Some(Slot {
            sink: None,
            lease: Arc::downgrade(&lease),
        });
        drop(queue);
        let mut reservation = SlotReservation {
            shared: shared.clone(),
            slot,
            committed: false,
        };
        let sink = make_sink();
        shared.queue.lock().slots[slot]
            .as_mut()
            .expect("reserved logging slot")
            .sink = Some(sink);
        reservation.committed = true;
        shared.changed.notify_all();
        Ok(LoggingRegistration { lease })
    }
    #[must_use]
    pub fn statistics(&self) -> LoggingStatistics {
        self.owner.shared.counters.snapshot()
    }
    #[must_use]
    pub fn state(&self) -> LoggingState {
        let shared = &self.owner.shared;
        if shared.accepting.load(Ordering::Acquire) {
            LoggingState::Open
        } else if shared.exited.load(Ordering::Acquire)
            && !shared.failed.load(Ordering::Relaxed)
            && self.owner.monitor.lock().is_none()
        {
            LoggingState::Quiescent
        } else {
            LoggingState::Closing
        }
    }
    pub fn request_shutdown(&self) {
        self.owner.shared.accepting.store(false, Ordering::Release);
        self.owner.shared.queue.lock().open = false;
        self.owner.shared.changed.notify_all();
    }
    #[must_use]
    pub fn flush(&self) -> LoggingFlush {
        let sequence = self.owner.shared.queue.lock().accepted;
        LoggingFlush {
            context: self.clone(),
            sequence,
        }
    }
    /// # Errors
    /// Returns an error for an open context, forbidden blocking wait, invalid deadline, or worker panic.
    pub fn join(&self, timeout: Duration) -> Result<bool> {
        crate::execution::check_wait(timeout)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::configuration("logging deadline overflow"))?;
        let shared = &self.owner.shared;
        if shared.accepting.load(Ordering::Acquire) {
            return Err(invalid_state("shutdown logging before joining"));
        }
        if !shared.exited.load(Ordering::Acquire)
            && matches!(
                self.owner
                    .stopped
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                Err(flume::RecvTimeoutError::Timeout)
            )
        {
            return Ok(false);
        }
        loop {
            let Some(mut monitor) = self.owner.monitor.try_lock_until(deadline) else {
                return Ok(false);
            };
            if monitor.as_ref().is_some_and(|handle| !handle.is_finished()) {
                drop(monitor);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(false);
                }
                // The observer has already joined the delivery worker. It executes no sink
                // callbacks or owner/panic-payload destruction, so only its own epilogue remains.
                thread::sleep(remaining.min(Duration::from_millis(1)));
                continue;
            }
            if let Some(handle) = monitor.take() {
                handle.join().map_err(|_| {
                    Error::new(ErrorKind::Internal, "logging teardown observer panicked")
                })?;
            }
            return if !shared.exited.load(Ordering::Acquire) {
                Err(Error::new(
                    ErrorKind::Internal,
                    "logging teardown observer exited without joining",
                ))
            } else if shared.failed.load(Ordering::Relaxed) {
                Err(Error::new(ErrorKind::Internal, "logging thread panicked"))
            } else {
                Ok(true)
            };
        }
    }
}
impl LoggingFlush {
    /// # Errors
    /// Returns an error for a forbidden blocking wait or an overflowing deadline.
    pub fn wait(&self, timeout: Duration) -> Result<bool> {
        crate::execution::check_wait(timeout)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| Error::configuration("logging deadline overflow"))?;
        let shared = &self.context.owner.shared;
        if shared.processed.load(Ordering::Acquire) >= self.sequence {
            return Ok(true);
        }
        if timeout.is_zero() {
            return Ok(false);
        }
        // Notify does not reacquire the delivery queue mutex after waking. Drive its
        // listener directly so these synchronous waits need no Tokio runtime or timer.
        let waker = std::task::Waker::from(Arc::new(FlushWake(thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        loop {
            let mut notified = std::pin::pin!(shared.progress.notified());
            // Register before reading the predicate to avoid losing a completion wake.
            notified.as_mut().enable();
            if shared.processed.load(Ordering::Acquire) >= self.sequence {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            if std::future::Future::poll(notified.as_mut(), &mut cx).is_pending() {
                thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
            }
        }
    }
}
struct FlushWake(thread::Thread);
impl std::task::Wake for FlushWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
impl LoggingRegistration {
    #[must_use]
    pub fn statistics(&self) -> LoggingStatistics {
        self.lease.counters.snapshot()
    }
    #[must_use]
    pub fn failed(&self) -> bool {
        self.lease.failed.load(Ordering::Acquire)
    }
    /// # Errors
    /// Returns an invalid-state error when this registration failed or its context is closing.
    pub fn validate_attachment(&self) -> Result<()> {
        if self.failed() || !self.lease.context.owner.shared.queue.lock().open {
            return Err(invalid_state("logging registration is failed or closing"));
        }
        Ok(())
    }
    #[cfg(feature = "tracing")]
    fn count(&self, select: impl Fn(&Counters) -> &AtomicU64) {
        increment(select(&self.lease.counters));
        increment(select(&self.lease.context.owner.shared.counters));
    }
    #[cfg(feature = "tracing")]
    fn enqueue(&self, record: LogRecord) {
        let shared = &self.lease.context.owner.shared;
        if self.failed() {
            self.count(|c| &c.rejected);
            return;
        }
        let Some(mut queue) = shared.queue.try_lock() else {
            self.count(|c| &c.overload);
            return;
        };
        if !queue.open {
            self.count(|c| &c.rejected);
            return;
        }
        let bytes = record.storage_bytes();
        if queue.records.len() >= shared.options.record_capacity
            || bytes > shared.options.byte_capacity.saturating_sub(queue.bytes)
            || queue.accepted == u64::MAX
        {
            self.count(|c| &c.overload);
            return;
        }
        if queue.records.try_reserve(1).is_err() {
            self.count(|c| &c.overload);
            return;
        }
        queue.bytes += bytes;
        queue.accepted += 1;
        let sequence = queue.accepted;
        queue.records.push_back(Queued {
            registration: self.clone(),
            record,
            sequence,
        });
        self.count(|c| &c.accepted);
        shared.changed.notify_all();
    }
}
enum Work {
    Record(Queued, Box<dyn LogSink>),
    Destroy(usize, Box<dyn LogSink>),
}
fn run(shared: &Shared) {
    loop {
        let work = {
            let mut queue = shared.queue.lock();
            loop {
                if let Some(slot) = queue
                    .slots
                    .iter()
                    .position(|s| s.as_ref().is_some_and(|s| s.lease.strong_count() == 0))
                {
                    let sink = queue.slots[slot]
                        .as_mut()
                        .expect("retiring slot")
                        .sink
                        .take()
                        .expect("retiring sink");
                    break Work::Destroy(slot, sink);
                }
                if let Some(queued) = queue.records.pop_front() {
                    queue.bytes -= queued.record.storage_bytes();
                    let sink = queue.slots[queued.registration.lease.slot]
                        .as_mut()
                        .expect("retained registration slot")
                        .sink
                        .take()
                        .expect("single worker owns sink");
                    break Work::Record(queued, sink);
                }
                if !queue.open && queue.slots.iter().all(Option::is_none) {
                    drop(queue);
                    return;
                }
                shared.changed.wait(&mut queue);
            }
        };
        match work {
            Work::Record(queued, sink) => {
                let counters = &queued.registration.lease.counters;
                if queued.registration.failed() {
                    increment(&counters.discarded);
                    increment(&shared.counters.discarded);
                } else {
                    let success = callback(|| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            sink.record(&queued.record)
                        }))
                        .unwrap_or(false)
                    });
                    if success {
                        increment(&counters.delivered);
                        increment(&shared.counters.delivered);
                    } else {
                        queued
                            .registration
                            .lease
                            .failed
                            .store(true, Ordering::Release);
                        increment(&counters.failures);
                        increment(&shared.counters.failures);
                    }
                }
                {
                    let mut queue = shared.queue.lock();
                    queue.slots[queued.registration.lease.slot]
                        .as_mut()
                        .expect("active slot")
                        .sink = Some(sink);
                    drop(queue);
                    shared.processed.store(queued.sequence, Ordering::Release);
                    shared.progress.notify_waiters();
                    shared.changed.notify_all();
                }
                drop(queued);
            }
            Work::Destroy(slot, sink) => {
                callback(|| {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(sink)))
                        .is_err()
                    {
                        increment(&shared.counters.failures);
                    }
                });
                let mut queue = shared.queue.lock();
                queue.slots[slot] = None;
                drop(queue);
                shared.changed.notify_all();
            }
        }
    }
}
// Never pass a user-owned panic payload to the observer: its destructor can
// initialize TLS or block just like a sink. Even nested payload-drop panics stay
// on the delivery thread, where stalled teardown remains observable as pending.
fn run_guarded(shared: &Shared) -> bool {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(shared)));
    let success = result.is_ok();
    let mut payload = result.err();
    while let Some(value) = payload {
        payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))).err();
    }
    success
}
thread_local! { static SUPPRESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
pub fn suppressed() -> bool {
    SUPPRESS.get()
}

pub fn with_suppression<T>(suppress: bool, call: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SUPPRESS.set(self.0);
        }
    }
    let _restore = Restore(SUPPRESS.replace(SUPPRESS.get() || suppress));
    call()
}
fn callback<T>(call: impl FnOnce() -> T) -> T {
    with_suppression(true, || crate::runtime::with_host_callback(call))
}

// Shared live placement, separate from MQTT configuration and state.
#[derive(Clone, Default)]
pub struct ClientLogging {
    #[cfg(feature = "tracing")]
    inner: Arc<arc_swap::ArcSwapOption<Dispatch>>,
}
#[cfg(feature = "tracing")]
struct Dispatch {
    subscriber: rumqttc_core::native_tracing::NativeDispatch,
}
impl ClientLogging {
    #[cfg_attr(
        not(feature = "tracing"),
        allow(clippy::unused_self, clippy::needless_pass_by_value)
    )]
    pub(crate) fn set(
        &self,
        registration: Option<LoggingRegistration>,
        identity: u64,
    ) -> Result<()> {
        if let Some(registration) = &registration {
            registration.validate_attachment()?;
        }
        #[cfg(feature = "tracing")]
        self.inner.store(registration.map(|registration| {
            Arc::new(Dispatch {
                subscriber: Arc::new(Subscriber {
                    registration,
                    identity,
                    next_span: AtomicU64::new(1),
                }),
            })
        }));
        #[cfg(not(feature = "tracing"))]
        {
            let _ = identity;
            if registration.is_some() {
                return Err(Error::configuration("logging requires tracing"));
            }
        }
        Ok(())
    }
    #[cfg(feature = "tracing")]
    pub(crate) fn scope<T>(&self, call: impl FnOnce() -> T) -> T {
        let dispatch = self.inner.load_full();
        rumqttc_core::native_tracing::with_dispatch(
            dispatch.as_deref().map(|d| &d.subscriber),
            call,
        )
    }
    #[cfg(feature = "use-rustls-no-provider")]
    pub(crate) fn current() -> Self {
        #[cfg(feature = "tracing")]
        return CURRENT
            .try_with(|v| v.borrow().clone())
            .ok()
            .flatten()
            .unwrap_or_default();
        #[cfg(not(feature = "tracing"))]
        Self::default()
    }
    pub(crate) fn future<F: std::future::Future>(&self, future: F) -> LoggedFuture<F> {
        LoggedFuture {
            future: Some(Box::pin(future)),
            logging: self.clone(),
        }
    }
}
#[cfg(feature = "tracing")]
thread_local! { static CURRENT: std::cell::RefCell<Option<ClientLogging>> = const { std::cell::RefCell::new(None) }; }
pub struct LoggedFuture<F> {
    future: Option<std::pin::Pin<Box<F>>>,
    logging: ClientLogging,
}
impl<F: std::future::Future> std::future::Future for LoggedFuture<F> {
    type Output = F::Output;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let logging = self.logging.clone();
        logging.enter(|| {
            self.future
                .as_mut()
                .expect("live logged future")
                .as_mut()
                .poll(cx)
        })
    }
}
impl<F> Drop for LoggedFuture<F> {
    fn drop(&mut self) {
        let future = self.future.take();
        self.logging.enter(|| drop(future));
    }
}
impl ClientLogging {
    #[cfg_attr(not(feature = "tracing"), allow(clippy::unused_self))]
    pub(crate) fn enter<T>(&self, call: impl FnOnce() -> T) -> T {
        #[cfg(feature = "tracing")]
        {
            struct Restore(Option<ClientLogging>);
            impl Drop for Restore {
                fn drop(&mut self) {
                    let _ = CURRENT.try_with(|v| {
                        v.replace(self.0.take());
                    });
                }
            }
            let _restore = CURRENT
                .try_with(|v| v.replace(Some(self.clone())))
                .ok()
                .map(Restore);
            self.scope(call)
        }
        #[cfg(not(feature = "tracing"))]
        call()
    }
}
#[cfg(feature = "tracing")]
struct Subscriber {
    registration: LoggingRegistration,
    identity: u64,
    next_span: AtomicU64,
}
#[cfg(feature = "tracing")]
const fn level(level: tracing::Level) -> LogLevel {
    match level {
        tracing::Level::ERROR => LogLevel::Error,
        tracing::Level::WARN => LogLevel::Warn,
        tracing::Level::INFO => LogLevel::Info,
        tracing::Level::DEBUG => LogLevel::Debug,
        tracing::Level::TRACE => LogLevel::Trace,
    }
}
#[cfg(feature = "tracing")]
fn audited(metadata: &tracing::Metadata<'_>) -> bool {
    match metadata.target() {
        "rumqttc::lifecycle" => matches!(
            metadata.name(),
            "mqtt.connection_attempt"
                | "mqtt.connection_attempt_failed"
                | "mqtt.connection_established"
                | "mqtt.connection_lost"
                | "mqtt.session_restored"
                | "mqtt.replay_prepared"
                | "mqtt.protocol_violation"
                | "mqtt.protocol_compatibility"
        ),
        "rumqttc_wrapper_core::runtime" => matches!(
            metadata.name(),
            "mqtt.wrapper.start" | "mqtt.wrapper.driver"
        ),
        "rumqttc_wrapper_core::handle" => matches!(
            metadata.name(),
            "mqtt.wrapper.try_admit" | "mqtt.wrapper.admit"
        ),
        "rumqttc_wrapper_core::operations" => metadata.name() == "mqtt.wrapper.complete",
        "rumqttc::wrapper" => matches!(
            metadata.name(),
            "mqtt.wrapper.shutdown_requested" | "mqtt.wrapper.shutdown_completed"
        ),
        _ => false,
    }
}
#[cfg(feature = "tracing")]
impl Subscriber {
    fn record(&self, metadata: &tracing::Metadata<'_>, visit: impl FnOnce(&mut Visitor)) {
        if !self.enabled_record(metadata) {
            return;
        }
        if SUPPRESS.get() {
            self.registration.count(|c| &c.suppressed);
            return;
        }
        let mut record = LogRecord {
            name: metadata.name().into(),
            target: metadata.target().into(),
            level: level(*metadata.level()),
            client_id: self.identity,
            fields: Vec::with_capacity(MAX_LOG_FIELDS),
            message: None,
            truncated: false,
        };
        visit(&mut Visitor(&mut record));
        self.registration.enqueue(record);
    }
    fn enabled_record(&self, metadata: &tracing::Metadata<'_>) -> bool {
        if !audited(metadata)
            || !self
                .registration
                .lease
                .filter
                .enabled(level(*metadata.level()), metadata.target())
        {
            return false;
        }
        if self.registration.failed()
            || !self
                .registration
                .lease
                .context
                .owner
                .shared
                .accepting
                .load(Ordering::Acquire)
        {
            self.registration.count(|c| &c.rejected);
            return false;
        }
        true
    }
}
#[cfg(feature = "tracing")]
impl tracing::Subscriber for Subscriber {
    // Never cache registration-dependent interests across subscribers or filter replacement.
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        self.enabled_record(metadata)
    }
    fn new_span(&self, attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.record(attributes.metadata(), |visitor| attributes.record(visitor));
        tracing::span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed).max(1))
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        self.record(event.metadata(), |visitor| event.record(visitor));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn try_close(&self, _: tracing::span::Id) -> bool {
        true
    }
}
#[cfg(feature = "tracing")]
struct Visitor<'a>(&'a mut LogRecord);
#[cfg(feature = "tracing")]
impl Visitor<'_> {
    fn allowed(field: &tracing::field::Field) -> bool {
        matches!(
            field.name(),
            "message"
                | "protocol"
                | "attempt_id"
                | "connection_generation"
                | "attempt_in_generation"
                | "reconnect"
                | "phase"
                | "error_kind"
                | "error"
                | "session_present"
                | "raw_session_present"
                | "session_resumed"
                | "compatibility"
                | "inflight"
                | "inflight_limit"
                | "queue_depth"
                | "replay_count"
                | "violation_kind"
                | "operation_id"
                | "success"
                | "mode"
        )
    }
    fn value(&mut self, field: &tracing::field::Field, value: LogValue) {
        if !Self::allowed(field) {
            return;
        }
        if field.name() == "message" {
            if let LogValue::Text(text) = value {
                if text.capacity() <= MAX_LOG_RECORD_BYTES.saturating_sub(self.0.storage_bytes()) {
                    self.0.message = Some(text);
                } else {
                    self.0.truncated = true;
                }
            }
            return;
        }
        let bytes = field.name().len()
            + match &value {
                LogValue::Text(s) => s.capacity(),
                _ => 0,
            };
        if self.0.fields.len() == MAX_LOG_FIELDS
            || bytes > MAX_LOG_RECORD_BYTES.saturating_sub(self.0.storage_bytes())
        {
            self.0.truncated = true;
            return;
        }
        self.0.fields.push(LogField {
            name: field.name().into(),
            value,
        });
    }
}
#[cfg(feature = "tracing")]
struct BoundedText {
    value: String,
    limit: usize,
    truncated: bool,
}
#[cfg(feature = "tracing")]
impl std::fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let mut count = value.len().min(self.limit.saturating_sub(self.value.len()));
        while !value.is_char_boundary(count) {
            count -= 1;
        }
        self.value.push_str(&value[..count]);
        if count != value.len() {
            self.truncated = true;
            return Err(std::fmt::Error);
        }
        Ok(())
    }
}
#[cfg(feature = "tracing")]
impl tracing::field::Visit for Visitor<'_> {
    fn record_bool(&mut self, f: &tracing::field::Field, v: bool) {
        self.value(f, LogValue::Bool(v));
    }
    fn record_i64(&mut self, f: &tracing::field::Field, v: i64) {
        self.value(f, LogValue::Signed(v));
    }
    fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
        self.value(f, LogValue::Unsigned(v));
    }
    fn record_f64(&mut self, f: &tracing::field::Field, v: f64) {
        self.value(f, LogValue::Float(v));
    }
    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
        if !Self::allowed(f) {
            return;
        }
        let mut text = BoundedText {
            value: String::with_capacity(if f.name() == "message" {
                MAX_LOG_MESSAGE_BYTES
            } else {
                MAX_LOG_TEXT_BYTES
            }),
            limit: if f.name() == "message" {
                MAX_LOG_MESSAGE_BYTES
            } else {
                MAX_LOG_TEXT_BYTES
            },
            truncated: false,
        };
        let _ = text.write_str(v);
        self.0.truncated |= text.truncated;
        self.value(f, LogValue::Text(text.value));
    }
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        if !Self::allowed(f) {
            return;
        }
        let mut text = BoundedText {
            value: String::with_capacity(if f.name() == "message" {
                MAX_LOG_MESSAGE_BYTES
            } else {
                MAX_LOG_TEXT_BYTES
            }),
            limit: if f.name() == "message" {
                MAX_LOG_MESSAGE_BYTES
            } else {
                MAX_LOG_TEXT_BYTES
            },
            truncated: false,
        };
        let _ = write!(text, "{v:?}");
        self.0.truncated |= text.truncated;
        self.value(f, LogValue::Text(text.value));
    }
}

#[cfg(all(test, feature = "tracing"))]
mod tests {
    use super::*;
    use std::sync::mpsc;
    const DEADLINE: Duration = Duration::from_secs(5);
    struct Capture(Arc<Mutex<Vec<LogRecord>>>, Arc<AtomicU64>);
    impl LogSink for Capture {
        fn record(&self, r: &LogRecord) -> bool {
            self.0.lock().push(r.clone());
            true
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            increment(&self.1);
        }
    }
    fn capture(
        context: &LoggingContext,
    ) -> (
        LoggingRegistration,
        Arc<Mutex<Vec<LogRecord>>>,
        Arc<AtomicU64>,
    ) {
        let records = Arc::new(Mutex::new(Vec::new()));
        let drops = Arc::new(AtomicU64::new(0));
        let registration = context
            .register_with(
                LogFilter {
                    level: LogLevel::Trace,
                    targets: vec![],
                },
                || Box::new(Capture(records.clone(), drops.clone())),
            )
            .unwrap();
        (registration, records, drops)
    }
    fn emit(logging: &ClientLogging, attempt: u64) {
        logging.enter(|| rumqttc_core::scoped_event!(name: "mqtt.connection_attempt", target: "rumqttc::lifecycle", tracing::Level::DEBUG, attempt_id = attempt));
    }
    fn emit_accepted(logging: &ClientLogging, registration: &LoggingRegistration, attempt: u64) {
        let before = registration.statistics().accepted;
        let deadline = Instant::now() + DEADLINE;
        loop {
            emit(logging, attempt);
            if registration.statistics().accepted > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "logging admission never became available"
            );
            thread::yield_now();
        }
    }
    #[test]
    fn typed_records_are_filtered_before_formatting_and_truncated_without_private_fields() {
        struct Never;
        impl std::fmt::Debug for Never {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("private field must not be formatted")
            }
        }
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let (registration, records, drops) = capture(&context);
        let logging = ClientLogging::default();
        logging.set(Some(registration.clone()), 42).unwrap();
        let held_output = records.lock();
        emit_accepted(&logging, &registration, 7);
        let deadline = Instant::now() + DEADLINE;
        while !context.owner.shared.queue.lock().records.is_empty() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        logging.enter(|| {
            rumqttc_core::scoped_event!(name: "unsupported", target: "dependency", tracing::Level::ERROR, error = ?Never);
            rumqttc_core::scoped_event!(name: "mqtt.connection_attempt", target: "rumqttc::lifecycle", tracing::Level::DEBUG,
                attempt_id = 8u64, reconnect = true, phase = "é".repeat(300).as_str(), secret = ?Never);
        });
        drop(held_output);
        assert!(context.flush().wait(DEADLINE).unwrap());
        let rows = records.lock();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].client_id, 42);
        assert!(rows[0].message.is_none());
        assert_eq!(rows[0].fields[0].value, LogValue::Unsigned(7));
        assert!(rows[1].truncated);
        assert_eq!(rows[1].fields[1].value, LogValue::Bool(true));
        assert!(rows[1].storage_bytes() <= MAX_LOG_RECORD_BYTES);
        assert!(matches!(&rows[1].fields[2].value, LogValue::Text(s) if s.len() == 256));
        drop(rows);
        logging.set(None, 42).unwrap();
        drop(registration);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn stalled_callback_bounds_queue_and_flush_survives_timeout_and_replacement() {
        struct Stall {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            drops: Arc<AtomicU64>,
        }
        impl LogSink for Stall {
            fn record(&self, _: &LogRecord) -> bool {
                self.entered.send(()).unwrap();
                self.release.lock().recv_timeout(DEADLINE).unwrap();
                true
            }
        }
        impl Drop for Stall {
            fn drop(&mut self) {
                increment(&self.drops);
            }
        }
        let context = LoggingContext::new(LoggingOptions {
            record_capacity: 2,
            ..LoggingOptions::default()
        })
        .unwrap();
        let (entered, seen) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let drops = Arc::new(AtomicU64::new(0));
        let registration = context
            .register_with(
                LogFilter {
                    level: LogLevel::Trace,
                    targets: vec![],
                },
                || {
                    Box::new(Stall {
                        entered,
                        release: Mutex::new(wait),
                        drops: drops.clone(),
                    })
                },
            )
            .unwrap();
        let logging = ClientLogging::default();
        logging.set(Some(registration.clone()), 1).unwrap();
        emit_accepted(&logging, &registration, 1);
        seen.recv_timeout(DEADLINE).unwrap();
        let overload_before = registration.statistics().overload;
        emit(&logging, 2);
        emit(&logging, 3);
        emit(&logging, 4);
        let flush = context.flush();
        assert!(!flush.wait(Duration::ZERO).unwrap());
        assert_eq!(registration.statistics().accepted, 3);
        assert_eq!(registration.statistics().overload, overload_before + 1);
        let (replacement, captured, _) = capture(&context);
        logging.set(Some(replacement.clone()), 1).unwrap();
        drop(registration);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        for _ in 0..3 {
            release.send(()).unwrap();
        }
        assert!(flush.wait(DEADLINE).unwrap());
        assert!(captured.lock().is_empty());
        emit_accepted(&logging, &replacement, 5);
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert_eq!(captured.lock().len(), 1);
        logging.set(None, 1).unwrap();
        drop(replacement);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn flush_deadlines_do_not_wait_for_the_delivery_queue() {
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let (registration, records, _) = capture(&context);
        let logging = ClientLogging::default();
        logging.set(Some(registration.clone()), 1).unwrap();
        let output_guard = records.lock();
        emit_accepted(&logging, &registration, 1);
        let flush = context.flush();
        let queue_guard = context.owner.shared.queue.lock();
        let observer = flush.clone();
        let (result, received) = mpsc::channel();
        let poller = thread::spawn(move || {
            let zero =
                crate::runtime::with_host_callback(|| observer.wait(Duration::ZERO)).unwrap();
            let finite = observer.wait(Duration::from_millis(20)).unwrap();
            result.send((zero, finite)).unwrap();
        });
        // Repeated wakes must neither reacquire the queue nor restart the deadline.
        let deadline = Instant::now() + Duration::from_millis(200);
        let result = loop {
            context.owner.shared.progress.notify_waiters();
            let result = received.recv_timeout(Duration::from_millis(1));
            if !matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) || Instant::now() >= deadline
            {
                break result;
            }
        };
        drop(queue_guard);
        drop(output_guard);
        poller.join().unwrap();
        assert_eq!(result.unwrap(), (false, false));
        assert!(flush.wait(DEADLINE).unwrap());
        assert_eq!(records.lock().len(), 1);

        // A completed prefix remains observable even while unrelated queue work holds the lock.
        let queue_guard = context.owner.shared.queue.lock();
        assert!(flush.wait(Duration::ZERO).unwrap());
        assert!(flush.wait(Duration::from_millis(20)).unwrap());
        drop(queue_guard);
        logging.set(None, 1).unwrap();
        drop(registration);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }
    #[test]
    fn failed_sink_discards_its_backlog_without_failing_peers() {
        struct Fail(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);
        impl LogSink for Fail {
            fn record(&self, _: &LogRecord) -> bool {
                self.0.send(()).unwrap();
                self.1.lock().recv_timeout(DEADLINE).unwrap();
                false
            }
        }
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let (entered, seen) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let failed = context
            .register_with(
                LogFilter {
                    level: LogLevel::Trace,
                    targets: vec![],
                },
                || Box::new(Fail(entered, Mutex::new(wait))),
            )
            .unwrap();
        let logging = ClientLogging::default();
        logging.set(Some(failed.clone()), 1).unwrap();
        emit_accepted(&logging, &failed, 1);
        seen.recv_timeout(DEADLINE).unwrap();
        emit(&logging, 2);
        emit(&logging, 3);
        release.send(()).unwrap();
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert!(failed.failed());
        assert_eq!(failed.statistics().discarded, 2);
        emit(&logging, 4);
        assert_eq!(failed.statistics().failures, 1);
        assert_eq!(failed.statistics().rejected, 1);
        let (peer, records, _) = capture(&context);
        logging.set(Some(peer.clone()), 2).unwrap();
        emit_accepted(&logging, &peer, 3);
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert_eq!(records.lock().len(), 1);
        logging.set(None, 2).unwrap();
        drop(failed);
        drop(peer);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }
    #[test]
    fn logging_callbacks_suppress_reentrancy_and_forbid_blocking_native_waits() {
        struct Reenter(
            Arc<Mutex<Option<ClientLogging>>>,
            crate::operations::OperationRegistry,
            mpsc::Sender<crate::OperationId>,
        );
        impl LogSink for Reenter {
            fn record(&self, _: &LogRecord) -> bool {
                assert!(!crate::blocking_wait_allowed());
                emit(self.0.lock().as_ref().unwrap(), 2);
                self.2
                    .send(self.1.allocate().unwrap().operation_id)
                    .unwrap();
                true
            }
        }
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let binding = Arc::new(Mutex::new(None));
        let (registry, _receivers) = crate::operations::OperationRegistry::new(1);
        let (admitted, observed) = mpsc::channel();
        let registration = context
            .register_with(
                LogFilter {
                    level: LogLevel::Trace,
                    targets: vec![],
                },
                || Box::new(Reenter(binding.clone(), registry.clone(), admitted)),
            )
            .unwrap();
        let logging = ClientLogging::default();
        logging.set(Some(registration.clone()), 1).unwrap();
        *binding.lock() = Some(logging.clone());
        emit_accepted(&logging, &registration, 1);
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert_eq!(registration.statistics().suppressed, 1);
        let operation = observed.recv_timeout(DEADLINE).unwrap();
        logging.enter(|| registry.complete(operation, Ok(crate::Completion::GracefulShutdown)));
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert_eq!(registration.statistics().accepted, 1);
        assert_eq!(registration.statistics().suppressed, 2);
        *binding.lock() = None;
        logging.set(None, 1).unwrap();
        drop(registration);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }
    #[test]
    fn destructor_stall_preserves_capacity_and_prevents_false_quiescence() {
        struct StallDrop(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);
        impl LogSink for StallDrop {
            fn record(&self, _: &LogRecord) -> bool {
                true
            }
        }
        impl Drop for StallDrop {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
                self.1.lock().recv_timeout(DEADLINE).unwrap();
            }
        }
        let context = LoggingContext::new(LoggingOptions {
            registration_capacity: 1,
            ..LoggingOptions::default()
        })
        .unwrap();
        let (entered, seen) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let registration = context
            .register_with(LogFilter::default(), || {
                Box::new(StallDrop(entered, Mutex::new(wait)))
            })
            .unwrap();
        drop(registration);
        seen.recv_timeout(DEADLINE).unwrap();
        assert!(
            context
                .register_with(LogFilter::default(), || panic!(
                    "capacity rejection must not transfer ownership"
                ))
                .is_err()
        );
        context.request_shutdown();
        assert!(!context.join(Duration::ZERO).unwrap());
        assert_eq!(context.state(), LoggingState::Closing);
        release.send(()).unwrap();
        assert!(context.join(DEADLINE).unwrap());
        assert_eq!(context.state(), LoggingState::Quiescent);
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "Exercise callback/owner TLS teardown and deadline/retry observations in one fixture"
    )]
    fn join_deadlines_include_delivery_thread_local_destruction() {
        struct ExitGate {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
            finished: Arc<AtomicBool>,
        }
        impl Drop for ExitGate {
            fn drop(&mut self) {
                self.entered.send(()).unwrap();
                self.release.recv_timeout(DEADLINE).unwrap();
                self.finished.store(true, Ordering::Release);
            }
        }
        thread_local! {
            static EXIT: std::cell::RefCell<Option<ExitGate>> = const { std::cell::RefCell::new(None) };
        }
        struct Sink {
            gate: Mutex<Option<ExitGate>>,
            initialize_on_drop: bool,
            destroyed: Arc<AtomicBool>,
        }
        impl Sink {
            fn initialize(&self) {
                EXIT.with(|slot| *slot.borrow_mut() = self.gate.lock().take());
            }
        }
        impl LogSink for Sink {
            fn record(&self, _: &LogRecord) -> bool {
                if !self.initialize_on_drop {
                    self.initialize();
                }
                true
            }
        }
        impl Drop for Sink {
            fn drop(&mut self) {
                if self.initialize_on_drop {
                    self.initialize();
                }
                self.destroyed.store(true, Ordering::Release);
            }
        }
        struct ReleaseOnDrop(mpsc::Sender<()>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        for initialize_on_drop in [false, true] {
            let context = LoggingContext::new(LoggingOptions::default()).unwrap();
            let (entered, observed) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let release = ReleaseOnDrop(release);
            let finished = Arc::new(AtomicBool::new(false));
            let destroyed = Arc::new(AtomicBool::new(false));
            let registration = context
                .register_with(
                    LogFilter {
                        level: LogLevel::Trace,
                        ..LogFilter::default()
                    },
                    || {
                        Box::new(Sink {
                            gate: Mutex::new(Some(ExitGate {
                                entered,
                                release: wait,
                                finished: finished.clone(),
                            })),
                            initialize_on_drop,
                            destroyed: destroyed.clone(),
                        })
                    },
                )
                .unwrap();
            if !initialize_on_drop {
                let logging = ClientLogging::default();
                logging.set(Some(registration.clone()), 1).unwrap();
                emit_accepted(&logging, &registration, 1);
                assert!(context.flush().wait(DEADLINE).unwrap());
                logging.set(None, 1).unwrap();
            }
            drop(registration);
            context.request_shutdown();
            observed.recv_timeout(DEADLINE).unwrap();
            assert!(destroyed.load(Ordering::Acquire));
            assert!(!finished.load(Ordering::Acquire));
            assert_eq!(context.state(), LoggingState::Closing);
            let waiter = context.clone();
            let (result, received) = mpsc::channel();
            let poller = thread::spawn(move || {
                let zero = waiter.join(Duration::ZERO).unwrap();
                let finite = waiter.join(Duration::from_millis(20)).unwrap();
                result.send((zero, finite)).unwrap();
            });
            let result = received.recv_timeout(Duration::from_millis(200));
            release.0.send(()).unwrap();
            poller.join().unwrap();
            assert_eq!(result.unwrap(), (false, false));
            assert!(context.join(DEADLINE).unwrap());
            assert!(finished.load(Ordering::Acquire));
            assert_eq!(context.state(), LoggingState::Quiescent);
            assert!(context.join(Duration::ZERO).unwrap());
        }
    }
    #[test]
    fn join_deadlines_include_contended_observation_locks() {
        for hold_monitor in [false, true] {
            let context = LoggingContext::new(LoggingOptions::default()).unwrap();
            let mut queue_guard = (!hold_monitor).then(|| context.owner.shared.queue.lock());
            if let Some(queue) = &mut queue_guard {
                queue.open = false;
                context
                    .owner
                    .shared
                    .accepting
                    .store(false, Ordering::Release);
                context.owner.shared.changed.notify_all();
            } else {
                context.request_shutdown();
                let deadline = Instant::now() + DEADLINE;
                while !context.owner.shared.exited.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
            }
            let monitor_guard = hold_monitor.then(|| context.owner.monitor.lock());
            let observer = context.clone();
            let (result, received) = mpsc::channel();
            let poller = thread::spawn(move || {
                let zero = observer.join(Duration::ZERO).unwrap();
                let finite = observer.join(Duration::from_millis(20)).unwrap();
                result.send((zero, finite)).unwrap();
            });
            let result = received.recv_timeout(Duration::from_millis(200));
            drop(queue_guard);
            drop(monitor_guard);
            poller.join().unwrap();
            assert_eq!(result.unwrap(), (false, false));
            assert!(context.join(DEADLINE).unwrap());
        }
    }
    #[test]
    fn nested_unattached_clients_clear_the_parent_clients_source_route() {
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let (registration, records, _) = capture(&context);
        let attached = ClientLogging::default();
        attached.set(Some(registration.clone()), 1).unwrap();
        let unattached = ClientLogging::default();
        let held_output = records.lock();
        emit_accepted(&attached, &registration, 1);
        let deadline = Instant::now() + DEADLINE;
        while !context.owner.shared.queue.lock().records.is_empty() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        attached.enter(|| unattached.enter(|| {
            rumqttc_core::scoped_event!(name: "mqtt.connection_attempt", target: "rumqttc::lifecycle", tracing::Level::DEBUG, attempt_id = 99u64);
        }));
        drop(held_output);
        assert!(context.flush().wait(DEADLINE).unwrap());
        assert_eq!(records.lock().len(), 1);
        attached.set(None, 1).unwrap();
        drop(registration);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn futures_restore_dispatch_on_every_poll_and_on_unpolled_cancellation() {
        struct FinalRecord;
        impl Drop for FinalRecord {
            fn drop(&mut self) {
                rumqttc_core::scoped_event!(name: "mqtt.connection_attempt", target: "rumqttc::lifecycle", tracing::Level::DEBUG, attempt_id = 1u64);
            }
        }

        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        let (registration, records, _) = capture(&context);
        let a = ClientLogging::default();
        let b = ClientLogging::default();
        a.set(Some(registration.clone()), 1).unwrap();
        b.set(Some(registration.clone()), 2).unwrap();
        emit_accepted(&a, &registration, 1);
        let tasks: Vec<_> = [a.clone(), b.clone()].into_iter().enumerate().map(|(index, logging)| {
            tokio::spawn(logging.future(async move {
                for _ in 0..32 {
                    rumqttc_core::scoped_event!(name: "mqtt.connection_attempt", target: "rumqttc::lifecycle", tracing::Level::DEBUG, attempt_id = (index + 1) as u64);
                    tokio::task::yield_now().await;
                }
            }))
        }).collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert!(context.flush().wait(DEADLINE).unwrap());
        let rows = records.lock().clone();
        assert!(!rows.is_empty());
        for row in rows {
            assert_eq!(row.fields[0].value, LogValue::Unsigned(row.client_id));
        }
        let probe = FinalRecord;
        let pending = a.future(async move {
            drop(probe);
            std::future::pending::<()>().await;
        });
        let before = registration.statistics().overload;
        // The cancelled future's destructor must still route through this sink.
        let guard = context.owner.shared.queue.lock();
        drop(pending);
        assert_eq!(registration.statistics().overload, before + 1);
        drop(guard);
        a.set(None, 1).unwrap();
        b.set(None, 2).unwrap();
        drop(registration);
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }

    #[test]
    fn filters_and_bounds_fail_before_sink_ownership_transfers() {
        assert!(
            LoggingContext::new(LoggingOptions {
                record_capacity: 0,
                ..LoggingOptions::default()
            })
            .is_err()
        );
        let context = LoggingContext::new(LoggingOptions::default()).unwrap();
        assert!(
            context
                .register_with(
                    LogFilter {
                        targets: vec!["a".repeat(129)],
                        ..LogFilter::default()
                    },
                    || panic!("invalid filter must not transfer ownership")
                )
                .is_err()
        );
        let tiny = LoggingContext::new(LoggingOptions {
            byte_capacity: 1,
            ..LoggingOptions::default()
        })
        .unwrap();
        let (registration, records, _) = capture(&tiny);
        let logging = ClientLogging::default();
        logging.set(Some(registration.clone()), 1).unwrap();
        emit(&logging, 1);
        assert_eq!(registration.statistics().overload, 1);
        assert!(records.lock().is_empty());
        logging.set(None, 1).unwrap();
        drop(registration);
        tiny.request_shutdown();
        assert!(tiny.join(DEADLINE).unwrap());
        let slot = LoggingContext::new(LoggingOptions {
            registration_capacity: 1,
            ..LoggingOptions::default()
        })
        .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            slot.register_with(LogFilter::default(), || {
                assert_eq!(slot.state(), LoggingState::Open);
                panic!("factory failure");
            })
        }));
        assert!(result.is_err());
        let (registration, _, _) = capture(&slot);
        drop(registration);
        slot.request_shutdown();
        assert!(slot.join(DEADLINE).unwrap());
        assert!(context.join(Duration::ZERO).is_err());
        context.request_shutdown();
        assert!(context.join(DEADLINE).unwrap());
    }
}
