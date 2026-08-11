use std::sync::Mutex;
use std::time::Duration;

use rumqttc_wrapper_core::{
    Completion, CompletionHandle, CompletionWaitOutcome, DeliveryStatus, Error, ErrorKind,
};

pub struct CompletionObject {
    pub operation_id: u64,
    state: Mutex<CompletionState>,
}

struct CompletionState {
    handle: Option<CompletionHandle>,
    result: Option<Result<Completion, Error>>,
}

impl CompletionObject {
    pub const fn new(handle: CompletionHandle) -> Self {
        Self {
            operation_id: handle.operation_id().get(),
            state: Mutex::new(CompletionState {
                handle: Some(handle),
                result: None,
            }),
        }
    }

    pub fn poll(&self) -> Result<Option<Result<Completion, Error>>, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "completion lock is poisoned")?;
        if let Some(result) = &state.result {
            let result = result.clone();
            drop(state);
            return Ok(Some(result));
        }
        let result = state
            .handle
            .as_ref()
            .ok_or("completion handle is unavailable")?
            .try_wait();
        match result {
            Ok(None) => Ok(None),
            Ok(Some(completion)) => {
                state.result = Some(Ok(completion.clone()));
                state.handle = None;
                drop(state);
                Ok(Some(Ok(completion)))
            }
            Err(error) => {
                state.result = Some(Err(error.clone()));
                state.handle = None;
                drop(state);
                Ok(Some(Err(error)))
            }
        }
    }

    pub fn wait(&self, timeout: Duration) -> Result<Result<Completion, Error>, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "completion lock is poisoned")?;
        if let Some(result) = &state.result {
            let result = result.clone();
            drop(state);
            return Ok(result);
        }
        let outcome = state
            .handle
            .as_ref()
            .ok_or("completion handle is unavailable")?
            .wait_timeout_outcome(timeout);
        let result = match outcome {
            CompletionWaitOutcome::Completed(result) => result,
            CompletionWaitOutcome::DeadlineElapsed => {
                return Ok(Err(Error::new(
                    ErrorKind::Timeout,
                    format!(
                        "operation {} did not complete before timeout",
                        self.operation_id
                    ),
                )
                .with_delivery(DeliveryStatus::Ambiguous)));
            }
        };
        state.result = Some(result.clone());
        state.handle = None;
        drop(state);
        Ok(result)
    }
}
