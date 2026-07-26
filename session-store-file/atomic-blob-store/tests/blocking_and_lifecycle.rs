#![cfg(any(unix, windows))]

mod common;

use common::test_directory;
use std::io::Cursor;

use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore,
    ENVELOPE_VERSION_V1,
};

fn options() -> AtomicBlobStoreOptions {
    let format =
        BlobFormatIdentity::new(b"BLOCKING", ".blob", ENVELOPE_VERSION_V1).expect("valid format");
    AtomicBlobStoreOptions::new(format).with_max_blob_size(1024 * 1024)
}

#[test]
fn blocking_facade_streams_and_closes_idempotently() {
    let root = test_directory();
    let store = BlockingAtomicBlobStore::open(root.path(), "blocking", options()).unwrap();

    let mut source = Cursor::new(b"streamed payload");
    store.save_from(b"key", &mut source, 16).unwrap();
    let mut output = Vec::new();
    let metadata = store.load_into(b"key", &mut output).unwrap().unwrap();
    assert_eq!(metadata.payload_len, 16);
    assert_eq!(output, b"streamed payload");

    store.flush().unwrap();
    store.close().unwrap();
    store.close().unwrap();
    assert!(matches!(
        store.load(b"key"),
        Err(AtomicBlobStoreError::StoreClosed)
    ));
}

#[test]
fn close_from_one_clone_closes_every_clone() {
    let root = test_directory();
    let store = BlockingAtomicBlobStore::open(root.path(), "clones", options()).unwrap();
    let clone = store.clone();
    store.save(b"key", b"value".to_vec()).unwrap();
    clone.close().unwrap();
    assert!(matches!(
        store.inspect(b"key"),
        Err(AtomicBlobStoreError::StoreClosed)
    ));
}

#[test]
fn concurrent_close_callers_share_the_shutdown_outcome() {
    let root = test_directory();
    let store = BlockingAtomicBlobStore::open(root.path(), "concurrent-close", options()).unwrap();
    let first = store.clone();
    let second = store.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let first_barrier = barrier.clone();
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        first.close()
    });
    let second_barrier = barrier.clone();
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        second.close()
    });
    barrier.wait();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert!(matches!(
        store.load(b"key"),
        Err(AtomicBlobStoreError::StoreClosed)
    ));
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn blocking_and_tokio_facades_have_format_parity() {
    use atomic_blob_store::tokio::AtomicBlobStore;

    let blocking_root = test_directory();
    let async_root = test_directory();
    let blocking =
        BlockingAtomicBlobStore::open(blocking_root.path(), "parity", options()).unwrap();
    let asynchronous = AtomicBlobStore::open(async_root.path(), "parity", options())
        .await
        .unwrap();

    blocking.save(b"key", b"value".to_vec()).unwrap();
    asynchronous.save(b"key", b"value".to_vec()).await.unwrap();
    assert_eq!(
        std::fs::read(blocking.blob_path(b"key")).unwrap(),
        std::fs::read(asynchronous.blob_path(b"key")).unwrap()
    );
    asynchronous.close().await.unwrap();
    blocking.close().unwrap();
}
