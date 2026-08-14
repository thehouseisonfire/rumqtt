use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Configuration,
    Admission,
    Backpressure,
    Network,
    Tls,
    Protocol,
    Authentication,
    Persistence,
    Timeout,
    Shutdown,
    Internal,
}

/// Stable machine-readable classification shared by native host wrappers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    ConfigurationInvalid,
    CommandInvalid,
    RequestBackpressure,
    Network,
    Tls,
    Protocol,
    Authentication,
    Persistence,
    Timeout,
    Shutdown,
    BrokerRejected,
    EventBufferOverflow,
    InternalPanic,
    Internal,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "CONFIGURATION_INVALID",
            Self::CommandInvalid => "COMMAND_INVALID",
            Self::RequestBackpressure => "REQUEST_BACKPRESSURE",
            Self::Network => "NETWORK",
            Self::Tls => "TLS",
            Self::Protocol => "PROTOCOL",
            Self::Authentication => "AUTHENTICATION",
            Self::Persistence => "PERSISTENCE",
            Self::Timeout => "TIMEOUT",
            Self::Shutdown => "SHUTDOWN",
            Self::BrokerRejected => "BROKER_REJECTED",
            Self::EventBufferOverflow => "EVENT_BUFFER_OVERFLOW",
            Self::InternalPanic => "INTERNAL_PANIC",
            Self::Internal => "INTERNAL",
        }
    }
}

const fn defaults(kind: ErrorKind) -> (ErrorCode, bool) {
    match kind {
        ErrorKind::Configuration => (ErrorCode::ConfigurationInvalid, false),
        ErrorKind::Admission => (ErrorCode::CommandInvalid, false),
        ErrorKind::Backpressure => (ErrorCode::RequestBackpressure, true),
        ErrorKind::Network => (ErrorCode::Network, true),
        ErrorKind::Tls => (ErrorCode::Tls, true),
        ErrorKind::Protocol => (ErrorCode::Protocol, false),
        ErrorKind::Authentication => (ErrorCode::Authentication, false),
        ErrorKind::Persistence => (ErrorCode::Persistence, false),
        ErrorKind::Timeout => (ErrorCode::Timeout, true),
        ErrorKind::Shutdown => (ErrorCode::Shutdown, false),
        ErrorKind::Internal => (ErrorCode::Internal, false),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeliveryStatus {
    #[default]
    NotApplicable,
    NotAdmitted,
    Rejected,
    Ambiguous,
}

#[derive(Clone, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    kind: ErrorKind,
    code: ErrorCode,
    retryable: bool,
    delivery: DeliveryStatus,
    message: Arc<str>,
    broker_reason: Option<u8>,
    #[source]
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

impl Error {
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let (code, retryable) = defaults(kind);
        Self {
            kind,
            code,
            retryable,
            delivery: DeliveryStatus::NotApplicable,
            message: Arc::from(message.into()),
            broker_reason: None,
            source: None,
        }
    }

    pub(crate) fn configuration(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Configuration, message)
    }

    pub(crate) fn sourced<E>(kind: ErrorKind, delivery: DeliveryStatus, error: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        let message: Arc<str> = Arc::from(error.to_string());
        let (code, retryable) = defaults(kind);
        Self {
            kind,
            code,
            retryable,
            delivery,
            message,
            broker_reason: None,
            source: Some(Arc::new(error)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    #[must_use]
    pub const fn delivery_status(&self) -> DeliveryStatus {
        self.delivery
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Numeric MQTT reason code when the broker explicitly rejected an operation.
    #[must_use]
    pub const fn broker_reason(&self) -> Option<u8> {
        self.broker_reason
    }

    #[must_use]
    pub const fn with_delivery(mut self, delivery: DeliveryStatus) -> Self {
        self.delivery = delivery;
        self
    }

    #[must_use]
    pub const fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = code;
        self
    }

    #[must_use]
    pub const fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub(crate) const fn with_broker_reason(mut self, reason: u8) -> Self {
        self.broker_reason = Some(reason);
        self.code = ErrorCode::BrokerRejected;
        self.retryable = false;
        self
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("retryable", &self.retryable)
            .field("delivery", &self.delivery)
            .field("message", &self.message)
            .field("broker_reason", &self.broker_reason)
            .finish_non_exhaustive()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io;

    use super::{DeliveryStatus, Error, ErrorKind};

    #[test]
    fn sourced_error_preserves_display_and_source_chain() {
        let error = Error::sourced(
            ErrorKind::Network,
            DeliveryStatus::Ambiguous,
            io::Error::new(io::ErrorKind::ConnectionReset, "connection reset"),
        );

        assert_eq!(error.to_string(), "connection reset");
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("connection reset")
        );
    }
}
