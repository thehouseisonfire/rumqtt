#![cfg(any(unix, windows))]

mod common;

use atomic_blob_store::{
    AtomicBlobStoreError, AtomicBlobStoreOptions, BlobFormatIdentity, BlobMetadata,
    BlockingAtomicBlobStore, ENVELOPE_VERSION_V1,
};
use common::test_directory;

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

fn load_fixture(name: &str) -> (Result<Option<BlobMetadata>, AtomicBlobStoreError>, Vec<u8>) {
    let root = test_directory();
    let format = BlobFormatIdentity::new(b"BLOBTEST", ".blob", ENVELOPE_VERSION_V1).unwrap();
    let options = AtomicBlobStoreOptions::new(format).with_max_blob_size(1024);
    let store = BlockingAtomicBlobStore::open(root.path(), "fixtures", options).unwrap();
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
    let mut output = Vec::new();
    let result = store.load_into(b"fixture", &mut output);
    (result, output)
}

#[test]
fn immutable_v1_fixtures_define_compatibility() {
    let (result, output) = load_fixture("valid");
    assert_eq!(result.unwrap().unwrap().payload_len, 3);
    assert_eq!(output, b"abc");
    for name in [
        "truncated",
        "oversized",
        "checksum-invalid",
        "trailing-data",
        "wrong-domain",
        "unsupported-version",
    ] {
        let (result, output) = load_fixture(name);
        assert!(result.is_err(), "{name}");
        assert!(output.is_empty(), "{name} wrote output before validation");
    }
}
