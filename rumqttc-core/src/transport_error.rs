use std::error::Error;
use std::io;

/// A transport failure that must stop connection candidate fallback.
///
/// MQTT 5 SRV redirects stop at this error instead of trying another target.
/// Wrap an I/O error with [`Self::new`] and [`Self::into_io`] to retain its
/// original source and signal terminal failure through proxy, TLS and framing.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TerminalTransportError(#[source] io::Error);

impl TerminalTransportError {
    /// Marks an original stream or connector error as terminal.
    #[must_use]
    pub const fn new(error: io::Error) -> Self {
        Self(error)
    }

    /// Returns the original error, including its typed payload or OS code.
    #[must_use]
    pub const fn get_ref(&self) -> &io::Error {
        &self.0
    }

    /// Converts the marker to an I/O error with the original error kind.
    #[must_use]
    pub fn into_io(self) -> io::Error {
        io::Error::new(self.0.kind(), self)
    }

    /// Finds a terminal marker in an error's typed source chain.
    #[must_use]
    pub fn find<'a>(error: &'a (dyn Error + 'static)) -> Option<&'a Self> {
        if let Some(terminal) = error.downcast_ref::<Self>() {
            return Some(terminal);
        }
        // io::Error::source() skips the contained error itself; inspect it too.
        if let Some(error) = error.downcast_ref::<io::Error>() {
            return error.get_ref().and_then(|inner| Self::find(inner));
        }
        error.source().and_then(Self::find)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("stream failed")]
    struct StreamFailure;

    #[derive(Debug, thiserror::Error)]
    #[error("connection failed: {0}")]
    struct ConnectionFailure(#[source] io::Error);

    #[test]
    fn marker_preserves_typed_payload_through_io_and_connection_sources() {
        let error = ConnectionFailure(
            TerminalTransportError::new(io::Error::new(io::ErrorKind::InvalidData, StreamFailure))
                .into_io(),
        );
        assert_eq!(error.0.kind(), io::ErrorKind::InvalidData);
        let original = TerminalTransportError::find(&error).unwrap().get_ref();
        assert!(original.get_ref().unwrap().is::<StreamFailure>());
        assert!(original.source().is_none());
    }

    #[test]
    fn marker_preserves_os_error_and_does_not_classify_unmarked_errors() {
        let original = io::Error::from_raw_os_error(12);
        let kind = original.kind();
        let error = TerminalTransportError::new(original).into_io();
        assert_eq!(error.kind(), kind);
        assert_eq!(
            TerminalTransportError::find(&error)
                .unwrap()
                .get_ref()
                .raw_os_error(),
            Some(12)
        );
        assert!(TerminalTransportError::find(&io::Error::from_raw_os_error(12)).is_none());
        assert!(
            TerminalTransportError::find(&ConnectionFailure(io::Error::other(StreamFailure)))
                .is_none()
        );
    }
}
