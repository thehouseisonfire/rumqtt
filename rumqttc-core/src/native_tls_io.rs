//! Native TLS BIOs cannot represent an asynchronous flush on every platform
//! (OpenSSL's BIO_CTRL_FLUSH turns WouldBlock into a fatal handshake error).
//! Defer BIO flushes to the next read/handshake boundary, and expose truthful
//! asynchronous flush and shutdown through the outer stream.
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// Some native TLS backends omit the underlying I/O error from their source
// chain, or discard it entirely. Keep it independently during the handshake.
type HandshakeError = Arc<Mutex<Option<Arc<io::Error>>>>;

#[derive(Debug)]
struct SharedIoError(Arc<io::Error>);

impl std::fmt::Display for SharedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SharedIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

struct BioStream<S> {
    io: S,
    dirty: bool,
    handshake_error: Option<HandshakeError>,
}
impl<S> BioStream<S> {
    fn record<T>(&self, result: Poll<io::Result<T>>) -> Poll<io::Result<T>> {
        // These conditions may be retried inside TLS and are not failures of
        // the handshake. Async streams normally signal readiness with Pending.
        match (&self.handshake_error, result) {
            (Some(capture), Poll::Ready(Err(error)))
                if !matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                let error = Arc::new(error);
                let mut saved = capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if saved.is_none() {
                    *saved = Some(error.clone());
                }
                drop(saved);
                Poll::Ready(Err(io::Error::new(error.kind(), SharedIoError(error))))
            }
            (_, result) => result,
        }
    }
}
impl<S: AsyncWrite + Unpin> BioStream<S> {
    fn flush_actual(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.dirty {
            return Poll::Ready(Ok(()));
        }
        let result = Pin::new(&mut self.io).poll_flush(cx);
        match self.record(result) {
            Poll::Ready(Ok(())) => {
                self.dirty = false;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for BioStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match this.flush_actual(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {
                let result = Pin::new(&mut this.io).poll_read(cx, buf);
                this.record(result)
            }
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for BioStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.io).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(count)) if count > 0) {
            this.dirty = true;
        }
        this.record(result)
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // This private bridge is only seen by TLS's synchronous BIO, never by
        // MQTT callers. NativeIo below waits for the real underlying flush.
        self.get_mut().dirty = true;
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.io).poll_shutdown(cx);
        this.record(result)
    }
}

pub(super) struct NativeIo<S> {
    tls: tokio_native_tls::TlsStream<BioStream<S>>,
    tls_shutdown_done: bool,
}
impl<S> NativeIo<S> {
    fn base(&mut self) -> &mut BioStream<S> {
        self.tls.get_mut().get_mut().get_mut()
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for NativeIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().tls).poll_read(cx, buf)
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for NativeIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().tls).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.tls).poll_flush(cx) {
            Poll::Ready(Ok(())) => this.base().flush_actual(cx),
            other => other,
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.tls_shutdown_done {
            match Pin::new(&mut this.tls).poll_shutdown(cx) {
                Poll::Ready(Ok(())) => this.tls_shutdown_done = true,
                other => return other,
            }
        }
        match this.base().flush_actual(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        Pin::new(&mut this.base().io).poll_shutdown(cx)
    }
}

