#![cfg(any(unix, windows))]

use atomic_blob_store::{
    AtomicBlobStore, AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity,
    ENVELOPE_VERSION_V1,
};

fn decode_hex(source: &str) -> Vec<u8> {
    let source = source.trim().as_bytes();
    source
        .chunks_exact(2)
        .map(|pair| {
            let digit = |value: u8| match value {
                b'0'..=b'9' => value - b'0',
                b'a'..=b'f' => value - b'a' + 10,
                _ => panic!("fixture is lowercase hexadecimal"),
            };
            digit(pair[0]) << 4 | digit(pair[1])
        })
        .collect()
}

async fn load_fixture(name: &str) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
    let root = tempfile::tempdir().unwrap();
    let format = BlobFormatIdentity::new(b"BLOBTEST", ".blob", ENVELOPE_VERSION_V1).unwrap();
    let options = AtomicBlobStoreOptions::new(format).with_max_blob_size(1024);
    let store = AtomicBlobStore::open(root.path(), "fixtures", options)
        .await
        .unwrap();
    let fixture = match name {
        "valid" => include_str!("fixtures/v1/valid.hex"),
        "truncated" => include_str!("fixtures/v1/truncated.hex"),
        "oversized" => include_str!("fixtures/v1/oversized.hex"),
        "checksum-invalid" => include_str!("fixtures/v1/checksum-invalid.hex"),
        "trailing-data" => include_str!("fixtures/v1/trailing-data.hex"),
        "wrong-domain" => include_str!("fixtures/v1/wrong-domain.hex"),
        "unsupported-version" => include_str!("fixtures/v1/unsupported-version.hex"),
        _ => unreachable!(),
    };
    std::fs::write(store.blob_path(b"fixture"), decode_hex(fixture)).unwrap();
    store.load(b"fixture").await
}

#[tokio::test]
async fn immutable_v1_fixtures_define_compatibility() {
    assert_eq!(load_fixture("valid").await.unwrap(), Some(b"abc".to_vec()));
    for name in [
        "truncated",
        "oversized",
        "checksum-invalid",
        "trailing-data",
        "wrong-domain",
        "unsupported-version",
    ] {
        assert!(load_fixture(name).await.is_err(), "{name}");
    }
}
