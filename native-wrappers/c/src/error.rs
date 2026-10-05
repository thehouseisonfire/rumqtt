use std::error::Error as _;
use std::num::NonZeroU32;
use std::sync::Arc;

use rumqttc_wrapper_core::{DeliveryStatus, Error, ErrorKind, OperationId};

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
pub const PERSISTENCE_ERROR: u32 = 12;
pub const AUTHENTICATION_ERROR: u32 = 13;
pub const REDIRECT_ERROR: u32 = 14;
pub const WEBSOCKET_HANDSHAKE_ERROR: u32 = 15;

pub const ERROR_NONE: u32 = 0;
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
    pub code: &'static str,
    pub message: Box<str>,
    pub source_chain: Arc<String>,
    pub retryable: bool,
    pub ambiguous: bool,
    pub broker_reason: Option<u8>,
    pub operation_id: Option<u64>,
    pub protocol: Option<NonZeroU32>,
    pub phase: Option<u32>,
    pub generation: Option<u64>,
    pub delivery_status: u32,
    failures: Option<Arc<FailureDetails>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailureDetails {
    pub store: Option<NonZeroU32>,
    pub auth: Option<NonZeroU32>,
    pub redirect: Option<NonZeroU32>,
    pub transport: Option<NonZeroU32>,
    pub websocket: Option<NonZeroU32>,
    pub tls_callback: Option<rumqttc_wrapper_core::TlsCallbackFailure>,
    pub ordered: Option<NonZeroU32>,
}
const EMPTY_FAILURES: FailureDetails = FailureDetails {
    store: None,
    auth: None,
    redirect: None,
    transport: None,
    websocket: None,
    tls_callback: None,
    ordered: None,
};

impl FailureDetails {
    fn from_core(error: &Error) -> Self {
        Self {
            ordered: error
                .ordered_disconnect_failure()
                .and_then(|failure| NonZeroU32::new(failure as u32)),
            store: error.store_failure().and_then(|failure| {
                NonZeroU32::new(match failure {
                    rumqttc_wrapper_core::StoreFailure::Load => 1,
                    rumqttc_wrapper_core::StoreFailure::Save => 2,
                    rumqttc_wrapper_core::StoreFailure::Clear => 3,
                    rumqttc_wrapper_core::StoreFailure::Corrupt => 4,
                    rumqttc_wrapper_core::StoreFailure::Version => 5,
                    rumqttc_wrapper_core::StoreFailure::Protocol => 6,
                    rumqttc_wrapper_core::StoreFailure::Oversized => 7,
                    rumqttc_wrapper_core::StoreFailure::Timeout => 8,
                    rumqttc_wrapper_core::StoreFailure::Panic => 9,
                    rumqttc_wrapper_core::StoreFailure::InUse => 10,
                    rumqttc_wrapper_core::StoreFailure::KeyMismatch => 11,
                })
            }),
            auth: error.auth_failure().and_then(|failure| {
                NonZeroU32::new(match failure {
                    rumqttc_wrapper_core::AuthFailure::Rejected => 1,
                    rumqttc_wrapper_core::AuthFailure::Panic => 2,
                    rumqttc_wrapper_core::AuthFailure::Timeout => 3,
                    rumqttc_wrapper_core::AuthFailure::InvalidResponse => 4,
                    rumqttc_wrapper_core::AuthFailure::Overlapping => 5,
                    rumqttc_wrapper_core::AuthFailure::ConnectionClosed => 6,
                    rumqttc_wrapper_core::AuthFailure::Method => 7,
                    rumqttc_wrapper_core::AuthFailure::BrokerRejected => 8,
                })
            }),
            tls_callback: error.tls_callback_failure(),
            websocket: error
                .websocket_failure()
                .and_then(|failure| NonZeroU32::new(failure as u32)),
            transport: error.transport_failure().and_then(|failure| {
                NonZeroU32::new(match failure {
                    rumqttc_wrapper_core::TransportFailure::Connect => 1,
                    rumqttc_wrapper_core::TransportFailure::NetworkOptions => 2,
                    rumqttc_wrapper_core::TransportFailure::Composition => 3,
                    rumqttc_wrapper_core::TransportFailure::InvalidResult => 4,
                    rumqttc_wrapper_core::TransportFailure::Abandoned => 5,
                    rumqttc_wrapper_core::TransportFailure::Io => 6,
                    rumqttc_wrapper_core::TransportFailure::Timeout => 7,
                    rumqttc_wrapper_core::TransportFailure::Panic => 8,
                    rumqttc_wrapper_core::TransportFailure::ResourceLimit => 9,
                })
            }),
            redirect: error
                .redirect_failure()
                .and_then(|failure| NonZeroU32::new(failure.code())),
        }
    }
}

impl ErrorHandle {
    pub fn argument(message: impl Into<String>) -> Self {
        Self::plain(INVALID_ARGUMENT, ERROR_NONE, message)
    }

    pub fn state(message: impl Into<String>) -> Self {
        Self::plain(INVALID_STATE, ERROR_ADMISSION, message)
    }

