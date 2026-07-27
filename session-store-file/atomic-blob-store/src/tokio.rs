use std::ffi::OsStr;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobInspection, BlobMetadata, CleanupReport,
    EngineHandle, LoadStreamEndpoint, Pending, QuarantineInfo, STREAM_CHUNK_SIZE,
    SaveStreamEndpoint, SaveStreamMessage,
};

/// An immediately submitted complete store operation.
#[must_use = "dropping an operation discards its result but does not cancel accepted work"]
pub struct Operation<T> {
    future: Pin<Box<dyn Future<Output = Result<T, AtomicBlobStoreError>> + Send + 'static>>,
}

impl<T: Send + 'static> Operation<T> {
    fn new(pending: Pending<T>) -> Self {
        Self {
            future: Box::pin(pending.wait_async()),
        }
    }
}

impl<T> Future for Operation<T> {
    type Output = Result<T, AtomicBlobStoreError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(context)
    }
}

/// Tokio endpoint facade for the executor-neutral atomic blob-store engine.
#[derive(Clone, Debug)]
pub struct AtomicBlobStore {
    core: EngineHandle,
}

impl AtomicBlobStore {
    #[cfg(all(test, any(unix, windows)))]
    pub(crate) fn from_test_core(core: EngineHandle) -> Self {
        Self { core }
    }

