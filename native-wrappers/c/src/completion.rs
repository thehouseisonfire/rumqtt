use std::sync::Arc;
use std::time::Duration;

use rumqttc_wrapper_core::{Completion, CompletionHandle, Error, TerminalOutcome};

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

    pub fn recovery_snapshot(&self) -> Option<rumqttc_wrapper_core::RecoverySnapshot> {
        self.handle.recovery_snapshot()
    }

    pub fn poll(&self) -> Option<Result<Completion, Error>> {
        match self.handle.try_wait() {
            Ok(None) => None,
            Ok(Some(completion)) => Some(Ok(completion)),
            Err(error) => Some(Err(error)),
        }
    }

    pub fn wait(&self, timeout: Duration) -> Result<Completion, Error> {
        self.handle.wait_timeout(timeout)
    }
}
