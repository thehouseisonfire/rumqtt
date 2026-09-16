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

/// Context independent of formatted diagnostic text. Generation identifies an
/// installed connection; it is absent before the first successful CONNACK.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ErrorContext {
    pub protocol: Option<crate::ProtocolVersion>,
    pub phase: Option<crate::ConnectionPhase>,
    pub generation: Option<u64>,
    pub operation_id: Option<crate::OperationId>,
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
    store_failure: Option<crate::StoreFailure>,
    auth_failure: Option<crate::AuthFailure>,
    redirect_failure: Option<crate::RedirectFailure>,
    context: ErrorContext,
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
            store_failure: None,
            auth_failure: None,
            redirect_failure: None,
            context: ErrorContext::default(),
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
            store_failure: None,
            auth_failure: None,
            redirect_failure: None,
            context: ErrorContext::default(),
            source: Some(Arc::new(error)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Structured terminal persistence failure, independent of diagnostic text.
    #[must_use]
    pub const fn store_failure(&self) -> Option<crate::StoreFailure> {
        self.store_failure
    }

    pub(crate) fn store(failure: crate::StoreFailure) -> Self {
        let mut error = Self::new(ErrorKind::Persistence, failure.to_string());
        error.store_failure = Some(failure);
        error
    }

    #[must_use]
    pub const fn auth_failure(&self) -> Option<crate::AuthFailure> {
        self.auth_failure
    }

    pub(crate) fn auth(failure: crate::AuthFailure) -> Self {
        let mut error = Self::new(ErrorKind::Authentication, failure.to_string());
        error.auth_failure = Some(failure);
        error
    }

    #[must_use]
    pub const fn redirect_failure(&self) -> Option<crate::RedirectFailure> {
        self.redirect_failure
    }

    pub(crate) fn redirect(failure: crate::RedirectFailure) -> Self {
        let mut error = Self::new(ErrorKind::Network, "broker redirect failed");
        error.redirect_failure = Some(failure);
        error.retryable = false;
        error
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
            .field("store_failure", &self.store_failure)
            .field("auth_failure", &self.auth_failure)
            .field("redirect_failure", &self.redirect_failure)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    #[must_use]
    pub const fn context(&self) -> ErrorContext {
        self.context
    }
    pub(crate) fn with_context(mut self, context: ErrorContext) -> Self {
        self.context.protocol = self.context.protocol.or(context.protocol);
        self.context.phase = self.context.phase.or(context.phase);
        self.context.generation = self.context.generation.or(context.generation);
        self.context.operation_id = self.context.operation_id.or(context.operation_id);
        self
    }
    pub(crate) fn with_operation(self, operation_id: crate::OperationId) -> Self {
        self.with_context(ErrorContext {
            operation_id: Some(operation_id),
            ..Default::default()
        })
    }
}

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
