use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::Duration;

use rumqttc_wrapper_core::{
    ClientHandle, Completion, DisconnectProtocolOptions, EventConsumer, NativeClient,
    NativeClientCloser, ProtocolVersion, WrapperEvent,
};

pub enum ClientError {
    Core(rumqttc_wrapper_core::Error),
    State(&'static str),
    Internal(&'static str),
}

pub struct ClientObject {
    pub protocol: ProtocolVersion,
    pub handle: ClientHandle,
    pub events: Mutex<EventConsumer>,
    closer: NativeClientCloser,
    native: NativeClient,
    abandoned: bool,
    failed: AtomicBool,
}
impl ClientObject {
    pub fn start(
        config: rumqttc_wrapper_core::ClientConfig,
    ) -> Result<Self, rumqttc_wrapper_core::Error> {
        let protocol = config.protocol_version();
        let mut native = NativeClient::start(config)?;
        let handle = native.handle();
        let closer = native.closer();
        let events = native
            .take_events()
            .expect("a newly created native client owns its event consumer");
        Ok(Self {
            protocol,
            handle,
            events: Mutex::new(events),
            closer,
            native,
            abandoned: false,
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
                return Err(ClientError::Internal("event-consumer lock is poisoned"));
            }
        };
        match timeout {
            Some(timeout) => events.recv_timeout(timeout),
            None => events.try_recv(),
        }
        .map_err(ClientError::Core)
    }

    pub fn close(&self, timeout: Duration) -> Result<Completion, rumqttc_wrapper_core::Error> {
        self.closer.close(timeout)
    }

    pub fn close_after_queued(
        &self,
        timeout: Duration,
        options: DisconnectProtocolOptions,
    ) -> Result<Completion, rumqttc_wrapper_core::Error> {
        self.closer
            .close_after_queued_with_options(timeout, options)
    }

    pub fn close_now(&self, timeout: Duration) -> Result<(), ClientError> {
        self.closer.close_now(timeout).map_err(ClientError::Core)
    }

    /// Cleanup preserves any admitted disconnect payload and waits for all host callbacks.
    pub fn shutdown_and_join(&self, timeout: Duration) -> Result<(), ClientError> {
        self.handle.close_now_idempotent();
        self.native.join(timeout).map_err(ClientError::Core)
    }

    pub fn close_with_options(
        &self,
        timeout: Duration,
        options: DisconnectProtocolOptions,
    ) -> Result<Completion, ClientError> {
        self.closer
            .close_with_options(timeout, options)
            .map_err(ClientError::Core)
    }

    pub fn close_now_with_options(
        &self,
        timeout: Duration,
        options: DisconnectProtocolOptions,
    ) -> Result<(), ClientError> {
        self.closer
            .close_now_with_options(timeout, options)
            .map_err(ClientError::Core)
    }

    /// Requests immediate shutdown and relinquishes join ownership without waiting.
    pub fn abandon(&mut self) {
        self.handle.close_now_idempotent();
        self.abandoned = true;
    }
}

impl Drop for ClientObject {
    fn drop(&mut self) {
        if !self.abandoned {
            let _ = self.shutdown_and_join(Duration::from_secs(2));
        }
    }
}
