//! Bounded, non-consuming readiness for foreign event loops.
//!
//! One logical drainer collects hints, observes sources, acknowledges revisions, and calls
//! `prepare_wait` before sleeping. Publication never acquires an administration lock. Resources
//! remain owned until context release, including after close/quiescence.
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use arc_swap::ArcSwapOption;

use crate::{
    CompletionHandle, ConnectionHandle, Error, ErrorCode, ErrorKind, PendingAdmissionHandle,
    Result, WrapperEvent,
};

mod platform;
use platform::Primitive;

/// Maximum subscriptions to one source, including subscriptions across contexts.
pub const MAX_SOURCE_SUBSCRIBERS: usize = 16;
/// Default number of registration slots per context.
pub const DEFAULT_NOTIFICATION_CAPACITY: usize = 1024;

/// The authoritative observation to inspect when a registration is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SourceKind {
    Events = 1,
    Completion = 2,
    PendingAdmission = 3,
    FirstConnection = 4,
}

/// Context-local identity; generations never repeat within a context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegistrationKey {
    pub slot: u64,
    pub generation: u64,
}

/// A hint, not an event or operation result. Acknowledge only after inspecting the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadyEntry {
    pub key: RegistrationKey,
    pub tag: u64,
    pub kind: SourceKind,
    pub revision: u64,
    pub invalidated: bool,
}

/// Resources remain valid in all states until their context ownership is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum NotificationState {
    Open = 0,
    Closing = 1,
    Quiescent = 2,
}

/// Administrative state and registration usage sampled without racing slot retirement.
#[derive(Clone, Copy, Debug)]
pub struct NotificationStatus {
    pub state: NotificationState,
    pub signal_error: i32,
    pub registrations: usize,
    pub capacity: usize,
}

/// Consumer-owned queue view. Registrations only keep a weak reference to this view.
/// Its sender allows checking buffered events even after the driver's sender is gone. It cannot
/// prevent receiver disconnection, and terminal status uses its independent channel.
#[derive(Debug)]
pub(crate) struct EventView(pub(crate) flume::Sender<WrapperEvent>);

/// Source-local wakeup state; does not own the client or its execution.
#[derive(Debug)]
pub(crate) struct Source {
    kind: SourceKind,
    revision: AtomicU64,
    invalidated: AtomicBool,
    terminal_published: AtomicBool,
    terminal_consumed: AtomicBool,
    events: OnceLock<Weak<EventView>>,
    subscribers: ArcSwapOption<Vec<Weak<Target>>>,
    administration: Mutex<()>,
    #[cfg(test)]
    after_snapshot: Mutex<Option<tests::Gate>>,
    #[cfg(test)]
    before_entry: Mutex<Option<tests::Gate>>,
}

impl Source {
    pub(crate) fn new(kind: SourceKind) -> Arc<Self> {
        Arc::new(Self {
            kind,
            // Events can become visible before their enqueue publication hook. Reserve the
            // initial revision so acknowledging that hint cannot acknowledge a future publication.
            revision: AtomicU64::new(u64::from(kind == SourceKind::Events)),
            invalidated: AtomicBool::new(false),
            terminal_published: AtomicBool::new(false),
            terminal_consumed: AtomicBool::new(false),
            events: OnceLock::new(),
            subscribers: ArcSwapOption::empty(),
            administration: Mutex::new(()),
            #[cfg(test)]
            after_snapshot: Mutex::new(None),
            #[cfg(test)]
            before_entry: Mutex::new(None),
        })
    }

    pub(crate) fn event_view(&self, sender: flume::Sender<WrapperEvent>) -> Arc<EventView> {
        let view = Arc::new(EventView(sender));
        self.events
            .set(Arc::downgrade(&view))
            .expect("event view initialized once");
        view
    }

