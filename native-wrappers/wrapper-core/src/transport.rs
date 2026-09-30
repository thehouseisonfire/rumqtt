//! Owned custom transports. Native proxy, TLS, and WebSocket framing remain
//! authoritative when a connector supplies a base stream.

use std::time::Instant;

use crate::{NetworkConfig, ProtocolVersion};

/// The stream's position in transport composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportMode {
    /// Raw bytes before native proxy, TLS, and WebSocket negotiation.
    Base,
    /// MQTT-ready bytes; configure TCP without a native proxy or TLS layer.
    Established,
}

/// Explicit acknowledgement of the requested socket settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkHandling {
    Applied,
    /// Allowed only when no socket settings were requested.
    NotApplicable,
}

/// Owned connection-attempt metadata. Generation increases on every socket
/// attempt (including failed attempts), independently of CONNACK generations.
#[derive(Clone, Debug)]
pub struct TransportRequest {
    pub protocol: ProtocolVersion,
    pub client_id: String,
    /// Actual dial target; this is the proxy endpoint when a proxy is enabled.
    pub target: String,
    pub generation: u64,
    /// The native attempt's absolute deadline, including MQTT negotiation.
    pub deadline: Instant,
    pub network: NetworkConfig,
    pub mode: TransportMode,
}

/// Safe, fixed classifications. Host diagnostic strings never cross wrappers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportFailure {
    #[error("custom transport connection failed")]
    Connect,
    #[error("custom transport socket settings are unsupported")]
    NetworkOptions,
    #[error("custom transport composition is invalid")]
    Composition,
    #[error("custom transport returned an invalid result")]
    InvalidResult,
    #[error("custom transport operation was abandoned")]
    Abandoned,
    #[error("custom transport operation failed")]
    Io,
    #[error("custom transport callback timed out")]
    Timeout,
    #[error("custom transport callback panicked")]
    Panic,
    #[error("custom transport retained-operation limit reached")]
    ResourceLimit,
}

impl TransportFailure {
    pub(crate) const fn retryable(self) -> bool {
        matches!(
            self,
            Self::Connect | Self::Io | Self::Timeout | Self::Abandoned
        )
    }

    #[must_use]
    pub fn into_io(self) -> std::io::Error {
        let kind = match self {
            Self::Timeout => std::io::ErrorKind::TimedOut,
            Self::InvalidResult => std::io::ErrorKind::InvalidData,
            Self::NetworkOptions | Self::Composition => std::io::ErrorKind::Unsupported,
            Self::Abandoned | Self::Connect | Self::Io => std::io::ErrorKind::BrokenPipe,
            Self::Panic | Self::ResourceLimit => std::io::ErrorKind::Other,
        };
        crate::backend::transport_io_error(kind, self)
    }
}

pub struct TransportConnection {
    pub io: Arc<dyn OwnedIo>,
    pub mode: TransportMode,
    pub network_handling: NetworkHandling,
}

pub type TransportFuture =
    Pin<Box<dyn Future<Output = Result<TransportConnection, TransportFailure>> + Send>>;

/// Methods and futures must return/yield promptly; cancellation drops the
/// future. Detached operations retain their owners and buffers until released.
/// Do not reuse a stream after cancellation. Destructors must not panic/block.
pub trait TransportConnector: Send + Sync + 'static {
    fn connect(&self, request: TransportRequest) -> TransportFuture;
}

#[derive(Clone)]
pub struct TransportConnectorConfig {
    pub connector: Arc<dyn TransportConnector>,
    pub mode: TransportMode,
}

impl std::fmt::Debug for TransportConnectorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportConnectorConfig")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}
impl PartialEq for TransportConnectorConfig {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode && Arc::ptr_eq(&self.connector, &other.connector)
    }
}
impl Eq for TransportConnectorConfig {}

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const CHUNK: usize = 16 * 1024;
const IMMEDIATE_WRITE_BUDGET: usize = 64;
pub type IoFuture<T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'static>>;

/// Calls must return promptly. A read owns its result; a write owns its input.
/// Dropping a future stops observation. Detached work must retain its buffers
/// and owner until released, and a discarded stream must never be reused.
/// One read may overlap one write, flush, or shutdown; writes are serialized.
pub trait OwnedIo: Send + Sync + 'static {
    fn read(&self, max: usize) -> IoFuture<Bytes>;
    fn write(&self, data: Bytes) -> IoFuture<usize>;
    fn flush(&self) -> IoFuture<()>;
    fn shutdown(&self) -> IoFuture<()>;
}

