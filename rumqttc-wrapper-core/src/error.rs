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
    delivery: DeliveryStatus,
    message: Arc<str>,
    broker_reason: Option<u8>,
    #[source]
    source: Option<Arc<dyn StdError + Send + Sync>>,
}

impl Error {
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
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
        Self {
            kind,
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
    pub(crate) const fn with_broker_reason(mut self, reason: u8) -> Self {
        self.broker_reason = Some(reason);
        self
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
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
