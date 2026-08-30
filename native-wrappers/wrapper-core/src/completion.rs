use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::{Error, ErrorKind, OperationId, QoS, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishCompletion {
    Qos0Flushed,
    Qos1Acknowledged,
    Qos2Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokerReason {
    pub code: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscribeResult {
    Granted(QoS),
    Rejected(BrokerReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeCompletion {
    pub results: Vec<SubscribeResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsubscribeResult {
    Success,
    NoSubscriptionExisted,
    Rejected(BrokerReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsubscribeCompletion {
    /// MQTT 3.1.1 has no per-filter UNSUBACK reasons.
    pub results: Option<Vec<UnsubscribeResult>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Publish(PublishCompletion),
    Subscribe(SubscribeCompletion),
    Unsubscribe(UnsubscribeCompletion),
    Acknowledged,
    Diagnostics(crate::DiagnosticsSnapshot),
    GracefulShutdown,
    ImmediateShutdown,
}

/// Result of waiting for an operation until a caller-supplied deadline.
#[derive(Clone, Debug)]
pub enum CompletionWaitOutcome {
    /// The operation reached a terminal state, successfully or with an error.
    Completed(Result<Completion>),
    /// The operation was still pending when the wait deadline elapsed.
    DeadlineElapsed,
}

#[derive(Debug)]
pub struct CompletionCell {
    operation_id: OperationId,
    result: Mutex<Option<Result<Completion>>>,
    completed: Condvar,
    notified: Notify,
}

impl CompletionCell {
    pub(crate) fn new(operation_id: OperationId) -> Arc<Self> {
        Arc::new(Self {
            operation_id,
            result: Mutex::new(None),
            completed: Condvar::new(),
            notified: Notify::new(),
        })
    }

    pub(crate) fn complete(&self, result: Result<Completion>) -> bool {
        let mut state = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.is_some() {
            return false;
        }
        *state = Some(result);
        drop(state);
        self.completed.notify_all();
        self.notified.notify_waiters();
        true
    }

    fn observe(&self) -> Option<Result<Completion>> {
        self.result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Clone, Debug)]
pub struct CompletionHandle {
    cell: Arc<CompletionCell>,
}

impl CompletionHandle {
    pub(crate) const fn new(cell: Arc<CompletionCell>) -> Self {
        Self { cell }
    }

    #[must_use]
    pub fn operation_id(&self) -> OperationId {
        self.cell.operation_id
    }

    /// Attempts to retrieve the terminal result without blocking.
    ///
    /// A successful `None` means that the operation is still pending. Like the
    /// other wait methods, observing or dropping this waiter never cancels work
    /// that has already been admitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion
    /// or when the operation itself fails.
    pub fn try_wait(&self) -> Result<Option<Completion>> {
        match self.cell.observe() {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }

    /// Waits asynchronously for the MQTT operation to finish.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails.
    pub async fn wait_async(&self) -> Result<Completion> {
        loop {
            let notified = self.cell.notified.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.cell.observe() {
                return result;
            }
            notified.await;
        }
    }

    /// Blocks until the MQTT operation finishes.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails.
    pub fn wait(&self) -> Result<Completion> {
        let mut state = self
            .cell
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.is_none() {
            state = self
                .cell
                .completed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.as_ref().expect("completion checked").clone()
    }

    /// Blocks for at most `timeout` while waiting for the MQTT operation to finish.
    ///
    /// # Errors
    ///
    /// Returns an error on timeout, when the driver terminates before reporting completion, or
    /// when the operation itself fails.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<Completion> {
        match self.wait_timeout_outcome(timeout) {
            CompletionWaitOutcome::Completed(result) => result,
            CompletionWaitOutcome::DeadlineElapsed => Err(Error::new(
                ErrorKind::Timeout,
                format!(
                    "operation {} did not complete before timeout",
                    self.operation_id().get()
                ),
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)),
        }
    }

    /// Blocks for at most `timeout`, preserving whether a timeout came from the wait deadline or
    /// from the operation's terminal result.
    #[must_use]
    pub fn wait_timeout_outcome(&self, timeout: Duration) -> CompletionWaitOutcome {
        let started = Instant::now();
        let mut remaining = timeout;
        let mut state = self
            .cell
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(result) = state.as_ref() {
                return CompletionWaitOutcome::Completed(result.clone());
            }
            let (next, wait) = self
                .cell
                .completed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if wait.timed_out() && state.is_none() {
                return CompletionWaitOutcome::DeadlineElapsed;
            }
            remaining = timeout.saturating_sub(started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn handle() -> CompletionHandle {
        CompletionHandle::new(CompletionCell::new(OperationId(
            NonZeroU64::new(1).unwrap(),
        )))
    }

    #[test]
    fn timeout_outcome_distinguishes_wait_deadline_from_terminal_error() {
        assert!(matches!(
            handle().wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));

        let handle = handle();
        handle
            .cell
            .complete(Err(Error::new(ErrorKind::Timeout, "disconnect timed out")));
        let CompletionWaitOutcome::Completed(Err(error)) =
            handle.wait_timeout_outcome(Duration::ZERO)
        else {
            panic!("terminal timeout was not preserved");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.message(), "disconnect timed out");
    }

    #[test]
    fn completion_is_repeatable_for_clones_and_blocking_waiters() {
        let handle = handle();
        let first = handle.clone();
        let second = handle.clone();
        let first_waiter = std::thread::spawn(move || first.wait());
        let second_waiter = std::thread::spawn(move || second.wait());
        handle.cell.complete(Ok(Completion::Acknowledged));

        assert_eq!(
            first_waiter.join().unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(
            second_waiter.join().unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(handle.wait().unwrap(), Completion::Acknowledged);
        assert_eq!(handle.try_wait().unwrap(), Some(Completion::Acknowledged));
    }

    #[tokio::test]
    async fn async_waiter_cancellation_and_deadline_do_not_change_terminal_result() {
        let handle = handle();
        let cancelled = handle.clone();
        let task = tokio::spawn(async move { cancelled.wait_async().await });
        task.abort();
        assert!(matches!(
            handle.wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));

        handle
            .cell
            .complete(Err(Error::new(ErrorKind::Protocol, "rejected")));
        for observer in [handle.clone(), handle] {
            let error = observer.wait_async().await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Protocol);
            assert_eq!(error.message(), "rejected");
        }
    }

    #[tokio::test]
    async fn mixed_waiters_and_dropped_clones_share_one_result() {
        let handle = handle();
        drop(handle.clone());

        let blocking = handle.clone();
        let blocking = std::thread::spawn(move || blocking.wait());
        let async_first = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.wait_async().await })
        };
        let async_second = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.wait_async().await })
        };

        assert!(handle.cell.complete(Ok(Completion::Acknowledged)));
        assert!(!handle.cell.complete(Ok(Completion::ImmediateShutdown)));
        assert_eq!(blocking.join().unwrap().unwrap(), Completion::Acknowledged);
        assert_eq!(
            async_first.await.unwrap().unwrap(),
            Completion::Acknowledged
        );
        assert_eq!(
            async_second.await.unwrap().unwrap(),
            Completion::Acknowledged
        );

        let last = handle.clone();
        drop(handle);
        assert_eq!(last.wait().unwrap(), Completion::Acknowledged);
    }
}

#[derive(Clone, Debug)]
pub struct Admission {
    pub operation_id: OperationId,
    pub completion: CompletionHandle,
}