    fn available(&self) -> bool {
        if self.kind != SourceKind::Events {
            return self.revision.load(SeqCst) != 0;
        }
        self.events
            .get()
            .and_then(Weak::upgrade)
            .is_some_and(|view| !view.0.is_empty())
            || (self.terminal_published.load(SeqCst) && !self.terminal_consumed.load(SeqCst))
    }

    pub(crate) fn complete(&self) {
        if !self.terminal_published.swap(true, SeqCst) {
            self.publish();
        }
    }

    pub(crate) fn terminal_consumed(&self) {
        self.terminal_consumed.store(true, SeqCst);
    }

    pub(crate) fn terminal_published(&self) {
        self.terminal_published.store(true, SeqCst);
        self.publish();
    }

    pub(crate) fn invalidate(&self) {
        self.invalidated.store(true, SeqCst);
        self.publish();
    }

    pub(crate) fn publish(&self) {
        // Reaching u64::MAX latches a failure, never a wrapped/ambiguous revision.
        let previous = self
            .revision
            .fetch_update(SeqCst, SeqCst, |revision| revision.checked_add(1));
        let subscribers = self.subscribers.load();
        #[cfg(test)]
        tests::pause(&self.after_snapshot);
        if let Some(subscribers) = &*subscribers {
            for target in subscribers.iter().filter_map(Weak::upgrade) {
                if !target.active.load(SeqCst) {
                    continue;
                }
                if let Some(signal) = target.signal.upgrade() {
                    match previous {
                        Ok(revision) => {
                            target.published.fetch_max(revision + 1, SeqCst);
                            signal.notify();
                        }
                        Err(_) => signal.fail(-1),
                    }
                }
            }
        }
    }

    fn subscribe(&self, target: &Arc<Target>) -> Result<()> {
        let _guard = lock(&self.administration);
        let current = self.subscribers.load();
        // Most operation sources have one observer. Grow only with their bounded fan-out.
        let capacity = current.as_ref().map_or(1, |subscribers| {
            (subscribers.len() + 1).min(MAX_SOURCE_SUBSCRIBERS)
        });
        let mut next = Vec::with_capacity(capacity);
        if let Some(current) = &*current {
            next.extend(
                current
                    .iter()
                    .filter(|target| target.strong_count() != 0)
                    .cloned(),
            );
        }
        if next.len() == MAX_SOURCE_SUBSCRIBERS {
            return Err(Error::new(
                ErrorKind::Backpressure,
                "source subscriber limit reached",
            ));
        }
        next.push(Arc::downgrade(target));
        self.subscribers.store(Some(Arc::new(next)));
        Ok(())
    }

    fn unsubscribe(&self, target: &Arc<Target>) {
        let _guard = lock(&self.administration);
        let current = self.subscribers.load();
        if let Some(current) = &*current {
            let next: Vec<_> = current
                .iter()
                .filter(|weak| weak.strong_count() != 0 && !weak.ptr_eq(&Arc::downgrade(target)))
                .cloned()
                .collect();
            self.subscribers
                .store((!next.is_empty()).then(|| Arc::new(next)));
        }
    }
}

/// A non-consuming event observer. Retaining it does not retain the consumer or client.
#[derive(Clone, Debug)]
pub struct EventReadiness(pub(crate) Arc<Source>);

#[derive(Debug)]
struct Target {
    signal: Weak<Signal>,
    active: AtomicBool,
    published: AtomicU64,
    acknowledged: AtomicU64,
}

// These owners intentionally have different cancellation semantics.
enum Observation {
    Events(EventReadiness),
    Completion(CompletionHandle),
    FirstConnection(ConnectionHandle),
    Pending(PendingAdmissionHandle),
}

impl Observation {
    fn source(&self) -> &Arc<Source> {
        match self {
            Self::Events(events) => &events.0,
            Self::Completion(completion) => completion.notification_source(),
            Self::FirstConnection(connection) => connection.notification_source(),
            Self::Pending(pending) => pending.notification_source(),
        }
    }
}

