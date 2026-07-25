#![cfg(any(unix, windows))]

use std::io::{self, Cursor, Write};

use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore,
    ENVELOPE_VERSION_V1,
};

const MAXIMUM: u64 = 2 * 64 * 1024 + 17;

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

#[test]
fn blocking_streaming_boundary_matrix_is_bounded_and_preserves_destination_ownership() {
    let root = tempfile::tempdir().unwrap();
    let store = BlockingAtomicBlobStore::open(root.path(), "blocking-matrix", options()).unwrap();

    for size in [0, 1, 64 * 1024, 64 * 1024 + 1, MAXIMUM as usize] {
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
    let mut failing = TrackingWriter {
        fail_after: Some(1),
        ..TrackingWriter::default()
    };
    assert!(matches!(
        store.load_into(b"destination", &mut failing),
        Err(AtomicBlobStoreError::OutputIo { .. })
    ));
    assert_eq!(failing.flushes, 0);
    store.close().unwrap();
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn blocking_and_tokio_scripts_produce_identical_canonical_bytes() {
    use atomic_blob_store::tokio::AtomicBlobStore;

    let root = tempfile::tempdir().unwrap();
    let blocking = BlockingAtomicBlobStore::open(root.path(), "blocking", options()).unwrap();
    let asynchronous = AtomicBlobStore::open(root.path(), "asynchronous", options())
        .await
        .unwrap();
    let payload = vec![0x5a; 64 * 1024 + 9];
    blocking
        .save_from(b"key", &mut Cursor::new(&payload), payload.len() as u64)
        .unwrap();
    asynchronous
        .save_from(b"key", &mut Cursor::new(&payload), payload.len() as u64)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(blocking.blob_path(b"key")).unwrap(),
        std::fs::read(asynchronous.blob_path(b"key")).unwrap()
    );
    blocking.close().unwrap();
    asynchronous.close().await.unwrap();
}
