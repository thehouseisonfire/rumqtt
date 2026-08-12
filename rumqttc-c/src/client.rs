use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::Duration;

use rumqttc_wrapper_core::{
    ClientHandle, Completion, EventConsumer, NativeClient, NativeClientCloser, WrapperEvent,
};

pub enum ClientError {
    Core(rumqttc_wrapper_core::Error),
    State(&'static str),
}

pub struct ClientObject {
    pub handle: ClientHandle,
    pub events: Mutex<EventConsumer>,
    closer: NativeClientCloser,
    _native: NativeClient,
    failed: AtomicBool,
}

impl ClientObject {
    pub fn start(
        config: rumqttc_wrapper_core::ClientConfig,
    ) -> Result<Self, rumqttc_wrapper_core::Error> {
        let mut native = NativeClient::start(config)?;
        let handle = native.handle();
        let closer = native.closer();
        let events = native
            .take_events()
            .expect("a newly created native client owns its event consumer");
        Ok(Self {
            handle,
            events: Mutex::new(events),
            closer,
            _native: native,
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
        Ok(self.closer.close(timeout))
    }

    pub fn close_now(&self, timeout: Duration) -> Result<(), ClientError> {
        self.closer.close_now(timeout).map_err(ClientError::Core)
    }
}

impl Drop for ClientObject {
    fn drop(&mut self) {
        let _ = self.closer.close_now(Duration::from_secs(2));
    }
}