#[derive(Clone, Copy)]
struct SavedFailure {
    kind: io::ErrorKind,
    typed: Option<TransportFailure>,
}
impl SavedFailure {
    fn from_error(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            typed: crate::backend::transport_io_failure(error),
        }
    }
    fn error(self) -> io::Error {
        self.typed
            .map_or_else(|| failure(self.kind), TransportFailure::into_io)
    }
}

type HostWork<T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send>>;

/// Own the host future so cancellation/destruction retains the callback
/// context too. A panicking destructor must not escape driver teardown.
pub(crate) struct HostFuture<T, E> {
    future: Option<HostWork<T, E>>,
    failure: fn(TransportFailure) -> E,
}
impl<T, E> HostFuture<T, E> {
    pub(crate) fn new(future: HostWork<T, E>, failure: fn(TransportFailure) -> E) -> Self {
        Self {
            future: Some(future),
            failure,
        }
    }
    fn destroy(&mut self) -> Result<(), TransportFailure> {
        let Some(future) = self.future.take() else {
            return Ok(());
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| drop(future));
        })) {
            Ok(()) => Ok(()),
            Err(payload) => {
                std::mem::forget(payload);
                Err(TransportFailure::Panic)
            }
        }
    }
}
impl<T, E> Future for HostFuture<T, E> {
    type Output = Result<T, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::runtime::with_host_callback(|| {
                this.future
                    .as_mut()
                    .expect("host future already completed")
                    .as_mut()
                    .poll(cx)
            })
        }));
        match result {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => match this.destroy() {
                Ok(()) => Poll::Ready(result),
                Err(error) => Poll::Ready(Err((this.failure)(error))),
            },
            Err(payload) => {
                std::mem::forget(payload);
                let _ = this.destroy();
                Poll::Ready(Err((this.failure)(TransportFailure::Panic)))
            }
        }
    }
}
impl<T, E> Drop for HostFuture<T, E> {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

fn start_io<T: Send + 'static>(call: impl FnOnce() -> IoFuture<T>) -> IoFuture<T> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::runtime::with_host_callback(call)
    }));
    match result {
        Ok(future) => Box::pin(HostFuture::new(future, TransportFailure::into_io)),
        Err(payload) => {
            std::mem::forget(payload);
            Box::pin(async { Err(TransportFailure::Panic.into_io()) })
        }
    }
}

struct PendingWrite {
    remaining: Bytes,
    operation: IoFuture<usize>,
}

struct StreamState {
    io: Arc<dyn OwnedIo>,
    read: Option<IoFuture<Bytes>>,
    read_limit: usize,
    read_cache: Bytes,
    eof: bool,
    read_failure: Option<SavedFailure>,
    write: Option<PendingWrite>,
    flush: Option<IoFuture<()>>,
    shutdown: Option<IoFuture<()>>,
    write_failure: Option<SavedFailure>,
    closing: bool,
    closed: bool,
}

/// Send futures are held in a Mutex to satisfy the non-WebSocket clients' Sync
/// bound without unsafe trait implementations. Polling always uses exclusive
/// get_mut: no mutex or wrapper lock is held while invoking foreign code.
pub(crate) struct OwnedStream(Mutex<StreamState>);

fn failure(kind: io::ErrorKind) -> io::Error {
    io::Error::new(kind, "foreign stream operation failed")
}

impl OwnedStream {
    pub(crate) fn new(io: Arc<dyn OwnedIo>) -> Self {
        Self(Mutex::new(StreamState {
            io,
            read: None,
            read_limit: 0,
            read_cache: Bytes::new(),
            eof: false,
            read_failure: None,
            write: None,
            flush: None,
            shutdown: None,
            write_failure: None,
            closing: false,
            closed: false,
        }))
    }

    fn state(&mut self) -> &mut StreamState {
        self.0
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl StreamState {
    fn drive_write(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(kind) = self.write_failure {
            return Poll::Ready(Err(kind.error()));
        }
        for _ in 0..IMMEDIATE_WRITE_BUDGET {
            let Some(write) = self.write.as_mut() else {
                return Poll::Ready(Ok(()));
            };
            let result = match write.operation.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result,
            };
            match result {
                Ok(count) if count > 0 && count <= write.remaining.len() => {
                    if count == write.remaining.len() {
                        self.write = None;
                        return Poll::Ready(Ok(()));
                    }
                    write.remaining = write.remaining.slice(count..);
                    write.operation = start_io(|| self.io.write(write.remaining.clone()));
                }
                other => {
                    let error = match other {
                        Ok(0) => failure(io::ErrorKind::WriteZero),
                        Ok(_) => TransportFailure::InvalidResult.into_io(),
                        Err(error) => error,
                    };
                    let saved = SavedFailure::from_error(&error);
                    self.write = None;
                    self.write_failure = Some(saved);
                    return Poll::Ready(Err(saved.error()));
                }
            }
        }
        // Short synchronous completions must not monopolize the MQTT task.
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn drive_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.drive_write(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if self.closed || self.shutdown.is_some() {
            return Poll::Ready(Ok(()));
        }
        let operation = self
            .flush
            .get_or_insert_with(|| start_io(|| self.io.flush()));
        match operation.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.flush = None;
                if let Err(error) = &result {
                    self.write_failure = Some(SavedFailure::from_error(error));
                }
                Poll::Ready(result.map_err(|error| SavedFailure::from_error(&error).error()))
            }
        }
    }
}