struct Registration {
    key: RegistrationKey,
    tag: u64,
    target: Arc<Target>,
    source: Arc<Source>,
    // Retain the source-specific observer, including pending external-owner accounting.
    _observation: Observation,
}

impl Registration {
    fn entry(&self) -> Option<ReadyEntry> {
        let source = &self.source;
        let published = self.target.published.load(SeqCst);
        let revision = published.max(source.revision.load(SeqCst)).max(1);
        // Invalidation sets its flag before publishing a new revision. Read the revision
        // first so a false flag can never accompany the invalidation's revision. A true
        // flag with an older revision is harmless: duplicate hints are permitted.
        let invalidated = source.invalidated.load(SeqCst);
        let acknowledged = self.target.acknowledged.load(SeqCst);
        let level = source.kind == SourceKind::Events && !invalidated && source.available();
        #[cfg(test)]
        tests::pause(&source.before_entry);
        (published > acknowledged || level).then(|| ReadyEntry {
            key: self.key,
            tag: self.tag,
            kind: source.kind,
            revision,
            invalidated,
        })
    }

    fn detach(&self) {
        self.target.active.store(false, SeqCst);
        self.source.unsubscribe(&self.target);
    }
}

struct Table {
    slots: Vec<Option<Registration>>,
    next_generation: u64,
    cursor: usize,
}

struct Signal {
    primitive: Primitive,
    closed: AtomicBool,
    close_done: AtomicBool,
    in_flight: AtomicUsize,
    signaled: AtomicBool,
    failure: AtomicI32,
    #[cfg(test)]
    before_write: Mutex<Option<tests::Gate>>,
}

impl std::fmt::Debug for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signal").finish_non_exhaustive()
    }
}

impl Signal {
    fn notify(&self) {
        if self.closed.load(SeqCst) {
            return;
        }
        self.in_flight.fetch_add(1, SeqCst);
        if !self.closed.load(SeqCst) && !self.signaled.swap(true, SeqCst) {
            self.write();
        }
        self.in_flight.fetch_sub(1, SeqCst);
    }

    fn write(&self) {
        #[cfg(test)]
        tests::pause(&self.before_write);
        if let Err(error) = self.primitive.signal() {
            self.fail(error.raw_os_error().unwrap_or(-2));
        }
    }

    fn fail(&self, error: i32) {
        let _ = self.failure.compare_exchange(0, error, SeqCst, SeqCst);
        // Failure wakeups obey the same close gate as ordinary source signals.
        if self.closed.load(SeqCst) {
            return;
        }
        self.in_flight.fetch_add(1, SeqCst);
        if !self.closed.load(SeqCst) {
            self.primitive.wake_failure();
        }
        self.in_flight.fetch_sub(1, SeqCst);
    }

    fn check(&self) -> Result<()> {
        if self.failure.load(SeqCst) != 0 {
            Err(Error::new(
                ErrorKind::Internal,
                "notification signaling failed; inspect context status",
            ))
        } else {
            Ok(())
        }
    }
}

struct Context {
    table: Mutex<Table>,
    collecting: AtomicBool,
    signal: Arc<Signal>,
    #[cfg(test)]
    after_subscribe: Mutex<Option<tests::Gate>>,
    #[cfg(test)]
    after_drain: Mutex<Option<tests::Gate>>,
}

// Reject overlapping collections even while a visitor runs without the table lock.
// Unwinding a visitor must also release collection ownership.
struct CollectionGuard<'a>(&'a AtomicBool);

impl<'a> CollectionGuard<'a> {
    fn new(collecting: &'a AtomicBool) -> Result<Self> {
        collecting
            .compare_exchange(false, true, SeqCst, SeqCst)
            .map_err(|_| state_error("another notification collection is active"))?;
        Ok(Self(collecting))
    }
}

impl Drop for CollectionGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, SeqCst);
    }
}

