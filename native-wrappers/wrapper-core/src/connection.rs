use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::{Error, ProtocolVersion, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionResult {
    pub protocol: ProtocolVersion,
    pub session_present: bool,
}

/// Outcome of a bounded observation, independent of the retained connection result.
#[derive(Clone, Debug)]
pub enum ConnectionWaitOutcome {
    /// The immutable first success or terminal failure before success.
    Completed(Result<ConnectionResult>),
    /// This wait expired; connection attempts are unaffected.
    DeadlineElapsed,
    /// Blocking observation was forbidden; the connection result is unaffected.
    ObservationRejected(Error),
}

#[derive(Clone, Debug)]
enum ConnectionState {
    Pending,
    Connected(ConnectionResult),
    Terminal(Error),
}

impl ConnectionState {
    fn observe(&self) -> Option<Result<ConnectionResult>> {
        match self {
            Self::Pending => None,
            Self::Connected(result) => Some(Ok(*result)),
            Self::Terminal(error) => Some(Err(error.clone())),
        }
    }
}

#[derive(Debug)]
struct ConnectionCell {
    state: Mutex<ConnectionState>,
    completed: Condvar,
    notified: Notify,
    #[cfg(feature = "readiness")]
    readiness: std::sync::OnceLock<Arc<crate::readiness::Source>>,
}

/// Cloneable, repeatably observable result of the first successful connection.
///
/// Recoverable connection-attempt failures deliberately leave this handle pending. Once a
/// connection succeeds, every current and future observer receives the same result. Terminal
/// shutdown or driver failure wakes observers with the corresponding error.
#[derive(Clone, Debug)]
pub struct ConnectionHandle {
    cell: Arc<ConnectionCell>,
}

impl ConnectionHandle {
    pub(crate) fn new() -> Self {
        Self {
            cell: Arc::new(ConnectionCell {
                state: Mutex::new(ConnectionState::Pending),
                completed: Condvar::new(),
                notified: Notify::new(),
                #[cfg(feature = "readiness")]
                readiness: std::sync::OnceLock::new(),
            }),
        }
    }

    pub(crate) fn connected(&self, result: ConnectionResult) {
        self.publish(ConnectionState::Connected(result));
    }

    pub(crate) fn terminate(&self, error: Error) {
        self.publish(ConnectionState::Terminal(error));
    }

    fn publish(&self, result: ConnectionState) {
        let mut state = self
            .cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(*state, ConnectionState::Pending) {
            return;
        }
        *state = result;
        drop(state);
        self.cell.completed.notify_all();
        self.cell.notified.notify_waiters();
        #[cfg(feature = "readiness")]
        if let Some(source) = self.cell.readiness.get() {
            source.complete();
        }
    }

    fn observe(&self) -> Option<Result<ConnectionResult>> {
        self.cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe()
    }

    #[cfg(feature = "readiness")]
    pub(crate) fn notification_source(&self) -> &Arc<crate::readiness::Source> {
        let source = self.cell.readiness.get_or_init(|| {
            crate::readiness::Source::new(crate::readiness::SourceKind::FirstConnection)
        });
        if self.observe().is_some() {
            source.complete();
        }
        source
    }

    /// Blocks for at most `timeout`, distinguishing a caller deadline from a terminal error.
    /// Nonzero waits on execution workers or in host callbacks are rejected even after completion.
    /// Zero-time waits only observe; dropping or timing out a waiter never cancels startup.
    #[must_use]
    pub fn wait_timeout_outcome(&self, timeout: Duration) -> ConnectionWaitOutcome {
        if let Err(error) = crate::execution::check_wait(timeout) {
            return ConnectionWaitOutcome::ObservationRejected(error);
        }
        let started = Instant::now();
        let mut state = self
            .cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(result) = state.observe() {
                return ConnectionWaitOutcome::Completed(result);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return ConnectionWaitOutcome::DeadlineElapsed;
            }
            let (next, _) = self
                .cell
                .completed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
    }

    #[must_use]
    pub fn try_wait(&self) -> Option<Result<ConnectionResult>> {
        self.observe()
    }

    /// Waits for the first successful connection or terminal driver failure.
    ///
    /// # Errors
    ///
    /// Returns the terminal driver error if the client stops before establishing a connection.
    pub async fn wait_async(&self) -> Result<ConnectionResult> {
        loop {
            let notified = self.cell.notified.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.observe() {
                return result;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ErrorKind};

    #[tokio::test]
    async fn first_connection_is_repeatable() {
        let handle = ConnectionHandle::new();
        let result = ConnectionResult {
            protocol: ProtocolVersion::V5,
            session_present: true,
        };
        handle.connected(result);
        assert_eq!(handle.wait_async().await.unwrap(), result);
        assert_eq!(handle.clone().wait_async().await.unwrap(), result);
    }

    #[tokio::test]
    async fn terminal_failure_wakes_waiters() {
        let handle = ConnectionHandle::new();
        handle.terminate(Error::new(ErrorKind::Internal, "failed"));
        let error = handle.wait_async().await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::Internal);
    }
    const DEADLINE: Duration = Duration::from_secs(5);

    fn result() -> ConnectionResult {
        ConnectionResult {
            protocol: ProtocolVersion::V5,
            session_present: true,
        }
    }

    fn completed(outcome: ConnectionWaitOutcome) -> Result<ConnectionResult> {
        match outcome {
            ConnectionWaitOutcome::Completed(result) => result,
            other => panic!("unexpected observation: {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocking_and_async_waiters_share_immutable_success_or_failure() {
        for success in [true, false] {
            let handle = ConnectionHandle::new();
            let mut threads = Vec::new();
            let mut tasks = Vec::new();
            let barrier = Arc::new(std::sync::Barrier::new(5));
            for _ in 0..4 {
                let observer = handle.clone();
                let barrier = barrier.clone();
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    completed(observer.wait_timeout_outcome(DEADLINE))
                }));
                let observer = handle.clone();
                tasks.push(tokio::spawn(async move { observer.wait_async().await }));
            }
            tokio::task::yield_now().await;
            barrier.wait();
            if success {
                handle.connected(result());
                handle.terminate(Error::new(ErrorKind::Internal, "later failure"));
                handle.connected(ConnectionResult {
                    session_present: false,
                    ..result()
                });
            } else {
                handle.terminate(Error::new(ErrorKind::Timeout, "terminal timeout"));
                handle.connected(result());
            }
            for observation in threads.into_iter().map(|thread| thread.join().unwrap()) {
                if success {
                    assert_eq!(observation.unwrap(), result());
                } else {
                    assert_eq!(observation.unwrap_err().code(), ErrorCode::Timeout);
                }
            }
            for task in tasks {
                let observation = task.await.unwrap();
                if success {
                    assert_eq!(observation.unwrap(), result());
                } else {
                    assert_eq!(observation.unwrap_err().code(), ErrorCode::Timeout);
                }
            }
            let late = completed(handle.clone().wait_timeout_outcome(Duration::ZERO));
            assert_eq!(late.is_ok(), success);
        }
    }

    #[test]
    fn deadlines_spurious_wakeups_and_terminal_timeouts_are_distinct() {
        let handle = ConnectionHandle::new();
        assert!(matches!(
            handle.wait_timeout_outcome(Duration::ZERO),
            ConnectionWaitOutcome::DeadlineElapsed
        ));
        std::thread::scope(|scope| {
            let observer = &handle;
            let waiter = scope.spawn(move || {
                let started = Instant::now();
                assert!(matches!(
                    observer.wait_timeout_outcome(Duration::from_millis(40)),
                    ConnectionWaitOutcome::DeadlineElapsed
                ));
                assert!(started.elapsed() >= Duration::from_millis(40));
            });
            for _ in 0..10 {
                handle.cell.completed.notify_all();
                std::thread::sleep(Duration::from_millis(1));
            }
            waiter.join().unwrap();
        });
        assert!(handle.try_wait().is_none());
        handle.terminate(Error::new(ErrorKind::Timeout, "terminal timeout"));
        let error = completed(handle.wait_timeout_outcome(Duration::ZERO)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::Timeout);
        assert_eq!(error.to_string(), "terminal timeout");
    }

    #[test]
    fn publication_racing_a_deadline_is_retained_for_the_next_observation() {
        for _ in 0..64 {
            let handle = ConnectionHandle::new();
            let waiting_handle = handle.clone();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let waiting = barrier.clone();
            let waiter = std::thread::spawn(move || {
                waiting.wait();
                waiting_handle.wait_timeout_outcome(Duration::ZERO)
            });
            barrier.wait();
            handle.connected(result());
            match waiter.join().unwrap() {
                ConnectionWaitOutcome::Completed(observed) => {
                    assert_eq!(observed.unwrap(), result());
                }
                ConnectionWaitOutcome::DeadlineElapsed => {}
                other @ ConnectionWaitOutcome::ObservationRejected(_) => {
                    panic!("unexpected {other:?}")
                }
            }
            assert_eq!(
                completed(handle.wait_timeout_outcome(Duration::ZERO)).unwrap(),
                result()
            );
        }
    }

    #[test]
    fn forbidden_waits_reject_before_observing_pending_or_completed_results() {
        let handle = ConnectionHandle::new();
        for finished in [false, true] {
            if finished {
                handle.connected(result());
            }
            let check = || {
                let ConnectionWaitOutcome::ObservationRejected(error) =
                    handle.wait_timeout_outcome(DEADLINE)
                else {
                    panic!("blocking observation was allowed");
                };
                assert_eq!(error.code(), ErrorCode::InvalidState);
                assert_eq!(handle.try_wait().is_some(), finished);
                match handle.wait_timeout_outcome(Duration::ZERO) {
                    ConnectionWaitOutcome::Completed(observed) => {
                        assert!(finished);
                        assert_eq!(observed.unwrap(), result());
                    }
                    ConnectionWaitOutcome::DeadlineElapsed => assert!(!finished),
                    other @ ConnectionWaitOutcome::ObservationRejected(_) => {
                        panic!("unexpected {other:?}")
                    }
                }
            };
            crate::runtime::with_host_callback(check);
            crate::execution::with_managed_thread(check);
        }
    }
}
