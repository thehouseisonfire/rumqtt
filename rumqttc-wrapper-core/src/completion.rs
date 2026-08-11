use std::time::Duration;

use flume::Receiver;

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
pub struct CompletionHandle {
    operation_id: OperationId,
    receiver: Receiver<Result<Completion>>,
}

impl CompletionHandle {
    pub(crate) const fn new(
        operation_id: OperationId,
        receiver: Receiver<Result<Completion>>,
    ) -> Self {
        Self {
            operation_id,
            receiver,
        }
    }

    #[must_use]
    pub const fn operation_id(&self) -> OperationId {
        self.operation_id
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
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(flume::TryRecvError::Empty) => Ok(None),
            Err(flume::TryRecvError::Disconnected) => Err(Error::new(
                ErrorKind::Shutdown,
                "driver closed before reporting completion",
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)),
        }
    }

    /// Waits asynchronously for the MQTT operation to finish.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails.
    pub async fn wait_async(self) -> Result<Completion> {
        self.receiver.recv_async().await.map_err(|_| {
            Error::new(
                ErrorKind::Shutdown,
                "driver closed before reporting completion",
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)
        })?
    }

    /// Blocks until the MQTT operation finishes.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver terminates before reporting completion or the operation
    /// itself fails.
    pub fn wait(self) -> Result<Completion> {
        self.receiver.recv().map_err(|_| {
            Error::new(
                ErrorKind::Shutdown,
                "driver closed before reporting completion",
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)
        })?
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
                    self.operation_id.get()
                ),
            )
            .with_delivery(crate::DeliveryStatus::Ambiguous)),
        }
    }

    /// Blocks for at most `timeout`, preserving whether a timeout came from the wait deadline or
    /// from the operation's terminal result.
    #[must_use]
    pub fn wait_timeout_outcome(&self, timeout: Duration) -> CompletionWaitOutcome {
        match self.receiver.recv_timeout(timeout) {
            Ok(result) => CompletionWaitOutcome::Completed(result),
            Err(flume::RecvTimeoutError::Timeout) => CompletionWaitOutcome::DeadlineElapsed,
            Err(flume::RecvTimeoutError::Disconnected) => {
                CompletionWaitOutcome::Completed(Err(Error::new(
                    ErrorKind::Shutdown,
                    "driver closed before reporting completion",
                )
                .with_delivery(crate::DeliveryStatus::Ambiguous)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn handle(receiver: Receiver<Result<Completion>>) -> CompletionHandle {
        CompletionHandle::new(OperationId(NonZeroU64::new(1).unwrap()), receiver)
    }

    #[test]
    fn timeout_outcome_distinguishes_wait_deadline_from_terminal_error() {
        let (_pending_sender, pending_receiver) = flume::unbounded();
        assert!(matches!(
            handle(pending_receiver).wait_timeout_outcome(Duration::ZERO),
            CompletionWaitOutcome::DeadlineElapsed
        ));

        let (terminal_sender, terminal_receiver) = flume::unbounded();
        terminal_sender
            .send(Err(Error::new(ErrorKind::Timeout, "disconnect timed out")))
            .unwrap();
        let CompletionWaitOutcome::Completed(Err(error)) =
            handle(terminal_receiver).wait_timeout_outcome(Duration::ZERO)
        else {
            panic!("terminal timeout was not preserved");
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert_eq!(error.message(), "disconnect timed out");
    }
}

#[derive(Debug)]
pub struct Admission {
    pub operation_id: OperationId,
    pub completion: CompletionHandle,
}