    pub async fn open(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = namespace.as_ref().to_owned();
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, namespace, options);
            return Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            std::thread::Builder::new()
                .name("atomic-blob-store-initialize".into())
                .spawn(move || {
                    let _ = sender.send(EngineHandle::open(root, namespace, options));
                })
                .map_err(|source| AtomicBlobStoreError::Io {
                    operation: crate::StoreOperation::StartInitialization,
                    source,
                })?;
            receiver
                .recv_async()
                .await
                .unwrap_or(Err(AtomicBlobStoreError::EngineFailed))
                .map(|core| Self { core })
        }
    }

    #[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
    #[doc(hidden)]
    pub async fn open_with_benchmark_events(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
        events: std::sync::mpsc::Sender<crate::bench_instrumentation::BenchmarkEvent>,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = namespace.as_ref().to_owned();
        let (sender, receiver) = flume::bounded(1);
        std::thread::Builder::new()
            .name("atomic-blob-store-initialize".into())
            .spawn(move || {
                let result =
                    EngineHandle::open_with_benchmark_events(root, namespace, options, events);
                let _ = sender.send(result);
            })
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: crate::StoreOperation::StartInitialization,
                source,
            })?;
        receiver
            .recv_async()
            .await
            .unwrap_or(Err(AtomicBlobStoreError::EngineFailed))
            .map(|core| Self { core })
    }

    #[cfg(all(test, any(unix, windows)))]
    pub(crate) async fn open_with_test_hook(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
        hook: std::sync::Arc<dyn Fn(crate::TestStage) -> std::io::Result<()> + Send + Sync>,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = namespace.as_ref().to_owned();
        let (sender, receiver) = flume::bounded(1);
        std::thread::Builder::new()
            .name("atomic-blob-store-initialize".into())
            .spawn(move || {
                let result = EngineHandle::open_with_test_hook(root, namespace, options, hook);
                let _ = sender.send(result);
            })
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: crate::StoreOperation::StartInitialization,
                source,
            })?;
        receiver
            .recv_async()
            .await
            .unwrap_or(Err(AtomicBlobStoreError::EngineFailed))
            .map(|core| Self { core })
    }

    #[cfg(all(test, any(unix, windows)))]
    pub(crate) fn registry_entries(&self) -> usize {
        self.core
            .inner
            .registry_entries
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(all(test, any(unix, windows)))]
    pub(crate) fn is_closing(&self) -> bool {
        matches!(
            *self
                .core
                .inner
                .lifecycle
                .lock()
                .expect("lifecycle lock poisoned"),
            crate::Lifecycle::Closing
        )
    }

    pub fn load(&self, key: &[u8]) -> Operation<Option<Vec<u8>>> {
        Operation::new(self.core.load(key))
    }

    pub fn save(&self, key: &[u8], payload: Vec<u8>) -> Operation<()> {
        Operation::new(self.core.save(key, payload))
    }

    pub async fn save_from<R>(
        &self,
        key: &[u8],
        reader: &mut R,
        declared_len: u64,
    ) -> Result<(), AtomicBlobStoreError>
    where
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        let SaveStreamEndpoint { chunks, result } =
            self.core.start_save_stream(key, declared_len)?;
        let result = result.wait_async();
        ::tokio::pin!(result);
        let mut read = 0_u64;
        while read < declared_len {
            let requested = usize::try_from(declared_len - read)
                .unwrap_or(usize::MAX)
                .min(STREAM_CHUNK_SIZE);
            let mut chunk = vec![0; requested];
            let read_result = ::tokio::select! {
                biased;
                outcome = &mut result => return outcome,
                outcome = reader.read(&mut chunk) => outcome,
            };
            let count = match read_result {
                Ok(0) => {
                    drop(chunks);
                    let _ = result.await;
                    return Err(AtomicBlobStoreError::InputEndedEarly {
                        declared: declared_len,
                        actual: read,
                    });
                }
                Ok(count) => count,
                Err(source) => {
                    drop(chunks);
                    let _ = result.await;
                    return Err(AtomicBlobStoreError::InputIo { source });
                }
            };
            chunk.truncate(count);
            read += u64::try_from(count).expect("a chunk length always fits in u64");
            if chunks
                .send_async(SaveStreamMessage::Chunk(chunk))
                .await
                .is_err()
            {
                return result.await;
            }
        }
        let mut trailing = [0_u8; 1];
        let trailing_result = ::tokio::select! {
            biased;
            outcome = &mut result => return outcome,
            outcome = reader.read(&mut trailing) => outcome,
        };
        match trailing_result {
            Ok(0) => {}
            Ok(_) => {
                drop(chunks);
                let _ = result.await;
                return Err(AtomicBlobStoreError::InputHasTrailingData {
                    declared: declared_len,
                });
            }
            Err(source) => {
                drop(chunks);
                let _ = result.await;
                return Err(AtomicBlobStoreError::InputIo { source });
            }
        }
        if chunks
            .send_async(SaveStreamMessage::Complete)
            .await
            .is_err()
        {
            return result.await;
        }
        drop(chunks);
        result.await
    }

    pub async fn load_into<W>(
        &self,
        key: &[u8],
        writer: &mut W,
    ) -> Result<Option<BlobMetadata>, AtomicBlobStoreError>
    where
        W: AsyncWrite + Unpin + Send + ?Sized,
    {
        let LoadStreamEndpoint {
            chunks,
            acknowledgement,
            result,
        } = self.core.start_load_stream(key)?;
        while let Ok(chunk) = chunks.recv_async().await {
            if let Err(source) = writer.write_all(&chunk).await {
                drop(chunks);
                drop(acknowledgement);
                let _ = result.wait_async().await;
                return Err(AtomicBlobStoreError::OutputIo { source });
            }
        }
        let _ = acknowledgement.send(());
        result.wait_async().await
    }

    pub fn clear(&self, key: &[u8]) -> Operation<()> {
        Operation::new(self.core.clear(key))
    }

    pub fn inspect(&self, key: &[u8]) -> Operation<BlobInspection> {
        Operation::new(self.core.inspect(key))
    }

    pub fn quarantine(&self, key: &[u8]) -> Operation<QuarantineInfo> {
        Operation::new(self.core.quarantine(key))
    }

    pub fn cleanup_stale_temporary_files(&self, minimum_age: Duration) -> Operation<CleanupReport> {
        Operation::new(self.core.cleanup_stale_temporary_files(minimum_age))
    }

    pub fn flush(&self) -> Operation<()> {
        Operation::new(self.core.flush())
    }

    pub fn close(&self) -> Operation<()> {
        Operation::new(self.core.close())
    }

    #[must_use]
    pub fn blob_path(&self, key: &[u8]) -> PathBuf {
        self.core.blob_path(key)
    }
}
