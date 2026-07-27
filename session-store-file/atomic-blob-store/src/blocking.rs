use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobInspection, BlobMetadata, CleanupReport,
    EngineHandle, LoadStreamEndpoint, QuarantineInfo, STREAM_CHUNK_SIZE, SaveStreamEndpoint,
    SaveStreamMessage,
};

/// Blocking facade for the executor-neutral atomic blob-store engine.
#[derive(Clone, Debug)]
pub struct BlockingAtomicBlobStore {
    core: EngineHandle,
}

impl BlockingAtomicBlobStore {
    #[cfg(all(test, feature = "tokio", any(unix, windows)))]
    pub(crate) fn from_test_core(core: EngineHandle) -> Self {
        Self { core }
    }

    pub fn open(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
    ) -> Result<Self, AtomicBlobStoreError> {
        EngineHandle::open(root, namespace, options).map(|core| Self { core })
    }

    #[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
    #[doc(hidden)]
    pub fn open_with_benchmark_events(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
        events: std::sync::mpsc::Sender<crate::bench_instrumentation::BenchmarkEvent>,
    ) -> Result<Self, AtomicBlobStoreError> {
        EngineHandle::open_with_benchmark_events(root, namespace, options, events)
            .map(|core| Self { core })
    }

    pub fn load(&self, key: &[u8]) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
        self.core.load(key).wait()
    }

    pub fn save(&self, key: &[u8], payload: Vec<u8>) -> Result<(), AtomicBlobStoreError> {
        self.core.save(key, payload).wait()
    }

    pub fn save_from<R: Read + ?Sized>(
        &self,
        key: &[u8],
        reader: &mut R,
        declared_len: u64,
    ) -> Result<(), AtomicBlobStoreError> {
        let SaveStreamEndpoint { chunks, result } =
            self.core.start_save_stream(key, declared_len)?;
        let mut read = 0_u64;
        while read < declared_len {
            let requested = usize::try_from(declared_len - read)
                .unwrap_or(usize::MAX)
                .min(STREAM_CHUNK_SIZE);
            let mut chunk = vec![0; requested];
            let count = match reader.read(&mut chunk) {
                Ok(0) => {
                    drop(chunks);
                    let _ = result.wait();
                    return Err(AtomicBlobStoreError::InputEndedEarly {
                        declared: declared_len,
                        actual: read,
                    });
                }
                Ok(count) => count,
                Err(source) => {
                    drop(chunks);
                    let _ = result.wait();
                    return Err(AtomicBlobStoreError::InputIo { source });
                }
            };
            chunk.truncate(count);
            read += u64::try_from(count).expect("a chunk length always fits in u64");
            if chunks.send(SaveStreamMessage::Chunk(chunk)).is_err() {
                return result.wait();
            }
        }
        let mut trailing = [0_u8; 1];
        match reader.read(&mut trailing) {
            Ok(0) => {}
            Ok(_) => {
                drop(chunks);
                let _ = result.wait();
                return Err(AtomicBlobStoreError::InputHasTrailingData {
                    declared: declared_len,
                });
            }
            Err(source) => {
                drop(chunks);
                let _ = result.wait();
                return Err(AtomicBlobStoreError::InputIo { source });
            }
        }
        if chunks.send(SaveStreamMessage::Complete).is_err() {
            return result.wait();
        }
        drop(chunks);
        result.wait()
    }

    pub fn load_into<W: Write + ?Sized>(
        &self,
        key: &[u8],
        writer: &mut W,
    ) -> Result<Option<BlobMetadata>, AtomicBlobStoreError> {
        let LoadStreamEndpoint {
            chunks,
            acknowledgement,
            result,
        } = self.core.start_load_stream(key)?;
        while let Ok(chunk) = chunks.recv() {
            if let Err(source) = writer.write_all(&chunk) {
                drop(chunks);
                drop(acknowledgement);
                let _ = result.wait();
                return Err(AtomicBlobStoreError::OutputIo { source });
            }
        }
        let _ = acknowledgement.send(());
        result.wait()
    }

    pub fn clear(&self, key: &[u8]) -> Result<(), AtomicBlobStoreError> {
        self.core.clear(key).wait()
    }

    pub fn inspect(&self, key: &[u8]) -> Result<BlobInspection, AtomicBlobStoreError> {
        self.core.inspect(key).wait()
    }

    pub fn quarantine(&self, key: &[u8]) -> Result<QuarantineInfo, AtomicBlobStoreError> {
        self.core.quarantine(key).wait()
    }

    pub fn cleanup_stale_temporary_files(
        &self,
        minimum_age: Duration,
    ) -> Result<CleanupReport, AtomicBlobStoreError> {
        self.core.cleanup_stale_temporary_files(minimum_age).wait()
    }

    pub fn flush(&self) -> Result<(), AtomicBlobStoreError> {
        self.core.flush().wait()
    }

    pub fn close(&self) -> Result<(), AtomicBlobStoreError> {
        self.core.close().wait()
    }

    #[must_use]
    pub fn blob_path(&self, key: &[u8]) -> PathBuf {
        self.core.blob_path(key)
    }
}
