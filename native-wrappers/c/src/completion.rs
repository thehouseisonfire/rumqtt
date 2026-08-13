use std::time::Duration;

use rumqttc_wrapper_core::{
    Completion, CompletionHandle, CompletionWaitOutcome, DeliveryStatus, Error, ErrorKind,
};

pub struct CompletionObject {
    pub operation_id: u64,
    handle: CompletionHandle,
}

impl CompletionObject {
    pub fn new(handle: CompletionHandle) -> Self {
        Self {
            operation_id: handle.operation_id().get(),
            handle,
        }
    }

    pub fn poll(&self) -> Result<Option<Result<Completion, Error>>, &'static str> {
        match self.handle.try_wait() {
            Ok(None) => Ok(None),
            Ok(Some(completion)) => Ok(Some(Ok(completion))),
            Err(error) => Ok(Some(Err(error))),
        }
    }

    pub fn wait(&self, timeout: Duration) -> Result<Result<Completion, Error>, &'static str> {
        match self.handle.wait_timeout_outcome(timeout) {
            CompletionWaitOutcome::Completed(result) => Ok(result),
            CompletionWaitOutcome::DeadlineElapsed => Ok(Err(Error::new(
                ErrorKind::Timeout,
                format!(
                    "operation {} did not complete before timeout",
                    self.operation_id
                ),
            )
            .with_delivery(DeliveryStatus::Ambiguous))),
        }
    }
}