pub(super) async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
    connector: &tokio_native_tls::TlsConnector,
    domain: &str,
    stream: S,
) -> Result<NativeIo<S>, super::Error> {
    let capture = Arc::new(Mutex::new(None));
    let result = connector
        .connect(
            domain,
            BioStream {
                io: stream,
                dirty: false,
                handshake_error: Some(capture.clone()),
            },
        )
        .await;
    let original = capture
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(error) = original {
        // Drop TLS's copy first so the original error can usually be returned
        // intact, including its OS code and custom error payload.
        drop(result);
        let error = Arc::try_unwrap(error)
            .unwrap_or_else(|error| io::Error::new(error.kind(), SharedIoError(error)));
        return Err(error.into());
    }
    let mut tls = result?;
    tls.get_mut().get_mut().get_mut().handshake_error = None;
    let mut stream = NativeIo {
        tls,
        tls_shutdown_done: false,
    };
    std::future::poll_fn(|cx| stream.base().flush_actual(cx)).await?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum FailureAt {
        Read,
        Write,
        Flush,
    }

    struct FailingIo {
        operation: FailureAt,
        error: Option<io::Error>,
    }

    impl AsyncRead for FailingIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if matches!(this.operation, FailureAt::Read) {
                Poll::Ready(Err(this.error.take().unwrap()))
            } else {
                Poll::Pending
            }
        }
    }

    impl AsyncWrite for FailingIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let this = self.get_mut();
            if matches!(this.operation, FailureAt::Write) {
                Poll::Ready(Err(this.error.take().unwrap()))
            } else {
                Poll::Ready(Ok(bytes.len()))
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if matches!(this.operation, FailureAt::Flush) {
                Poll::Ready(Err(this.error.take().unwrap()))
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("transport marker")]
    struct Marker(Arc<()>);

    #[tokio::test]
    async fn handshake_returns_original_stream_errors_and_releases_the_capture() {
        let connector =
            tokio_native_tls::TlsConnector::from(native_tls::TlsConnector::new().unwrap());
        for operation in [FailureAt::Read, FailureAt::Write, FailureAt::Flush] {
            for os_error in [false, true] {
                let owner = Arc::new(());
                let weak = Arc::downgrade(&owner);
                let error = if os_error {
                    io::Error::from_raw_os_error(5)
                } else {
                    io::Error::other(Marker(owner.clone()))
                };
                let kind = error.kind();
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    connect(
                        &connector,
                        "localhost",
                        FailingIo {
                            operation,
                            error: Some(error),
                        },
                    ),
                )
                .await
                .unwrap();
                let Err(super::super::Error::Io(error)) = result else {
                    panic!("handshake did not preserve the original I/O error");
                };
                assert_eq!(error.kind(), kind);
                if os_error {
                    assert_eq!(error.raw_os_error(), Some(5));
                } else {
                    let marker = error.get_ref().unwrap().downcast_ref::<Marker>().unwrap();
                    assert!(Arc::ptr_eq(&marker.0, &owner));
                }
                drop(error);
                drop(owner);
                assert!(weak.upgrade().is_none());
            }
        }
    }

    #[test]
    fn captured_error_survives_a_tls_backend_discarding_its_source() {
        let owner = Arc::new(());
        let capture = Arc::new(Mutex::new(None));
        let stream = BioStream {
            io: (),
            dirty: false,
            handshake_error: Some(capture.clone()),
        };
        let result: Poll<io::Result<()>> =
            stream.record(Poll::Ready(Err(io::Error::other(Marker(owner.clone())))));
        // A backend may return an unrelated error and drop all of its sources.
        drop(result);
        drop(stream);
        let original = capture.lock().unwrap().take().unwrap();
        let marker = original
            .get_ref()
            .unwrap()
            .downcast_ref::<Marker>()
            .unwrap();
        assert!(Arc::ptr_eq(&marker.0, &owner));
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted] {
            let stream = BioStream {
                io: (),
                dirty: false,
                handshake_error: Some(capture.clone()),
            };
            assert!(matches!(stream.record::<()>(Poll::Pending), Poll::Pending));
            assert!(
                matches!(stream.record::<()>(Poll::Ready(Err(kind.into()))), Poll::Ready(Err(error)) if error.kind() == kind)
            );
            assert!(capture.lock().unwrap().is_none());
        }
    }

    struct DeferredFlush {
        pending: bool,
        reads: usize,
        error: bool,
    }
    impl AsyncRead for DeferredFlush {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.get_mut().reads += 1;
            buf.put_slice(b"x");
            Poll::Ready(Ok(()))
        }
    }
    impl AsyncWrite for DeferredFlush {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if std::mem::take(&mut this.pending) {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else if this.error {
                Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.poll_flush(cx)
        }
    }
    #[test]
    fn bio_flush_defers_pending_to_read_and_preserves_real_flush_errors() {
        for error in [false, true] {
            let mut stream = BioStream {
                io: DeferredFlush {
                    pending: true,
                    reads: 0,
                    error,
                },
                dirty: false,
                handshake_error: None,
            };
            let mut cx = Context::from_waker(std::task::Waker::noop());
            assert!(matches!(
                Pin::new(&mut stream).poll_write(&mut cx, b"hello"),
                Poll::Ready(Ok(5))
            ));
            assert!(matches!(
                Pin::new(&mut stream).poll_flush(&mut cx),
                Poll::Ready(Ok(()))
            ));
            let mut data = [0; 1];
            let mut buf = ReadBuf::new(&mut data);
            assert!(
                Pin::new(&mut stream)
                    .poll_read(&mut cx, &mut buf)
                    .is_pending()
            );
            assert_eq!(stream.io.reads, 0);
            assert!(buf.filled().is_empty());
            let result = Pin::new(&mut stream).poll_read(&mut cx, &mut buf);
            if error {
                assert!(
                    matches!(result, Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe)
                );
                assert_eq!(stream.io.reads, 0);
            } else {
                assert!(matches!(result, Poll::Ready(Ok(()))));
                assert_eq!(buf.filled(), b"x");
                assert!(!stream.dirty);
            }
        }
    }
}