impl AsyncRead for OwnedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let state = self.get_mut().state();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        // TLS implementations may read immediately after write acceptance,
        // without flushing. Keep the owned write progressing and register this
        // reader's waker, while allowing the independent read to overlap it.
        if let Poll::Ready(Err(error)) = state.drive_write(cx) {
            return Poll::Ready(Err(error));
        }
        if let Some(kind) = state.read_failure {
            return Poll::Ready(Err(kind.error()));
        }
        if state.eof {
            return Poll::Ready(Ok(()));
        }
        if state.read_cache.is_empty() {
            if state.read.is_none() {
                state.read_limit = buf.remaining().min(CHUNK);
                state.read = Some(start_io(|| state.io.read(state.read_limit)));
            }
            let operation = state.read.as_mut().expect("read operation installed");
            let result = match operation.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result,
            };
            state.read = None;
            match result {
                Ok(data) if data.len() <= state.read_limit => state.read_cache = data,
                other => {
                    let error = other
                        .err()
                        .unwrap_or_else(|| TransportFailure::InvalidResult.into_io());
                    let saved = SavedFailure::from_error(&error);
                    state.read_failure = Some(saved);
                    return Poll::Ready(Err(saved.error()));
                }
            }
            if state.read_cache.is_empty() {
                state.eof = true;
                return Poll::Ready(Ok(()));
            }
        }
        let count = buf.remaining().min(state.read_cache.len());
        buf.put_slice(&state.read_cache[..count]);
        state.read_cache = state.read_cache.slice(count..);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for OwnedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let state = self.get_mut().state();
        if state.closing {
            return Poll::Ready(Err(failure(io::ErrorKind::BrokenPipe)));
        }
        let progress = if state.flush.is_some() {
            state.drive_flush(cx)
        } else {
            state.drive_write(cx)
        };
        match progress {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let accepted = buf.len().min(CHUNK);
        let remaining = Bytes::copy_from_slice(&buf[..accepted]);
        state.write = Some(PendingWrite {
            operation: start_io(|| state.io.write(remaining.clone())),
            remaining,
        });
        // Pending means no bytes accepted from THIS call. Report the bounded
        // buffer acceptance immediately; flush/the next write reports errors.
        let _ = state.drive_write(cx);
        Poll::Ready(Ok(accepted))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().state().drive_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let state = self.get_mut().state();
        if let Some(kind) = state.write_failure {
            return Poll::Ready(Err(kind.error()));
        }
        if state.closed {
            return Poll::Ready(Ok(()));
        }
        state.closing = true;
        if state.shutdown.is_none() {
            match state.drive_flush(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {}
            }
            state.shutdown = Some(start_io(|| state.io.shutdown()));
        }
        let operation = state
            .shutdown
            .as_mut()
            .expect("shutdown operation installed");
        match operation.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                state.shutdown = None;
                if let Err(error) = &result {
                    state.write_failure = Some(SavedFailure::from_error(error));
                } else {
                    state.closed = true;
                }
                Poll::Ready(result.map_err(|error| SavedFailure::from_error(&error).error()))
            }
        }
    }
}

#[cfg(all(test, feature = "transport-proof"))]
#[path = "backend/transport_proof.rs"]
mod proof;

#[cfg(test)]
mod host_future_tests {
    use super::*;
    struct PanickingDrop {
        ready: bool,
    }
    impl Future for PanickingDrop {
        type Output = Result<(), TransportFailure>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            if self.ready {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
    }
    impl Drop for PanickingDrop {
        fn drop(&mut self) {
            panic!("host future destruction failed");
        }
    }
    #[test]
    fn future_destruction_panics_are_contained_on_completion_and_unpolled_cancellation() {
        let mut future = Box::pin(HostFuture::new(
            Box::pin(PanickingDrop { ready: true }),
            std::convert::identity,
        ));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Err(TransportFailure::Panic))
        ));
        drop(future);
        let future = HostFuture::new(
            Box::pin(PanickingDrop { ready: false }),
            std::convert::identity,
        );
        drop(future);
    }
}