impl Context {
    fn close(&self) {
        if self.signal.closed.swap(true, SeqCst) {
            return;
        }
        // Detach and release potentially cancellation-bearing observers outside the table lock.
        let registrations: Vec<_> = lock(&self.table)
            .slots
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        for registration in registrations {
            registration.detach();
        }
        self.signal.write();
        self.signal.close_done.store(true, SeqCst);
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.close();
    }
}

/// Explicitly owned collection of bounded source registrations and one OS wake primitive.
#[derive(Clone)]
pub struct NotificationContext(Arc<Context>);

impl NotificationContext {
    /// Creates a context, allocating all registration slots before publishing it.
    ///
    /// # Errors
    /// Returns configuration/allocation or OS-resource errors.
    pub fn new(capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(Error::configuration(
                "notification capacity must be nonzero",
            ));
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| Error::configuration("notification capacity is too large"))?;
        slots.resize_with(capacity, || None);
        let primitive = Primitive::new()
            .map_err(|_| Error::configuration("notification adapter unavailable"))?;
        Ok(Self(Arc::new(Context {
            table: Mutex::new(Table {
                slots,
                next_generation: 1,
                cursor: 0,
            }),
            collecting: AtomicBool::new(false),
            signal: Arc::new(Signal {
                primitive,
                closed: AtomicBool::new(false),
                close_done: AtomicBool::new(false),
                in_flight: AtomicUsize::new(0),
                signaled: AtomicBool::new(false),
                failure: AtomicI32::new(0),
                #[cfg(test)]
                before_write: Mutex::new(None),
            }),
            #[cfg(test)]
            after_subscribe: Mutex::new(None),
            #[cfg(test)]
            after_drain: Mutex::new(None),
        })))
    }

    fn table(&self) -> Result<MutexGuard<'_, Table>> {
        self.0
            .table
            .try_lock()
            .map_err(|_| state_error("another notification administration/drain call is active"))
    }

    fn open(&self) -> Result<()> {
        if self.0.signal.closed.load(SeqCst) {
            return Err(state_error("notification context is closing"));
        }
        self.0.signal.check()
    }

    fn register(&self, observation: Observation, tag: u64) -> Result<RegistrationKey> {
        let mut table = self.table()?;
        self.open()?;
        let slot = table
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Backpressure,
                    "notification registration capacity reached",
                )
            })?;
        let generation = table.next_generation;
        table.next_generation = generation.checked_add(1).ok_or_else(|| {
            self.0.signal.fail(-1);
            state_error("notification generations exhausted")
        })?;
        let key = RegistrationKey {
            slot: u64::try_from(slot).expect("slot fits u64"),
            generation,
        };
        let target = Arc::new(Target {
            signal: Arc::downgrade(&self.0.signal),
            active: AtomicBool::new(true),
            published: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
        });
        let source = observation.source().clone();
        source.subscribe(&target)?;
        #[cfg(test)]
        tests::pause(&self.0.after_subscribe);
        // Install first, then check: a concurrent publication either visits our subscription or
        // is visible here. No source-result lock is taken while updating subscriber snapshots.
        let revision = source.revision.load(SeqCst);
        if (source.kind != SourceKind::Events && revision != 0)
            || source.available()
            || source.invalidated.load(SeqCst)
        {
            target.published.fetch_max(revision.max(1), SeqCst);
            self.0.signal.notify();
        }
        table.slots[slot] = Some(Registration {
            key,
            tag,
            target,
            source,
            _observation: observation,
        });
        drop(table);
        Ok(key)
    }

    /// Registers client events without taking event-consumer authority.
    /// # Errors
    /// Returns context state, capacity, or source subscriber-limit errors.
    pub fn register_events(&self, source: &EventReadiness, tag: u64) -> Result<RegistrationKey> {
        self.register(Observation::Events(source.clone()), tag)
    }

    /// Registers a repeatable terminal MQTT result; dropping registration never cancels MQTT.
    /// # Errors
    /// Returns context state, capacity, or source subscriber-limit errors.
    pub fn register_completion(
        &self,
        source: &CompletionHandle,
        tag: u64,
    ) -> Result<RegistrationKey> {
        self.register(Observation::Completion(source.clone()), tag)
    }

    /// Registers the immutable first-connection result without retaining client execution.
    /// Dropping the registration never cancels startup.
    /// # Errors
    /// Returns context state, capacity, or source subscriber-limit errors.
    pub fn register_first_connection(
        &self,
        source: &ConnectionHandle,
        tag: u64,
    ) -> Result<RegistrationKey> {
        self.register(Observation::FirstConnection(source.clone()), tag)
    }

    /// Registers admission outcome and retains one external pending observer.
    /// # Errors
    /// Returns context state, capacity, or source subscriber-limit errors.
    pub fn register_pending_admission(
        &self,
        source: &PendingAdmissionHandle,
        tag: u64,
    ) -> Result<RegistrationKey> {
        self.register(Observation::Pending(source.clone()), tag)
    }

    /// Removes a source. Releasing the last pending observer requests existing cancellation.
    /// # Errors
    /// Returns an error for stale keys or overlapping administration calls.
    pub fn deregister(&self, key: RegistrationKey) -> Result<()> {
        let registration = {
            let mut table = self.table()?;
            let slot = find(&mut table, key)?;
            let registration = slot
                .take()
                .ok_or_else(|| state_error("notification registration is absent"))?;
            drop(table);
            registration
        };
        registration.detach();
        Ok(())
    }

    /// Visits ready hints with round-robin fairness, consuming no source observations.
    /// The visitor is invoked only by the calling host, never by MQTT workers.
    /// It runs without the table lock and may acknowledge, deregister, or request close.
    /// Closing stops further visits; the returned count includes the visitor that closed.
    /// Nested or concurrent collection is rejected. Each call scans at most one table pass.
    /// # Errors
    /// Returns a context state or overlapping-drainer error.
    pub fn collect(&self, limit: usize, mut visit: impl FnMut(ReadyEntry)) -> Result<usize> {
        let _collection = CollectionGuard::new(&self.0.collecting)?;
        let mut table = self.table()?;
        self.open()?;
        let capacity = table.slots.len();
        let mut written = 0;
        for scanned in 0..capacity {
            if written == limit {
                break;
            }
            let slot = table.cursor;
            table.cursor = (slot + 1) % capacity;
            if let Some(entry) = table.slots[slot].as_ref().and_then(Registration::entry) {
                drop(table);
                visit(entry);
                written += 1;
                if written == limit || scanned + 1 == capacity {
                    return Ok(written);
                }
                table = self.table()?;
                if self.0.signal.closed.load(SeqCst) {
                    return Ok(written);
                }
                self.0.signal.check()?;
            }
        }
        drop(table);
        Ok(written)
    }

    /// Acknowledges only the observed revision; newer publications remain discoverable.
    /// # Errors
    /// Returns an error for stale keys, fabricated revisions, or overlapping drainer calls.
    pub fn acknowledge(&self, key: RegistrationKey, revision: u64) -> Result<()> {
        let mut table = self.table()?;
        self.open()?;
        let registration = find(&mut table, key)?
            .as_ref()
            .ok_or_else(|| state_error("notification registration is absent"))?;
        let source = &registration.source;
        let published = registration
            .target
            .published
            .load(SeqCst)
            .max(source.revision.load(SeqCst))
            .max(u64::from(
                source.kind == SourceKind::Events && source.available(),
            ));
        if revision == 0 || revision > published {
            return Err(state_error("notification revision is invalid"));
        }
        registration.target.acknowledged.fetch_max(revision, SeqCst);
        drop(table);
        Ok(())
    }

    /// Drains/resets OS signaling, then checks durable readiness before the host sleeps.
    /// `true` means service/retry immediately, including when a bounded drain has more to do.
    /// # Errors
    /// Returns context state, signaling, or overlapping-drainer errors.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "Serialize the full drain/check/signal transaction"
    )]
    pub fn prepare_wait(&self) -> Result<bool> {
        let table = self.table()?;
        self.open()?;
        let drained = self.0.signal.primitive.drain().map_err(|error| {
            self.0.signal.fail(error.raw_os_error().unwrap_or(-2));
            Error::new(ErrorKind::Internal, "notification drain failed")
        })?;
        #[cfg(test)]
        tests::pause(&self.0.after_drain);
        self.0.signal.signaled.store(false, SeqCst);
        let ready = !drained
            || table
                .slots
                .iter()
                .flatten()
                .any(|registration| registration.entry().is_some());
        if ready {
            self.0.signal.notify();
        }
        self.open()?;
        Ok(ready)
    }

    /// Starts close, deregistering sources while leaving exported resources valid.
    pub fn request_close(&self) {
        self.0.close();
    }

    /// Samples close and signal quiescence without waiting.
    #[must_use]
    pub fn state(&self) -> NotificationState {
        if !self.0.signal.closed.load(SeqCst) {
            NotificationState::Open
        } else if self.0.signal.close_done.load(SeqCst) && self.0.signal.in_flight.load(SeqCst) == 0
        {
            NotificationState::Quiescent
        } else {
            NotificationState::Closing
        }
    }

    /// Captures administrative state/usage under one guard; signal failure is independently sampled.
    /// # Errors
    /// Returns an overlapping-administration error.
    pub fn status(&self) -> Result<NotificationStatus> {
        let table = self.table()?;
        Ok(NotificationStatus {
            state: self.state(),
            signal_error: self.signal_error(),
            registrations: table.slots.iter().flatten().count(),
            capacity: table.slots.len(),
        })
    }

    /// Sticky OS error, -1 for counter exhaustion, or -2 for an unclassified I/O failure.
    #[must_use]
    pub fn signal_error(&self) -> i32 {
        self.0.signal.failure.load(SeqCst)
    }

    /// Captures registration usage. May be called after close.
    /// # Errors
    /// Returns an overlapping-administration error.
    pub fn usage(&self) -> Result<(usize, usize)> {
        let table = self.table()?;
        Ok((table.slots.iter().flatten().count(), table.slots.len()))
    }

    /// Borrows the readable descriptor until final context release. Wait-only; do not read it.
    #[cfg(unix)]
    #[must_use]
    pub fn borrowed_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.signal.primitive.borrowed_fd()
    }

    /// Duplicates the wait descriptor. Its owner must remove it from wait sets before closing it.
    /// # Errors
    /// Returns an OS duplication error.
    #[cfg(unix)]
    pub fn duplicate_fd(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        self.borrowed_fd().try_clone_to_owned()
    }

    /// Borrows a Windows waitable event until final context release. Wait-only.
    #[cfg(windows)]
    #[must_use]
    pub fn borrowed_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.0.signal.primitive.borrowed_handle()
    }

    /// Duplicates the waitable event with synchronization-only access.
    /// # Errors
    /// Returns an OS duplication error.
    #[cfg(windows)]
    pub fn duplicate_handle(&self) -> std::io::Result<std::os::windows::io::OwnedHandle> {
        self.0.signal.primitive.duplicate_handle()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn find(table: &mut Table, key: RegistrationKey) -> Result<&mut Option<Registration>> {
    let slot = usize::try_from(key.slot)
        .ok()
        .and_then(|slot| table.slots.get_mut(slot))
        .ok_or_else(|| state_error("notification registration key is invalid"))?;
    if slot
        .as_ref()
        .is_none_or(|registration| registration.key != key)
    {
        return Err(state_error("notification registration key is stale"));
    }
    Ok(slot)
}

fn state_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Admission, message).with_code(ErrorCode::InvalidState)
}

#[cfg(test)]
mod tests;
