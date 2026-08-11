use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::{
    ClientHandle, Command, Completion, DeliveryStatus, Error, ErrorKind, EventConsumer,
    NativeClient, WrapperEvent,
};

use crate::completion::CompletionObject;

pub enum ClientError {
    Core(rumqttc_wrapper_core::Error),
    State(&'static str),
}

pub struct ClientObject {
    pub handle: ClientHandle,
    pub events: Mutex<EventConsumer>,
    native: Mutex<Option<NativeClient>>,
    close: Mutex<CloseState>,
    close_changed: Condvar,
    failed: AtomicBool,
}

enum CloseState {
    Open,
    Graceful {
        completion: Arc<CompletionObject>,
        worker_active: bool,
    },
    Closed {
        graceful: bool,
    },
}

fn remaining(started: Instant, timeout: Duration) -> Duration {
    timeout.saturating_sub(started.elapsed())
}

fn close_wait_timeout() -> Error {
    Error::new(
        ErrorKind::Timeout,
        "graceful close did not complete before timeout",
    )
    .with_delivery(DeliveryStatus::Ambiguous)
}

impl ClientObject {
    pub fn start(
        config: rumqttc_wrapper_core::ClientConfig,
    ) -> Result<Self, rumqttc_wrapper_core::Error> {
        let mut native = NativeClient::start(config)?;
        let handle = native.handle();
        let events = native
            .take_events()
            .expect("a newly created native client owns its event consumer");
        Ok(Self {
            handle,
            events: Mutex::new(events),
            native: Mutex::new(Some(native)),
            close: Mutex::new(CloseState::Open),
            close_changed: Condvar::new(),
            failed: AtomicBool::new(false),
        })
    }

    pub fn poison(&self) {
        self.failed.store(true, Ordering::Release);
        self.handle.close_now_idempotent();
    }

    pub fn ensure_usable(&self) -> Result<(), &'static str> {
        if self.failed.load(Ordering::Acquire) {
            Err("client is failed after an internal panic")
        } else {
            Ok(())
        }
    }

    pub fn recv(&self, timeout: Option<Duration>) -> Result<Option<WrapperEvent>, ClientError> {
        let mut events = match self.events.try_lock() {
            Ok(events) => events,
            Err(TryLockError::WouldBlock) => {
                return Err(ClientError::State("another event receive is active"));
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(ClientError::State("event-consumer lock is poisoned"));
            }
        };
        match timeout {
            Some(timeout) => events.recv_timeout(timeout),
            None => events.try_recv(),
        }
        .map_err(ClientError::Core)
    }

    pub fn close(
        &self,
        timeout: Duration,
    ) -> Result<Result<Completion, rumqttc_wrapper_core::Error>, ClientError> {
        let started = Instant::now();
        let completion = {
            let mut close = self
                .close
                .lock()
                .map_err(|_| ClientError::State("close lock is poisoned"))?;
            loop {
                match &mut *close {
                    CloseState::Open => {
                        let admission = self
                            .handle
                            .try_admit(Command::GracefulDisconnect {
                                timeout: Some(remaining(started, timeout)),
                            })
                            .map_err(ClientError::Core)?;
                        *close = CloseState::Graceful {
                            completion: Arc::new(CompletionObject::new(admission.completion)),
                            worker_active: false,
                        };
                    }
                    CloseState::Graceful {
                        completion,
                        worker_active: false,
                    } => {
                        let completion = Arc::clone(completion);
                        if let CloseState::Graceful { worker_active, .. } = &mut *close {
                            *worker_active = true;
                        }
                        break completion;
                    }
                    CloseState::Graceful {
                        worker_active: true,
                        ..
                    } => {
                        let wait = remaining(started, timeout);
                        if wait.is_zero() {
                            return Ok(Err(close_wait_timeout()));
                        }
                        let (next, _) = self
                            .close_changed
                            .wait_timeout(close, wait)
                            .map_err(|_| ClientError::State("close lock is poisoned"))?;
                        close = next;
                    }
                    CloseState::Closed { graceful: true } => {
                        return Ok(Ok(Completion::GracefulShutdown));
                    }
                    CloseState::Closed { graceful: false } => {
                        return Err(ClientError::State("client was already closed immediately"));
                    }
                }
            }
        };

        let result = match completion.wait(remaining(started, timeout)) {
            Ok(result) => result,
            Err(message) => {
                self.release_close_worker();
                return Err(ClientError::State(message));
            }
        };
        if result.is_err() {
            self.release_close_worker();
            return Ok(result);
        }
        if let Err(error) = self.join(remaining(started, timeout)) {
            self.release_close_worker();
            return Err(error);
        }

        let mut close = self
            .close
            .lock()
            .map_err(|_| ClientError::State("close lock is poisoned"))?;
        if matches!(*close, CloseState::Graceful { .. }) {
            *close = CloseState::Closed { graceful: true };
        }
        self.close_changed.notify_all();
        Ok(result)
    }

    fn release_close_worker(&self) {
        if let Ok(mut close) = self.close.lock() {
            if let CloseState::Graceful { worker_active, .. } = &mut *close {
                *worker_active = false;
            }
            self.close_changed.notify_all();
        }
    }

    pub fn close_now(&self, timeout: Duration) -> Result<(), ClientError> {
        self.handle.close_now_idempotent();
        if let Ok(mut close) = self.close.lock()
            && !matches!(*close, CloseState::Closed { .. })
        {
            *close = CloseState::Closed { graceful: false };
            self.close_changed.notify_all();
        }
        self.join(timeout)
    }

    fn join(&self, timeout: Duration) -> Result<(), ClientError> {
        let native = self
            .native
            .lock()
            .map_err(|_| ClientError::State("native-client lock is poisoned"))?;
        if let Some(native) = native.as_ref() {
            native.join(timeout).map_err(ClientError::Core)?;
        }
        drop(native);
        Ok(())
    }
}

impl Drop for ClientObject {
    fn drop(&mut self) {
        self.handle.close_now_idempotent();
        if let Ok(native) = self.native.get_mut()
            && let Some(native) = native.as_ref()
        {
            let _ = native.join(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_time_uses_one_budget_for_all_close_phases() {
        let started = Instant::now();
        assert!(remaining(started, Duration::ZERO).is_zero());
        assert!(remaining(started, Duration::from_secs(1)) <= Duration::from_secs(1));
    }
}
