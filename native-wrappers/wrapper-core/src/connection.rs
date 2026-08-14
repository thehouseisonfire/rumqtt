use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::{Error, ProtocolVersion, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionResult {
    pub protocol: ProtocolVersion,
    pub session_present: bool,
}

#[derive(Clone, Debug)]
enum ConnectionState {
    Pending,
    Connected(ConnectionResult),
    Terminal(Error),
}

#[derive(Debug)]
struct ConnectionCell {
    state: Mutex<ConnectionState>,
    notified: Notify,
}

/// Cloneable, repeatably observable result of the first successful connection.
///
/// Recoverable connection-attempt failures deliberately leave this handle pending. Once a
/// connection succeeds, every current and future observer receives the same result. Terminal
/// shutdown or driver failure wakes observers with the corresponding error.
#[derive(Clone, Debug)]
pub struct ConnectionHandle {
    cell: Arc<ConnectionCell>,
}

impl ConnectionHandle {
    pub(crate) fn new() -> Self {
        Self {
            cell: Arc::new(ConnectionCell {
                state: Mutex::new(ConnectionState::Pending),
                notified: Notify::new(),
            }),
        }
    }

    pub(crate) fn connected(&self, result: ConnectionResult) {
        let mut state = self
            .cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, ConnectionState::Pending) {
            *state = ConnectionState::Connected(result);
            drop(state);
            self.cell.notified.notify_waiters();
        }
    }

    pub(crate) fn terminate(&self, error: Error) {
        let mut state = self
            .cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, ConnectionState::Pending) {
            *state = ConnectionState::Terminal(error);
            drop(state);
            self.cell.notified.notify_waiters();
        }
    }

    fn observe(&self) -> Option<Result<ConnectionResult>> {
        match &*self
            .cell
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ConnectionState::Pending => None,
            ConnectionState::Connected(result) => Some(Ok(*result)),
            ConnectionState::Terminal(error) => Some(Err(error.clone())),
        }
    }

    #[must_use]
    pub fn try_wait(&self) -> Option<Result<ConnectionResult>> {
        self.observe()
    }

    pub async fn wait_async(&self) -> Result<ConnectionResult> {
        loop {
            let notified = self.cell.notified.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.observe() {
                return result;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, ErrorKind};

    #[tokio::test]
    async fn first_connection_is_repeatable() {
        let handle = ConnectionHandle::new();
        let result = ConnectionResult {
            protocol: ProtocolVersion::V5,
            session_present: true,
        };
        handle.connected(result);
        assert_eq!(handle.wait_async().await.unwrap(), result);
        assert_eq!(handle.clone().wait_async().await.unwrap(), result);
    }

    #[tokio::test]
    async fn terminal_failure_wakes_waiters() {
        let handle = ConnectionHandle::new();
        handle.terminate(Error::new(ErrorKind::Internal, "failed"));
        let error = handle.wait_async().await.unwrap_err();
        assert_eq!(error.code(), ErrorCode::Internal);
    }
}
