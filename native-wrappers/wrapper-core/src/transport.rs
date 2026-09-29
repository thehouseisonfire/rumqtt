//! Private feasibility proof. These types are compiled only into proof tests.
//! The eventual foreign contract must also define connector metadata, network
//! settings, composition, and wrapper lifecycle ownership before publication.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const CHUNK: usize = 16 * 1024;
const IMMEDIATE_WRITE_BUDGET: usize = 64;
type IoFuture<T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'static>>;

/// Calls must return promptly. A read owns its result; a write owns its input.
/// Dropping a future stops observation. Detached work must retain its buffers
/// and owner until released, and a discarded stream must never be reused.
/// One read may overlap one write, flush, or shutdown; writes are serialized.
trait OwnedIo: Send + Sync + 'static {
    fn read(&self, max: usize) -> IoFuture<Bytes>;
    fn write(&self, data: Bytes) -> IoFuture<usize>;
    fn flush(&self) -> IoFuture<()>;
    fn shutdown(&self) -> IoFuture<()>;
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
    read_failure: Option<io::ErrorKind>,
    write: Option<PendingWrite>,
    flush: Option<IoFuture<()>>,
    shutdown: Option<IoFuture<()>>,
    write_failure: Option<io::ErrorKind>,
    closing: bool,
    closed: bool,
}

/// Send futures are held in a Mutex to satisfy the non-WebSocket clients' Sync
/// bound without unsafe trait implementations. Polling always uses exclusive
/// get_mut: no mutex or wrapper lock is held while invoking foreign code.
struct OwnedStream(Mutex<StreamState>);

fn failure(kind: io::ErrorKind) -> io::Error {
    io::Error::new(kind, "foreign stream operation failed")
}

impl OwnedStream {
    fn new(io: Arc<dyn OwnedIo>) -> Self {
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
            return Poll::Ready(Err(failure(kind)));
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
                    write.operation = self.io.write(write.remaining.clone());
                }
                other => {
                    let kind = match other {
                        Ok(0) => io::ErrorKind::WriteZero,
                        Ok(_) => io::ErrorKind::InvalidData,
                        Err(error) => error.kind(),
                    };
                    self.write = None;
                    self.write_failure = Some(kind);
                    return Poll::Ready(Err(failure(kind)));
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
        let operation = self.flush.get_or_insert_with(|| self.io.flush());
        match operation.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.flush = None;
                if let Err(error) = &result {
                    self.write_failure = Some(error.kind());
                }
                Poll::Ready(result.map_err(|error| failure(error.kind())))
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
        if let Some(kind) = state.read_failure {
            return Poll::Ready(Err(failure(kind)));
        }
        if state.eof {
            return Poll::Ready(Ok(()));
        }
        if state.read_cache.is_empty() {
            if state.read.is_none() {
                state.read_limit = buf.remaining().min(CHUNK);
                state.read = Some(state.io.read(state.read_limit));
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
                    let kind = other
                        .err()
                        .map_or(io::ErrorKind::InvalidData, |error| error.kind());
                    state.read_failure = Some(kind);
                    return Poll::Ready(Err(failure(kind)));
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
            operation: state.io.write(remaining.clone()),
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
            return Poll::Ready(Err(failure(kind)));
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
            state.shutdown = Some(state.io.shutdown());
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
                    state.write_failure = Some(error.kind());
                } else {
                    state.closed = true;
                }
                Poll::Ready(result.map_err(|error| failure(error.kind())))
            }
        }
    }
}

#[path = "backend/transport_proof.rs"]
mod proof;
