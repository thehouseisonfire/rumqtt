use std::error::Error as _;

use rumqttc_wrapper_core::{DeliveryStatus, Error, ErrorKind};

pub const OK: u32 = 0;
pub const INVALID_ARGUMENT: u32 = 1;
pub const INVALID_STATE: u32 = 2;
pub const CONFIG_ERROR: u32 = 3;
pub const BACKPRESSURE: u32 = 4;
pub const TIMEOUT: u32 = 5;
pub const DISCONNECTED: u32 = 6;
pub const PROTOCOL_ERROR: u32 = 7;
pub const BROKER_REJECTED: u32 = 8;
pub const AMBIGUOUS: u32 = 9;
pub const INTERNAL_ERROR: u32 = 10;
pub const WOULD_BLOCK: u32 = 11;

pub(crate) const ERROR_NONE: u32 = 0;
const ERROR_CONFIGURATION: u32 = 1;
const ERROR_ADMISSION: u32 = 2;
const ERROR_BACKPRESSURE: u32 = 3;
const ERROR_NETWORK: u32 = 4;
const ERROR_TLS: u32 = 5;
const ERROR_PROTOCOL: u32 = 6;
const ERROR_AUTHENTICATION: u32 = 7;
const ERROR_PERSISTENCE: u32 = 8;
const ERROR_TIMEOUT: u32 = 9;
const ERROR_SHUTDOWN: u32 = 10;
const ERROR_INTERNAL: u32 = 11;

#[derive(Clone, Debug)]
pub struct ErrorHandle {
    pub status: u32,
    pub kind: u32,
    pub message: String,
    pub source_chain: String,
    pub retryable: bool,
    pub ambiguous: bool,
    pub broker_reason: Option<u8>,
    pub operation_id: Option<u64>,
}

impl ErrorHandle {
    pub fn argument(message: impl Into<String>) -> Self {
        Self::plain(INVALID_ARGUMENT, ERROR_NONE, message)
    }

    pub fn state(message: impl Into<String>) -> Self {
        Self::plain(INVALID_STATE, ERROR_SHUTDOWN, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::plain(INTERNAL_ERROR, ERROR_INTERNAL, message)
    }

    pub fn plain(status: u32, kind: u32, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status,
            kind,
            source_chain: message.clone(),
            message,
            retryable: false,
            ambiguous: status == AMBIGUOUS,
            broker_reason: None,
            operation_id: None,
        }
    }

    pub fn from_core(error: &Error, operation_id: Option<u64>) -> Self {
        let ambiguous = error.delivery_status() == DeliveryStatus::Ambiguous;
        let status = if error.kind() == ErrorKind::Timeout {
            TIMEOUT
        } else if ambiguous {
            AMBIGUOUS
        } else if error.broker_reason().is_some()
            || error.delivery_status() == DeliveryStatus::Rejected
        {
            BROKER_REJECTED
        } else {
            match error.kind() {
                ErrorKind::Configuration => CONFIG_ERROR,
                ErrorKind::Admission => INVALID_ARGUMENT,
                ErrorKind::Backpressure => BACKPRESSURE,
                ErrorKind::Shutdown if error.delivery_status() == DeliveryStatus::NotAdmitted => {
                    INVALID_STATE
                }
                ErrorKind::Network | ErrorKind::Tls | ErrorKind::Shutdown => DISCONNECTED,
                ErrorKind::Protocol => PROTOCOL_ERROR,
                ErrorKind::Authentication => BROKER_REJECTED,
                ErrorKind::Timeout => TIMEOUT,
                ErrorKind::Persistence | ErrorKind::Internal => INTERNAL_ERROR,
            }
        };
        let kind = match error.kind() {
            ErrorKind::Configuration => ERROR_CONFIGURATION,
            ErrorKind::Admission => ERROR_ADMISSION,
            ErrorKind::Backpressure => ERROR_BACKPRESSURE,
            ErrorKind::Network => ERROR_NETWORK,
            ErrorKind::Tls => ERROR_TLS,
            ErrorKind::Protocol => ERROR_PROTOCOL,
            ErrorKind::Authentication => ERROR_AUTHENTICATION,
            ErrorKind::Persistence => ERROR_PERSISTENCE,
            ErrorKind::Timeout => ERROR_TIMEOUT,
            ErrorKind::Shutdown => ERROR_SHUTDOWN,
            ErrorKind::Internal => ERROR_INTERNAL,
        };
        let mut source_chain = error.to_string();
        let mut source = error.source();
        while let Some(next) = source {
            source_chain.push_str(": ");
            source_chain.push_str(&next.to_string());
            source = next.source();
        }
        Self {
            status,
            kind,
            message: error.message().to_owned(),
            source_chain,
            retryable: matches!(
                error.kind(),
                ErrorKind::Backpressure | ErrorKind::Network | ErrorKind::Tls | ErrorKind::Timeout
            ),
            ambiguous,
            broker_reason: error.broker_reason(),
            operation_id,
        }
    }

    pub const fn with_operation(mut self, operation_id: u64) -> Self {
        self.operation_id = Some(operation_id);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_failures_use_matching_public_error_kinds() {
        assert_eq!(ErrorHandle::argument("invalid argument").kind, ERROR_NONE);
        assert_eq!(ErrorHandle::state("invalid state").kind, ERROR_SHUTDOWN);
        assert_eq!(
            ErrorHandle::internal("internal failure").kind,
            ERROR_INTERNAL
        );
    }
}