    pub fn would_block(message: impl Into<String>) -> Self {
        Self::plain(WOULD_BLOCK, ERROR_NONE, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::plain(INTERNAL_ERROR, ERROR_INTERNAL, message)
    }

    pub fn panic(message: impl Into<String>) -> Self {
        let mut error = Self::internal(message);
        error.code = "INTERNAL_PANIC";
        error
    }

    pub fn plain(status: u32, kind: u32, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status,
            kind,
            code: match status {
                OK => "NONE",
                INVALID_ARGUMENT => "INVALID_ARGUMENT",
                INVALID_STATE => "INVALID_STATE",
                CONFIG_ERROR => "CONFIGURATION_INVALID",
                BACKPRESSURE => "REQUEST_BACKPRESSURE",
                TIMEOUT => "TIMEOUT",
                DISCONNECTED => "NETWORK",
                PROTOCOL_ERROR => "PROTOCOL",
                BROKER_REJECTED => "BROKER_REJECTED",
                AMBIGUOUS => "AMBIGUOUS",
                INTERNAL_ERROR => "INTERNAL",
                WOULD_BLOCK => "WOULD_BLOCK",
                PERSISTENCE_ERROR => "PERSISTENCE",
                AUTHENTICATION_ERROR => "AUTHENTICATION",
                REDIRECT_ERROR => "REDIRECT",
                WEBSOCKET_HANDSHAKE_ERROR => "WEBSOCKET_HANDSHAKE",
                _ => "UNKNOWN",
            },
            source_chain: Arc::new(message.clone()),
            message: message.into_boxed_str(),
            retryable: false,
            ambiguous: status == AMBIGUOUS,
            broker_reason: None,
            operation_id: None,
            protocol: None,
            phase: None,
            generation: None,
            delivery_status: 0,
            failures: None,
        }
    }

    pub fn from_core(error: &Error, operation_id: Option<u64>) -> Self {
        let ambiguous = error.delivery_status() == DeliveryStatus::Ambiguous;
        let status = core_status(error);
        let kind = core_kind(error.kind());
        let mut source_chain = error.to_string();
        let mut source = error.source();
        while let Some(next) = source {
            source_chain.push_str(": ");
            source_chain.push_str(&next.to_string());
            source = next.source();
        }
        let details = FailureDetails::from_core(error);
        let failures = (details != EMPTY_FAILURES).then(|| Arc::new(details));
        Self {
            status,
            kind,
            code: error.code().as_str(),
            message: error.message().into(),
            source_chain: Arc::new(source_chain),
            retryable: error.retryable(),
            ambiguous,
            broker_reason: error.broker_reason(),
            operation_id: operation_id
                .or_else(|| error.context().operation_id.map(OperationId::get)),
            protocol: error.context().protocol.and_then(|value| {
                NonZeroU32::new(match value {
                    rumqttc_wrapper_core::ProtocolVersion::V4 => 1,
                    rumqttc_wrapper_core::ProtocolVersion::V5 => 2,
                })
            }),
            phase: error.context().phase.map(|value| match value {
                rumqttc_wrapper_core::ConnectionPhase::Attempt => 1,
                rumqttc_wrapper_core::ConnectionPhase::Established => 2,
            }),
            generation: error.context().generation,
            delivery_status: match error.delivery_status() {
                DeliveryStatus::NotApplicable => 0,
                DeliveryStatus::NotAdmitted => 1,
                DeliveryStatus::Rejected => 2,
                DeliveryStatus::Ambiguous => 3,
            },
            failures,
        }
    }

    pub fn failure_details(&self) -> &FailureDetails {
        self.failures.as_deref().unwrap_or(&EMPTY_FAILURES)
    }

    pub const fn with_operation(mut self, operation_id: u64) -> Self {
        self.operation_id = Some(operation_id);
        self
    }
}

const fn core_kind(kind: ErrorKind) -> u32 {
    match kind {
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
    }
}

fn core_status(error: &Error) -> u32 {
    if error.kind() == ErrorKind::Timeout {
        TIMEOUT
    } else if error.websocket_failure().is_some() {
        WEBSOCKET_HANDSHAKE_ERROR
    } else if error.delivery_status() == DeliveryStatus::Ambiguous {
        AMBIGUOUS
    } else if error.broker_reason().is_some() || error.delivery_status() == DeliveryStatus::Rejected
    {
        BROKER_REJECTED
    } else if error.store_failure().is_some() {
        PERSISTENCE_ERROR
    } else if error.auth_failure().is_some() {
        AUTHENTICATION_ERROR
    } else if error.redirect_failure().is_some() {
        REDIRECT_ERROR
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
            ErrorKind::Authentication => AUTHENTICATION_ERROR,
            ErrorKind::Timeout => TIMEOUT,
            ErrorKind::Persistence => PERSISTENCE_ERROR,
            ErrorKind::Internal => INTERNAL_ERROR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_failures_use_matching_public_error_kinds() {
        assert_eq!(ErrorHandle::argument("invalid argument").kind, ERROR_NONE);
        let state = ErrorHandle::state("invalid state");
        assert_eq!(state.kind, ERROR_ADMISSION);
        assert_eq!(state.code, "INVALID_STATE");
        let pending = ErrorHandle::would_block("not ready");
        assert_eq!(pending.status, WOULD_BLOCK);
        assert_eq!(pending.kind, ERROR_NONE);
        assert_eq!(pending.code, "WOULD_BLOCK");
        let shutdown =
            ErrorHandle::from_core(&Error::new(ErrorKind::Shutdown, "client is closed"), None);
        assert_eq!(shutdown.kind, ERROR_SHUTDOWN);
        assert_eq!(shutdown.code, "SHUTDOWN");
        assert_eq!(
            ErrorHandle::internal("internal failure").kind,
            ERROR_INTERNAL
        );
        assert_eq!(ErrorHandle::panic("panic").code, "INTERNAL_PANIC");
        let persistence =
            ErrorHandle::from_core(&Error::new(ErrorKind::Persistence, "store failed"), None);
        assert_eq!(persistence.status, PERSISTENCE_ERROR);
        let authentication = ErrorHandle::from_core(
            &Error::new(ErrorKind::Authentication, "authentication failed"),
            None,
        );
        assert_eq!(authentication.status, AUTHENTICATION_ERROR);
    }
}
