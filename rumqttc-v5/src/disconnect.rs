//! Ordered, process-local publish shutdown completion.

use crate::eventloop::RequestEnvelope;
use crate::{ConnectionError, PublishNoticeError, Request};
pub type TerminalCheckpoint =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ConnectionError>> + Send>>;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

#[derive(Debug, Default)]
pub struct Ledger {
    pending: AtomicUsize,
    failure: OnceLock<PublishNoticeError>,
}
impl Ledger {
    pub fn result(&self) -> Result<bool, DisconnectNoticeError> {
        // Acquire completed observations before inspecting the failure they published.
        let pending = self.pending.load(Ordering::Acquire);
        self.failure.get().map_or_else(
            || Ok(pending == 0),
            |error| Err(DisconnectNoticeError::Publish(error.clone())),
        )
    }
}

#[derive(Debug)]
pub struct Observation(Option<Arc<Ledger>>);
impl Observation {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        ledger.pending.fetch_add(1, Ordering::Relaxed);
        Self(Some(ledger))
    }
    pub fn finish(mut self, result: Result<(), PublishNoticeError>) {
        if let Some(ledger) = self.0.take() {
            if let Err(error) = result {
                let _ = ledger.failure.set(error);
            }
            ledger.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if let Some(ledger) = self.0.take() {
            let _ = ledger.failure.set(PublishNoticeError::Recv);
            ledger.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Current shutdown policy and progress. Counts are exposed separately.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShutdownPhase {
    #[default]
    Open,
    AdmittedDrain,
    Approaching,
    Draining,
    Flushing,
    Completed,
    TimedOut,
    Failed,
}

/// Terminal failure of an admitted ordered shutdown. Delivery can be ambiguous.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DisconnectNoticeError {
    #[error("total ordered shutdown deadline expired")]
    DisconnectTimeout,
    #[error("transport failed before ordered shutdown completed: {0}")]
    Transport(#[source] Arc<ConnectionError>),
    #[error("MQTT protocol failed before ordered shutdown completed: {0}")]
    Protocol(#[source] Arc<ConnectionError>),
    #[error("required session persistence failed: {0}")]
    Persistence(#[source] Arc<ConnectionError>),
    #[error("a preceding publish did not complete successfully: {0}")]
    Publish(#[source] PublishNoticeError),
    #[error("ordered shutdown superseded by immediate disconnect")]
    SupersededByImmediate,
    #[error("ordered shutdown superseded by another terminal operation")]
    Superseded,
    #[error("event loop or request receiver terminated")]
    ReceiverTerminated,
    #[error("session was reset before ordered shutdown completed")]
    SessionReset,
    #[error("ordered shutdown cannot cross a broker redirect")]
    Redirected,
    #[error("replay information is insufficient to complete ordered shutdown")]
    ReplayUnavailable,
}

impl DisconnectNoticeError {
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::DisconnectTimeout => "timeout",
            Self::Transport(_) => "transport",
            Self::Protocol(_) => "protocol",
            Self::Persistence(_) => "persistence",
            Self::Publish(_) => "publish",
            Self::SupersededByImmediate => "superseded_by_immediate",
            Self::Superseded => "superseded",
            Self::ReceiverTerminated => "receiver_terminated",
            Self::SessionReset => "session_reset",
            Self::Redirected => "redirected",
            Self::ReplayUnavailable => "replay_unavailable",
        }
    }
}

/// Completion of an ordered disconnect, independently of channel admission.
///
/// Success proves that preceding publishes reached their `QoS` milestones and
/// DISCONNECT was flushed through the transport. It does not prove server receipt
/// or application consumption. Keep polling the event loop while waiting.
/// Dropping this handle does not cancel shutdown.
#[must_use = "channel admission is not disconnect completion; await or explicitly drop this notice"]
#[derive(Debug)]
pub struct DisconnectNotice(oneshot::Receiver<Result<(), DisconnectNoticeError>>);

impl DisconnectNotice {
    /// Block until DISCONNECT is flushed or shutdown fails.
    ///
    /// # Panics
    /// Panics when called from an asynchronous execution context.
    ///
    /// # Errors
    ///
    /// Returns the terminal ordered-shutdown failure when it does not complete successfully.
    pub fn wait(self) -> Result<(), DisconnectNoticeError> {
        self.0
            .blocking_recv()
            .unwrap_or(Err(DisconnectNoticeError::ReceiverTerminated))
    }
    /// Await DISCONNECT flush or a typed terminal failure while another task
    /// continues driving the event loop.
    ///
    /// # Errors
    ///
    /// Returns the terminal ordered-shutdown failure when it does not complete successfully.
    pub async fn wait_async(self) -> Result<(), DisconnectNoticeError> {
        self.0
            .await
            .unwrap_or(Err(DisconnectNoticeError::ReceiverTerminated))
    }
}

#[derive(Debug)]
pub struct Completion(Mutex<Option<oneshot::Sender<Result<(), DisconnectNoticeError>>>>);
impl Completion {
    pub(crate) fn new() -> (Arc<Self>, DisconnectNotice) {
        let (tx, rx) = oneshot::channel();
        (Arc::new(Self(Mutex::new(Some(tx)))), DisconnectNotice(rx))
    }
    pub(crate) fn finish(&self, result: Result<(), DisconnectNoticeError>) {
        let tx = self.0.lock().unwrap().take();
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.finish(Err(DisconnectNoticeError::ReceiverTerminated));
    }
}

#[derive(Debug)]
pub struct RequestMeta {
    pub sequence: u64,
    pub closing: bool,
    pub invalid_timeout: bool,
    pub deadline: Option<Instant>,
    pub completion: Option<Arc<Completion>>,
}
impl RequestMeta {
    pub const fn new() -> Self {
        Self {
            sequence: 0,
            closing: false,
            invalid_timeout: false,
            deadline: None,
            completion: None,
        }
    }
}

impl rumqttc_core::admission::Item for RequestEnvelope {
    fn fence(&self) -> Option<Option<Duration>> {
        match self.request {
            Request::DisconnectAfterQueued(_) => Some(None),
            Request::DisconnectAfterQueuedWithTimeout(_, timeout) => Some(Some(timeout)),
            _ => None,
        }
    }
    fn admitted(&mut self, sequence: u64, deadline: Option<Instant>) {
        self.meta.sequence = sequence;
        self.meta.deadline = deadline;
    }
    fn closing(&mut self) {
        self.meta.closing = true;
    }
    fn invalid_timeout(&mut self) {
        self.meta.invalid_timeout = true;
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::*;

    #[test]
    fn replayed_notice_is_not_observed_twice() {
        let ledger = Arc::new(Ledger::default());
        let mut notice = crate::notice::PublishNoticeTx::internal();
        notice.observe(&ledger);
        notice.observe(&ledger);
        assert_eq!(ledger.pending.load(Ordering::Acquire), 1);
        notice.success(crate::notice::PublishResult::Qos0Flushed);
        assert!(matches!(ledger.result(), Ok(true)));
    }

    #[test]
    fn completed_observations_preserve_first_failure() {
        let ledger = Arc::new(Ledger::default());
        let first = Observation::new(Arc::clone(&ledger));
        let second = Observation::new(Arc::clone(&ledger));
        assert!(matches!(ledger.result(), Ok(false)));
        first.finish(Err(PublishNoticeError::ShutdownInterrupted));
        drop(second);
        assert_eq!(ledger.pending.load(Ordering::Acquire), 0);
        assert!(matches!(
            ledger.result(),
            Err(DisconnectNoticeError::Publish(
                PublishNoticeError::ShutdownInterrupted
            ))
        ));
    }

    #[test]
    fn zero_pending_observes_failure_published_by_another_thread() {
        let ledger = Arc::new(Ledger::default());
        let observation = Observation::new(Arc::clone(&ledger));
        let worker = std::thread::spawn(move || observation.finish(Err(PublishNoticeError::Recv)));
        while ledger.pending.load(Ordering::Acquire) != 0 {
            std::thread::yield_now();
        }
        assert!(matches!(
            ledger.result(),
            Err(DisconnectNoticeError::Publish(PublishNoticeError::Recv))
        ));
        worker.join().unwrap();
    }

    #[test]
    fn successful_observations_complete_exactly_once() {
        let ledger = Arc::new(Ledger::default());
        Observation::new(Arc::clone(&ledger)).finish(Ok(()));
        assert_eq!(ledger.pending.load(Ordering::Acquire), 0);
        assert!(matches!(ledger.result(), Ok(true)));
    }
}
