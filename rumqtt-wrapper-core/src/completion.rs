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
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                flume::RecvTimeoutError::Timeout => Error::new(
                    ErrorKind::Timeout,
                    format!(
                        "operation {} did not complete before timeout",
                        self.operation_id.get()
                    ),
                )
                .with_delivery(crate::DeliveryStatus::Ambiguous),
                flume::RecvTimeoutError::Disconnected => Error::new(
                    ErrorKind::Shutdown,
                    "driver closed before reporting completion",
                )
                .with_delivery(crate::DeliveryStatus::Ambiguous),
            })?
    }
}

#[derive(Debug)]
pub struct Admission {
    pub operation_id: OperationId,
    pub completion: CompletionHandle,
}
