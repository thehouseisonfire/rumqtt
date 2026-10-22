//! Bounded, driver-owned attempts to enter native admission.
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Notify;

use crate::acknowledgement::AckReservation;
use crate::handle::Shared;
use crate::{Admission, Command, DeliveryStatus, Error, ErrorKind, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingAdmissionLimits {
    pub max_pending: usize,
    pub max_bytes: usize,
}

impl Default for PendingAdmissionLimits {
    fn default() -> Self {
        Self {
            max_pending: 1024,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PendingAdmissionSnapshot {
    pub pending: usize,
    pub retained_bytes: usize,
    pub limits: PendingAdmissionLimits,
}

#[derive(Clone, Debug)]
pub enum PendingAdmissionOutcome {
    Admitted(Admission),
    Cancelled,
    Failed(Error),
}

struct Cell {
    outcome: Mutex<Option<PendingAdmissionOutcome>>,
    changed: Condvar,
    wake: Notify,
    ready: Notify,
    #[cfg(feature = "readiness")]
    readiness: std::sync::OnceLock<Arc<crate::readiness::Source>>,
    cancel: AtomicBool,
    owners: AtomicUsize,
    #[cfg(test)]
    after_attempt: Mutex<Option<(flume::Sender<()>, flume::Receiver<()>)>>,
}

impl Cell {
    fn finish(&self, outcome: PendingAdmissionOutcome) {
        let mut state = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.is_none() {
            *state = Some(outcome);
            drop(state);
            self.changed.notify_all();
            self.ready.notify_waiters();
            #[cfg(feature = "readiness")]
            if let Some(readiness) = self.readiness.get() {
                readiness.complete();
            }
        }
    }
}

/// An external observer; task ownership is deliberately excluded from owner counting.
pub struct PendingAdmissionHandle(Arc<Cell>);

impl Clone for PendingAdmissionHandle {
    fn clone(&self) -> Self {
        self.0.owners.fetch_add(1, Ordering::Relaxed);
        Self(self.0.clone())
    }
}

impl Drop for PendingAdmissionHandle {
    fn drop(&mut self) {
        if self.0.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.cancel();
        }
    }
}

impl PendingAdmissionHandle {
    #[cfg(feature = "readiness")]
    pub(crate) fn notification_source(&self) -> &Arc<crate::readiness::Source> {
        let source = self.0.readiness.get_or_init(|| {
            crate::readiness::Source::new(crate::readiness::SourceKind::PendingAdmission)
        });
        if self.poll().is_some() {
            source.complete();
        }
        source
    }

    #[cfg(test)]
    pub(crate) fn after_attempt(&self, entered: flume::Sender<()>, release: flume::Receiver<()>) {
        *self.0.after_attempt.lock().unwrap() = Some((entered, release));
    }
    /// Requests cancellation. Only a Cancelled outcome proves non-admission.
    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Release);
        self.0.wake.notify_one();
        self.0.ready.notify_waiters();
    }

    #[must_use]
    pub fn cancellation_requested(&self) -> bool {
        self.0.cancel.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn poll(&self) -> Option<PendingAdmissionOutcome> {
        self.0
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Observes the terminal outcome without blocking execution or cancelling work.
    pub async fn wait_async(&self) -> PendingAdmissionOutcome {
        loop {
            let ready = self.0.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if let Some(outcome) = self.poll() {
                return outcome;
            }
            ready.await;
        }
    }

    /// A deadline ends this observation only.
    /// # Errors
    /// Returns a timeout or rejects blocking on execution workers/host callbacks.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<PendingAdmissionOutcome> {
        crate::execution::check_wait(timeout)?;
        let started = Instant::now();
        let mut state = self
            .0
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(outcome) = state.as_ref() {
                return Ok(outcome.clone());
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "pending admission observation timed out",
                ));
            }
            state = self
                .0
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

struct Budget(Mutex<PendingAdmissionSnapshot>);

/// Reserve before constructing owned command inputs. Dropping rolls back construction.
pub struct PendingAdmissionReservation {
    budget: Arc<Budget>,
    bytes: usize,
    pub(crate) epoch: u64,
}

impl Drop for PendingAdmissionReservation {
    fn drop(&mut self) {
        let mut budget = self
            .budget
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.pending -= 1;
        budget.retained_bytes -= self.bytes;
    }
}

pub struct Registry {
    budget: Arc<Budget>,
    sender: flume::Sender<Job>,
    receiver: Mutex<Option<flume::Receiver<Job>>>,
    terminal: Arc<Mutex<Option<Error>>>,
    changed: Notify,
}

impl Registry {
    pub(crate) fn new() -> Self {
        let limits = PendingAdmissionLimits::default();
        let (sender, receiver) = flume::bounded(limits.max_pending);
        Self {
            budget: Arc::new(Budget(Mutex::new(PendingAdmissionSnapshot {
                pending: 0,
                retained_bytes: 0,
                limits,
            }))),
            sender,
            receiver: Mutex::new(Some(receiver)),
            terminal: Arc::new(Mutex::new(None)),
            changed: Notify::new(),
        }
    }

    // Startup only, before handles are published. The count reservation bounds the inbox too.
    pub(crate) fn set_limits(&mut self, limits: PendingAdmissionLimits) {
        let (sender, receiver) = flume::bounded(limits.max_pending);
        self.sender = sender;
        *self.receiver.get_mut().unwrap() = Some(receiver);
        self.budget.0.lock().unwrap().limits = limits;
    }

    pub(crate) fn snapshot(&self) -> PendingAdmissionSnapshot {
        *self
            .budget
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reserve(&self, bytes: usize, epoch: u64) -> Result<PendingAdmissionReservation> {
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = terminal.as_ref() {
            return Err(error.clone());
        }
        drop(terminal);
        let mut budget = self
            .budget
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes > budget.limits.max_bytes {
            return Err(invalid("request exceeds the pending admission byte limit"));
        }
        if budget.pending >= budget.limits.max_pending
            || bytes > budget.limits.max_bytes - budget.retained_bytes
        {
            return Err(Error::new(
                ErrorKind::Backpressure,
                "pending admission capacity exhausted",
            )
            .with_retryable(true)
            .with_delivery(DeliveryStatus::NotAdmitted));
        }
        budget.pending += 1;
        budget.retained_bytes += bytes;
        drop(budget);
        Ok(PendingAdmissionReservation {
            budget: self.budget.clone(),
            bytes,
            epoch,
        })
    }

    pub(crate) fn validate_reservation(
        &self,
        reservation: &PendingAdmissionReservation,
        command: &Command,
    ) -> Result<()> {
        if !Arc::ptr_eq(&self.budget, &reservation.budget)
            || command_charge(command)? > reservation.bytes
        {
            return Err(invalid(
                "pending reservation does not cover this command/client",
            ));
        }
        Ok(())
    }

    pub(crate) fn register(&self, mut job: Job) -> Result<PendingAdmissionHandle> {
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = terminal.as_ref() {
            return Err(error.clone());
        }
        job.terminal = Some(self.terminal.clone());
        let handle = PendingAdmissionHandle(job.cell.clone());
        let sent = self.sender.try_send(job);
        drop(terminal);
        // A rejected Job's destructor also observes terminal state.
        sent.map_err(|_| invalid("admission owner is unavailable"))?;
        Ok(handle)
    }

    pub(crate) fn close(&self, error: Error) {
        self.terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert(error.with_delivery(DeliveryStatus::NotAdmitted));
        self.changed.notify_one();
    }

    pub(crate) fn runner(shared: Arc<Shared>) -> Runner {
        let receiver = shared
            .pending
            .receiver
            .lock()
            .unwrap()
            .take()
            .expect("one admission owner");
        Runner { shared, receiver }
    }
}

pub struct Job {
    // Drop storage before releasing its reservation.
    command: Option<Command>,
    ack: Option<AckReservation>,
    reservation: Option<PendingAdmissionReservation>,
    cell: Arc<Cell>,
    fallback: Error,
    terminal: Option<Arc<Mutex<Option<Error>>>>,
}

impl Job {
    pub(crate) fn new(
        command: Command,
        ack: Option<AckReservation>,
        reservation: PendingAdmissionReservation,
    ) -> Self {
        Self {
            command: Some(command),
            ack,
            reservation: Some(reservation),
            cell: Arc::new(Cell {
                outcome: Mutex::new(None),
                changed: Condvar::new(),
                wake: Notify::new(),
                ready: Notify::new(),
                #[cfg(feature = "readiness")]
                readiness: std::sync::OnceLock::new(),
                cancel: AtomicBool::new(false),
                owners: AtomicUsize::new(1),
                #[cfg(test)]
                after_attempt: Mutex::new(None),
            }),
            fallback: Error::new(ErrorKind::Internal, "admission owner stopped")
                .with_delivery(DeliveryStatus::NotAdmitted),
            terminal: None,
        }
    }

    fn finish(&mut self, outcome: PendingAdmissionOutcome) {
        self.command.take();
        self.ack.take();
        self.reservation.take();
        self.cell.finish(outcome);
    }

    async fn run(mut self, shared: Arc<Shared>, budget: Arc<AtomicUsize>) {
        let cell = self.cell.clone();
        loop {
            let native = shared.backend().publish_waiter();
            let progress = shared.shutdown_progress();
            tokio::pin!(progress);
            progress.as_mut().enable();
            let cancel = cell.wake.notified();
            tokio::pin!(cancel);
            cancel.as_mut().enable();
            poll_fn(|cx| {
                if budget
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                    .is_ok()
                {
                    std::task::Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
            if self.cell.cancel.load(Ordering::Acquire) {
                self.finish(PendingAdmissionOutcome::Cancelled);
                return;
            }
            let result = shared
                .try_deferred(
                    self.reservation.as_ref().unwrap().epoch,
                    self.command.as_ref().unwrap(),
                    &mut self.ack,
                )
                .map_err(|error| shared.contextualize(error));
            #[cfg(test)]
            let hook = cell.after_attempt.lock().unwrap().take();
            #[cfg(test)]
            if let Some((entered, release)) = hook {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            match result {
                Err(error)
                    if error.kind() == ErrorKind::Backpressure
                        && error.recovery_failure().is_none() => {}
                result => {
                    let outcome = match result {
                        Ok(admission) => PendingAdmissionOutcome::Admitted(admission),
                        Err(error) => PendingAdmissionOutcome::Failed(error),
                    };
                    self.finish(outcome);
                    return;
                }
            }
            if let Some(native) = native {
                tokio::select! { () = native.wait_async() => {}, () = &mut progress => {}, () = &mut cancel => {} }
            } else {
                tokio::select! { () = &mut progress => {}, () = &mut cancel => {} }
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let error = self
            .terminal
            .as_ref()
            .and_then(|terminal| {
                terminal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_else(|| self.fallback.clone());
        self.finish(PendingAdmissionOutcome::Failed(error));
    }
}

pub struct Runner {
    shared: Arc<Shared>,
    receiver: flume::Receiver<Job>,
}

impl Runner {
    pub(crate) async fn run(self) {
        let budget = Arc::new(AtomicUsize::new(64));
        let work = self.run_inner(budget.clone());
        tokio::pin!(work);
        poll_fn(|cx| {
            budget.store(64, Ordering::Relaxed);
            work.as_mut().poll(cx)
        })
        .await;
    }

    async fn run_inner(&self, budget: Arc<AtomicUsize>) {
        let mut active = FuturesUnordered::<Pin<Box<dyn Future<Output = ()> + Send>>>::new();
        loop {
            let changed = self.shared.pending.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.shared.pending.terminal.lock().unwrap().is_some() {
                break;
            }
            tokio::select! {
                () = &mut changed => {},
                job = self.receiver.recv_async() => match job {
                    Ok(job) => active.push(Box::pin(job.run(self.shared.clone(), budget.clone()))),
                    Err(_) => break,
                },
                _ = active.next(), if !active.is_empty() => {},
            }
            tokio::task::yield_now().await;
        }
        // Destruction of active futures restores ACKs and releases command storage.
        drop(active);
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let error = self
            .shared
            .pending
            .terminal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| {
                Error::new(ErrorKind::Internal, "admission execution cancelled")
                    .with_delivery(DeliveryStatus::NotAdmitted)
            });
        self.shared.pending.close(error.clone());
        while let Ok(mut job) = self.receiver.try_recv() {
            job.fallback = error.clone();
            drop(job);
        }
    }
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::Admission, message).with_delivery(DeliveryStatus::NotAdmitted)
}

/// Logical command storage, including variable collection metadata. Does not measure RSS.
pub fn command_charge(command: &Command) -> Result<usize> {
    fn properties(values: &[(String, String)]) -> Option<usize> {
        values.iter().try_fold(0usize, |n, (k, v)| {
            n.checked_add(size_of::<(String, String)>())?
                .checked_add(k.len())?
                .checked_add(v.len())
        })
    }
    let bytes = match command {
        Command::Publish(p) => {
            let mut n = p.topic.len().checked_add(p.payload.len());
            if let crate::PublishProtocolOptions::V5(p) = &p.protocol {
                n = n
                    .and_then(|n| n.checked_add(p.response_topic.as_ref().map_or(0, String::len)))
                    .and_then(|n| {
                        n.checked_add(p.correlation_data.as_ref().map_or(0, bytes::Bytes::len))
                    })
                    .and_then(|n| n.checked_add(p.content_type.as_ref().map_or(0, String::len)))
                    .and_then(|n| n.checked_add(properties(&p.user_properties)?));
            }
            n
        }
        Command::Subscribe(p) => {
            let n = p.filters.iter().try_fold(0usize, |n, f| {
                n.checked_add(size_of::<crate::Subscription>())?
                    .checked_add(f.filter.len())
            });
            match &p.protocol {
                crate::SubscribeProtocolOptions::V5(p) => {
                    n.and_then(|n| n.checked_add(properties(&p.user_properties)?))
                }
                crate::SubscribeProtocolOptions::VersionNeutral => n,
            }
        }
        Command::Unsubscribe(p) => {
            let n = p.filters.iter().try_fold(0usize, |n, f| {
                n.checked_add(size_of::<String>())?.checked_add(f.len())
            });
            match &p.protocol {
                crate::UnsubscribeProtocolOptions::V5(p) => {
                    n.and_then(|n| n.checked_add(properties(&p.user_properties)?))
                }
                crate::UnsubscribeProtocolOptions::VersionNeutral => n,
            }
        }
        Command::Acknowledge(_) => Some(0),
        Command::AcknowledgeWithOptions { protocol, .. } => match protocol {
            crate::AcknowledgementProtocolOptions::VersionNeutral => Some(0),
            crate::AcknowledgementProtocolOptions::V5(p) => properties(&p.user_properties)
                .and_then(|n| n.checked_add(p.reason_string.as_ref().map_or(0, String::len)))
                // The command and prepared reservation each own the selected options.
                .and_then(|n| n.checked_mul(2)),
        },
        _ => return Err(invalid("command does not support deferred admission")),
    };
    bytes.ok_or_else(|| invalid("pending command size overflow"))
}

pub fn validate_command(command: &Command, backend: &crate::backend::BackendClient) -> Result<()> {
    use crate::{
        PublishProtocolOptions as P, SubscribeProtocolOptions as S, UnsubscribeProtocolOptions as U,
    };
    let v4 = matches!(backend, crate::backend::BackendClient::V4(_));
    match command {
        Command::Publish(p) => {
            crate::validation::validate_mqtt_utf8_string(&p.topic, "publish topic")?;
            let alias = matches!(&p.protocol, P::V5(p) if p.topic_alias.is_some());
            if p.topic.contains(['+', '#']) || (p.topic.is_empty() && !alias) {
                return Err(invalid("invalid publish topic"));
            }
            if v4 && !matches!(p.protocol, P::VersionNeutral) {
                return Err(invalid("MQTT 5 properties require MQTT 5"));
            }
            crate::backend::v5::validate_publish(p)?;
            if !v4 {
                let limits = backend.publish_budget_snapshot()?.limits;
                let charged = command_charge(command)?.checked_add(if p.topic.is_empty() {
                    usize::from(u16::MAX)
                } else {
                    0
                });
                if charged.is_none_or(|bytes| bytes > limits.max_bytes) {
                    return Err(invalid("publish exceeds native byte limit")
                        .with_code(crate::ErrorCode::PublishTooLarge)
                        .with_publish_failure(crate::PublishFailure::TooLarge));
                }
            }
        }
        Command::Subscribe(p) => {
            if p.filters.is_empty() {
                return Err(invalid("subscribe requires a filter"));
            }
            if v4
                && (!matches!(p.protocol, S::VersionNeutral)
                    || p.filters.iter().any(|f| {
                        !matches!(
                            f.protocol,
                            crate::SubscriptionProtocolOptions::VersionNeutral
                        )
                    }))
            {
                return Err(invalid("MQTT 5 properties require MQTT 5"));
            }
            for f in &p.filters {
                crate::validation::validate_mqtt_utf8_string(&f.filter, "topic filter")?;
                if !crate::backend::v5::valid_filter(&f.filter) {
                    return Err(invalid("invalid topic filter"));
                }
                if f.filter.starts_with("$share/")
                    && matches!(&f.protocol, crate::SubscriptionProtocolOptions::V5(p) if p.no_local)
                {
                    return Err(invalid("shared subscriptions cannot set no_local"));
                }
            }
            crate::backend::v5::validate_subscribe(p)?;
        }
        Command::Unsubscribe(p) => {
            if p.filters.is_empty() {
                return Err(invalid("unsubscribe requires a filter"));
            }
            if v4 && !matches!(p.protocol, U::VersionNeutral) {
                return Err(invalid("MQTT 5 properties require MQTT 5"));
            }
            for f in &p.filters {
                crate::validation::validate_mqtt_utf8_string(f, "topic filter")?;
                if !crate::backend::v5::valid_filter(f) {
                    return Err(invalid("invalid topic filter"));
                }
            }
            crate::backend::v5::validate_unsubscribe(p)?;
        }
        Command::Acknowledge(_) | Command::AcknowledgeWithOptions { .. } => {} // Coordinator validates at registration.
        _ => return Err(invalid("command does not support deferred admission")),
    }
    validate_packet_size(command, v4)
}

fn validate_packet_size(command: &Command, v4: bool) -> Result<()> {
    const fn width(n: u128) -> u128 {
        match n {
            0..=127 => 1,
            128..=16_383 => 2,
            16_384..=2_097_151 => 3,
            _ => 4,
        }
    }
    fn users(p: &[(String, String)]) -> u128 {
        p.iter()
            .map(|(k, v)| 5 + k.len() as u128 + v.len() as u128)
            .sum()
    }
    fn field(s: Option<usize>) -> u128 {
        s.map_or(0, |n| 3 + n as u128)
    }
    let remaining = match command {
        Command::Publish(p) => {
            let properties = match &p.protocol {
                crate::PublishProtocolOptions::VersionNeutral => 0,
                crate::PublishProtocolOptions::V5(p) => {
                    users(&p.user_properties)
                        + field(p.response_topic.as_ref().map(String::len))
                        + field(p.correlation_data.as_ref().map(bytes::Bytes::len))
                        + field(p.content_type.as_ref().map(String::len))
                        + u128::from(p.payload_format_indicator.is_some()) * 2
                        + u128::from(p.topic_alias.is_some()) * 3
                        + u128::from(p.message_expiry_interval.is_some()) * 5
                }
            };
            2 + p.topic.len() as u128
                + p.payload.len() as u128
                + u128::from(p.qos != crate::QoS::AtMostOnce) * 2
                + if v4 {
                    0
                } else {
                    properties + width(properties)
                }
        }
        Command::Subscribe(p) => {
            let properties = match &p.protocol {
                crate::SubscribeProtocolOptions::VersionNeutral => 0,
                crate::SubscribeProtocolOptions::V5(p) => {
                    users(&p.user_properties)
                        + p.subscription_identifier
                            .map_or(0, |id| 1 + width(id as u128))
                }
            };
            2 + if v4 {
                0
            } else {
                properties + width(properties)
            } + p
                .filters
                .iter()
                .map(|f| 3 + f.filter.len() as u128)
                .sum::<u128>()
        }
        Command::Unsubscribe(p) => {
            let properties = match &p.protocol {
                crate::UnsubscribeProtocolOptions::VersionNeutral => 0,
                crate::UnsubscribeProtocolOptions::V5(p) => users(&p.user_properties),
            };
            2 + if v4 {
                0
            } else {
                properties + width(properties)
            } + p.filters.iter().map(|f| 2 + f.len() as u128).sum::<u128>()
        }
        _ => return Ok(()),
    };
    if remaining > 268_435_455 {
        return Err(invalid("command exceeds the MQTT remaining-length limit"));
    }
    Ok(())
}

#[cfg(all(test, feature = "readiness"))]
mod readiness_tests {
    use super::*;
    use crate::readiness::NotificationContext;

    #[test]
    fn registration_retains_pending_observer_and_final_removal_requests_cancellation() {
        let registry = Registry::new();
        let job = Job::new(Command::Diagnostics, None, registry.reserve(0, 0).unwrap());
        let pending = registry.register(job).unwrap();
        let cell = Arc::downgrade(&pending.0);
        let context = NotificationContext::new(1).unwrap();
        let key = context.register_pending_admission(&pending, 1).unwrap();
        drop(pending);
        assert!(!cell.upgrade().unwrap().cancel.load(Ordering::Acquire));
        assert!(!context.prepare_wait().unwrap());
        context.deregister(key).unwrap();
        assert!(cell.upgrade().unwrap().cancel.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_request_is_not_readiness_and_terminal_results_survive_registry_drop() {
        let registry = Registry::new();
        let job = Job::new(Command::Diagnostics, None, registry.reserve(0, 0).unwrap());
        let pending = registry.register(job).unwrap();
        let context = NotificationContext::new(1).unwrap();
        let key = context.register_pending_admission(&pending, 1).unwrap();
        pending.cancel();
        assert!(!context.prepare_wait().unwrap());
        drop(registry);
        assert!(context.prepare_wait().unwrap());
        let mut revision = 0;
        context
            .collect(1, |entry| revision = entry.revision)
            .unwrap();
        context.acknowledge(key, revision).unwrap();
        assert!(!context.prepare_wait().unwrap());
        assert!(matches!(
            pending.poll(),
            Some(PendingAdmissionOutcome::Failed(_))
        ));
    }
}
