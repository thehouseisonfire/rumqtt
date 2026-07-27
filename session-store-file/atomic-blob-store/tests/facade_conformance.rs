#![cfg(any(unix, windows))]

mod common;

use std::io::{self, Cursor, Read, Write};
#[cfg(feature = "tokio")]
use std::pin::Pin;
#[cfg(feature = "tokio")]
use std::task::{Context, Poll};

use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore,
    ENVELOPE_VERSION_V1,
};
use common::test_directory;

const MAXIMUM: u64 = 2 * 64 * 1024 + 17;
const CHUNK: usize = 64 * 1024;
const BOUNDARY_SIZES: [usize; 6] = [0, 1, CHUNK, CHUNK + 1, 2 * CHUNK, MAXIMUM as usize];

#[cfg(feature = "tokio")]
#[derive(Debug, Eq, PartialEq)]
struct ScriptObservation {
    metadata_len: u64,
    loaded: Vec<u8>,
    canonical: Vec<u8>,
    canonical_filename: String,
    inspection_size: u64,
    early_eof: &'static str,
    trailing: &'static str,
    source_failure: &'static str,
    destination_failure: &'static str,
    destination_flushes: usize,
    destination_shutdowns: usize,
    absent_after_clear: bool,
    after_close: &'static str,
}

#[cfg(feature = "tokio")]
fn error_category(error: &AtomicBlobStoreError) -> &'static str {
    match error {
        AtomicBlobStoreError::InputEndedEarly { .. } => "input-ended-early",
        AtomicBlobStoreError::InputHasTrailingData { .. } => "input-has-trailing-data",
        AtomicBlobStoreError::InputIo { .. } => "input-io",
        AtomicBlobStoreError::OutputIo { .. } => "output-io",
        AtomicBlobStoreError::StoreClosed => "store-closed",
        _ => "other",
    }
}

fn options() -> AtomicBlobStoreOptions {
    AtomicBlobStoreOptions::new(
        BlobFormatIdentity::new(b"CONFTEST", ".blob", ENVELOPE_VERSION_V1).unwrap(),
    )
    .with_max_blob_size(MAXIMUM)
}

#[derive(Default)]
struct TrackingWriter {
    bytes: Vec<u8>,
    flushes: usize,
    shutdowns: usize,
    fail_after: Option<usize>,
}

impl Write for TrackingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .fail_after
            .is_some_and(|limit| self.bytes.len() >= limit)
        {
            return Err(io::Error::other("scripted destination failure"));
        }
        let accepted = self.fail_after.map_or(bytes.len(), |limit| {
            bytes.len().min(limit - self.bytes.len())
        });
        self.bytes.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncWrite for TrackingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Write::write(&mut *self, bytes))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shutdowns += 1;
        Poll::Ready(Ok(()))
    }
}

struct ScriptedReader {
    bytes: Vec<u8>,
    position: usize,
    fail_at: Option<usize>,
}

impl ScriptedReader {
    fn new(bytes: Vec<u8>, fail_at: Option<usize>) -> Self {
        Self {
            bytes,
            position: 0,
            fail_at,
        }
    }
}

impl Read for ScriptedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.fail_at.is_some_and(|limit| self.position >= limit) {
            return Err(io::Error::other("scripted source failure"));
        }
        let remaining = self.bytes.len().saturating_sub(self.position);
        let before_failure = self
            .fail_at
            .map_or(remaining, |limit| limit.saturating_sub(self.position));
        let count = output.len().min(remaining).min(before_failure);
        output[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(feature = "tokio")]
