use std::sync::Arc;
use std::time::Duration;

use rumqttc_wrapper_core::{
    Completion, CompletionHandle, CompletionWaitOutcome, DeliveryStatus, Error, ErrorKind,
    TerminalOutcome,
};

pub struct CompletionObject {
    pub operation_id: u64,
    handle: CompletionHandle,
}

impl CompletionObject {
    pub fn outcome(&self) -> Option<Arc<TerminalOutcome>> {
        self.handle.try_outcome()
    }
    pub fn new(handle: CompletionHandle) -> Self {
        Self {
            operation_id: handle.operation_id().get(),
            handle,
        }
    }

    pub fn poll(&self) -> Option<Result<Completion, Error>> {
        match self.handle.try_wait() {
            Ok(None) => None,
            Ok(Some(completion)) => Some(Ok(completion)),
            Err(error) => Some(Err(error)),
        }
    }

    pub fn wait(&self, timeout: Duration) -> Result<Completion, Error> {
        match self.handle.wait_timeout_outcome(timeout) {
            CompletionWaitOutcome::Completed(result) => result,
            CompletionWaitOutcome::DeadlineElapsed => Err(Error::new(
                ErrorKind::Timeout,
                format!(
                    "operation {} did not complete before timeout",
                    self.operation_id
                ),
            )
            .with_delivery(DeliveryStatus::Ambiguous)),
        }
    }
}