impl tokio::io::AsyncRead for ScriptedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        output: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut bytes = vec![0; output.remaining()];
        match Read::read(&mut *self, &mut bytes) {
            Ok(count) => {
                output.put_slice(&bytes[..count]);
                Poll::Ready(Ok(()))
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

#[test]
fn blocking_streaming_boundary_matrix_is_bounded_and_preserves_destination_ownership() {
    let root = test_directory();
    let store = BlockingAtomicBlobStore::open(root.path(), "blocking-matrix", options()).unwrap();

    for size in BOUNDARY_SIZES {
        let key = format!("size-{size}");
        let payload = vec![size as u8; size];
        store
            .save_from(key.as_bytes(), &mut Cursor::new(&payload), size as u64)
            .unwrap();
        let mut destination = TrackingWriter::default();
        let metadata = store
            .load_into(key.as_bytes(), &mut destination)
            .unwrap()
            .unwrap();
        assert_eq!(metadata.payload_len, size as u64);
        assert_eq!(destination.bytes, payload);
        assert_eq!(destination.flushes, 0);
        assert_eq!(destination.shutdowns, 0);
    }

    let mut over_limit = Cursor::new(Vec::<u8>::new());
    assert!(matches!(
        store.save_from(b"over", &mut over_limit, MAXIMUM + 1),
        Err(AtomicBlobStoreError::BlobTooLarge { .. })
    ));

    store.save(b"preserved", b"old".to_vec()).unwrap();
    let mut early = Cursor::new(b"new".to_vec());
    assert!(matches!(
        store.save_from(b"preserved", &mut early, 4),
        Err(AtomicBlobStoreError::InputEndedEarly { .. })
    ));
    let mut trailing = Cursor::new(b"new!".to_vec());
    assert!(matches!(
        store.save_from(b"preserved", &mut trailing, 3),
        Err(AtomicBlobStoreError::InputHasTrailingData { .. })
    ));
    assert_eq!(store.load(b"preserved").unwrap(), Some(b"old".to_vec()));

    let payload = vec![7; 64 * 1024 + 1];
    store.save(b"destination", payload).unwrap();
    for fail_after in [0, CHUNK] {
        let mut failing = TrackingWriter {
            fail_after: Some(fail_after),
            ..TrackingWriter::default()
        };
        assert!(matches!(
            store.load_into(b"destination", &mut failing),
            Err(AtomicBlobStoreError::OutputIo { .. })
        ));
        assert_eq!(failing.flushes, 0);
    }

    std::fs::write(store.blob_path(b"invalid"), b"not-an-envelope").unwrap();
    let mut invalid_destination = TrackingWriter::default();
    assert!(
        store
            .load_into(b"invalid", &mut invalid_destination)
            .is_err()
    );
    assert!(invalid_destination.bytes.is_empty());
    assert_eq!(invalid_destination.flushes, 0);

    for (name, payload_size, declared) in [
        ("eof-before-data", 0, 1),
        ("eof-within-chunk", CHUNK - 1, CHUNK as u64),
        ("eof-at-boundary", CHUNK, CHUNK as u64 + 1),
    ] {
        let mut source = Cursor::new(vec![1; payload_size]);
        assert!(
            matches!(
                store.save_from(name.as_bytes(), &mut source, declared),
                Err(AtomicBlobStoreError::InputEndedEarly { .. })
            ),
            "{name}"
        );
    }

    store.save(b"source-failure", b"old".to_vec()).unwrap();
    for fail_at in [0, CHUNK] {
        let payload = vec![3; CHUNK + 1];
        let mut source = ScriptedReader::new(payload.clone(), Some(fail_at));
        assert!(matches!(
            store.save_from(
                b"source-failure",
                &mut source,
                u64::try_from(payload.len()).unwrap()
            ),
            Err(AtomicBlobStoreError::InputIo { .. })
        ));
        assert_eq!(
            store.load(b"source-failure").unwrap(),
            Some(b"old".to_vec())
        );
    }

    // Blocking reads cannot be cancelled while pending. A source failure on the mandatory
    // trailing-data probe exercises the corresponding boundary after all declared input arrived.
    let payload = vec![4; CHUNK + 1];
    let mut source = ScriptedReader::new(payload.clone(), Some(payload.len()));
    assert!(matches!(
        store.save_from(
            b"source-failure",
            &mut source,
            u64::try_from(payload.len()).unwrap()
        ),
        Err(AtomicBlobStoreError::InputIo { .. })
    ));
    assert_eq!(
        store.load(b"source-failure").unwrap(),
        Some(b"old".to_vec())
    );
    store.close().unwrap();
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn tokio_streaming_boundary_matrix_matches_blocking_contracts() {
    use atomic_blob_store::tokio::AtomicBlobStore;

    let root = test_directory();
    let store = AtomicBlobStore::open(root.path(), "tokio-matrix", options())
        .await
        .unwrap();

    for size in BOUNDARY_SIZES {
        let key = format!("size-{size}");
        let payload = vec![size as u8; size];
        store
            .save_from(
                key.as_bytes(),
                &mut Cursor::new(&payload),
                u64::try_from(size).unwrap(),
            )
            .await
            .unwrap();
        let mut destination = TrackingWriter::default();
        let metadata = store
            .load_into(key.as_bytes(), &mut destination)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(metadata.payload_len, size as u64);
        assert_eq!(destination.bytes, payload);
        assert_eq!(destination.flushes, 0);
        assert_eq!(destination.shutdowns, 0);
    }

    let mut over_limit = Cursor::new(Vec::<u8>::new());
    assert!(matches!(
        store.save_from(b"over", &mut over_limit, MAXIMUM + 1).await,
        Err(AtomicBlobStoreError::BlobTooLarge { .. })
    ));

    store.save(b"preserved", b"old".to_vec()).await.unwrap();
    for (name, payload_size, declared) in [
        ("eof-before-data", 0, 1),
        ("eof-within-chunk", CHUNK - 1, CHUNK as u64),
        ("eof-at-boundary", CHUNK, CHUNK as u64 + 1),
    ] {
        let mut source = Cursor::new(vec![1; payload_size]);
        assert!(
            matches!(
                store
                    .save_from(name.as_bytes(), &mut source, declared)
                    .await,
                Err(AtomicBlobStoreError::InputEndedEarly { .. })
            ),
            "{name}"
        );
    }
    let mut trailing = Cursor::new(b"new!".to_vec());
    assert!(matches!(
        store.save_from(b"preserved", &mut trailing, 3).await,
        Err(AtomicBlobStoreError::InputHasTrailingData { .. })
    ));

    for fail_at in [0, CHUNK] {
        let payload = vec![3; CHUNK + 1];
        let mut source = ScriptedReader::new(payload.clone(), Some(fail_at));
        assert!(matches!(
            store
                .save_from(
                    b"preserved",
                    &mut source,
                    u64::try_from(payload.len()).unwrap()
                )
                .await,
            Err(AtomicBlobStoreError::InputIo { .. })
        ));
        assert_eq!(
            store.load(b"preserved").await.unwrap(),
            Some(b"old".to_vec())
        );
    }

    let payload = vec![7; CHUNK + 1];
    store.save(b"destination", payload).await.unwrap();
    for fail_after in [0, CHUNK] {
        let mut destination = TrackingWriter {
            fail_after: Some(fail_after),
            ..TrackingWriter::default()
        };
        assert!(matches!(
            store.load_into(b"destination", &mut destination).await,
            Err(AtomicBlobStoreError::OutputIo { .. })
        ));
        assert_eq!(destination.flushes, 0);
        assert_eq!(destination.shutdowns, 0);
    }

    std::fs::write(store.blob_path(b"invalid"), b"not-an-envelope").unwrap();
    let mut destination = TrackingWriter::default();
    assert!(store.load_into(b"invalid", &mut destination).await.is_err());
    assert!(destination.bytes.is_empty());
    assert_eq!(destination.flushes, 0);
    assert_eq!(destination.shutdowns, 0);
    store.close().await.unwrap();
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn blocking_and_tokio_scripts_produce_identical_canonical_bytes() {
    use atomic_blob_store::tokio::AtomicBlobStore;

    let root = test_directory();
    let blocking = BlockingAtomicBlobStore::open(root.path(), "blocking", options()).unwrap();
    let asynchronous = AtomicBlobStore::open(root.path(), "asynchronous", options())
        .await
        .unwrap();
    let payload = vec![0x5a; CHUNK + 9];

    let blocking_observation = {
        blocking
            .save_from(b"key", &mut Cursor::new(&payload), payload.len() as u64)
            .unwrap();
        let mut destination = TrackingWriter {
            fail_after: None,
            ..TrackingWriter::default()
        };
        let metadata = blocking
            .load_into(b"key", &mut destination)
            .unwrap()
            .unwrap();
        let inspection = blocking.inspect(b"key").unwrap();
        let canonical_path = blocking.blob_path(b"key");
        let canonical = std::fs::read(&canonical_path).unwrap();
        let early_eof = error_category(
            &blocking
                .save_from(b"early", &mut Cursor::new(b"x"), 2)
                .unwrap_err(),
        );
        let trailing = error_category(
            &blocking
                .save_from(b"trailing", &mut Cursor::new(b"xy"), 1)
                .unwrap_err(),
        );
        let mut failing_source = ScriptedReader::new(vec![1; CHUNK + 1], Some(CHUNK));
        let source_failure = error_category(
            &blocking
                .save_from(b"source", &mut failing_source, (CHUNK + 1) as u64)
                .unwrap_err(),
        );
        let mut failing_destination = TrackingWriter {
            fail_after: Some(CHUNK),
            ..TrackingWriter::default()
        };
        let destination_failure = error_category(
            &blocking
                .load_into(b"key", &mut failing_destination)
                .unwrap_err(),
        );
        blocking.clear(b"key").unwrap();
        let absent_after_clear = blocking.load(b"key").unwrap().is_none();
        blocking.close().unwrap();
        let after_close = error_category(&blocking.load(b"key").unwrap_err());
        ScriptObservation {
            metadata_len: metadata.payload_len,
            loaded: destination.bytes,
            canonical,
            canonical_filename: canonical_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            inspection_size: inspection.size.unwrap(),
            early_eof,
            trailing,
            source_failure,
            destination_failure,
            destination_flushes: failing_destination.flushes,
            destination_shutdowns: failing_destination.shutdowns,
            absent_after_clear,
            after_close,
        }
    };

    let asynchronous_observation = {
        asynchronous
            .save_from(b"key", &mut Cursor::new(&payload), payload.len() as u64)
            .await
            .unwrap();
        let mut destination = TrackingWriter::default();
        let metadata = asynchronous
            .load_into(b"key", &mut destination)
            .await
            .unwrap()
            .unwrap();
        let inspection = asynchronous.inspect(b"key").await.unwrap();
        let canonical_path = asynchronous.blob_path(b"key");
        let canonical = std::fs::read(&canonical_path).unwrap();
        let early_eof = error_category(
            &asynchronous
                .save_from(b"early", &mut Cursor::new(b"x"), 2)
                .await
                .unwrap_err(),
        );
        let trailing = error_category(
            &asynchronous
                .save_from(b"trailing", &mut Cursor::new(b"xy"), 1)
                .await
                .unwrap_err(),
        );
        let mut failing_source = ScriptedReader::new(vec![1; CHUNK + 1], Some(CHUNK));
        let source_failure = error_category(
            &asynchronous
                .save_from(b"source", &mut failing_source, (CHUNK + 1) as u64)
                .await
                .unwrap_err(),
        );
        let mut failing_destination = TrackingWriter {
            fail_after: Some(CHUNK),
            ..TrackingWriter::default()
        };
        let destination_failure = error_category(
            &asynchronous
                .load_into(b"key", &mut failing_destination)
                .await
                .unwrap_err(),
        );
        asynchronous.clear(b"key").await.unwrap();
        let absent_after_clear = asynchronous.load(b"key").await.unwrap().is_none();
        asynchronous.close().await.unwrap();
        let after_close = error_category(&asynchronous.load(b"key").await.unwrap_err());
        ScriptObservation {
            metadata_len: metadata.payload_len,
            loaded: destination.bytes,
            canonical,
            canonical_filename: canonical_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            inspection_size: inspection.size.unwrap(),
            early_eof,
            trailing,
            source_failure,
            destination_failure,
            destination_flushes: failing_destination.flushes,
            destination_shutdowns: failing_destination.shutdowns,
            absent_after_clear,
            after_close,
        }
    };

    assert_eq!(blocking_observation, asynchronous_observation);
}
