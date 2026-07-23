use atomic_blob_store::AtomicBlobStoreError;
use rumqttc_v5::{
    PersistedAckMode, PersistedSession, SessionDecodeError, SessionStore, SessionStoreKey,
};

use super::*;

fn session(last_pkid: u16) -> PersistedSession {
    PersistedSession {
        format_version: 1,
        client_id: "client".to_owned(),
        clean_start: false,
        session_expiry_interval: Some(60),
        outgoing_inflight_upper_limit: Some(10),
        ack_mode: PersistedAckMode::Automatic,
        last_pkid,
        last_puback: 0,
        replay: Vec::new(),
        incoming_qos2: Vec::new(),
    }
}

#[test]
fn golden_key_bytes_filename_and_namespace_are_stable() {
    let key = SessionStoreKey::new("scope", "client");
    assert_eq!(
        encode_session_store_key(&key).unwrap(),
        b"\x01\x05\x00\x00\x00\x05scope\x00\x00\x00\x06client"
    );
    assert_eq!(
        session_filename(&key).unwrap(),
        "0d9d0d150464f826fe9632a8ecd5520e381e3b04490afb307e47da3da7f6c198.session"
    );
    assert_eq!(SessionFileStore::namespace_name(), Path::new("v5"));
    assert_eq!(
        legacy_example_filename(&key),
        "73636f7065.636c69656e74.session"
    );
    assert_ne!(
        encode_session_store_key(&SessionStoreKey::new("ab", "c")).unwrap(),
        encode_session_store_key(&SessionStoreKey::new("a", "bc")).unwrap()
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn existing_checkpoint_round_trip_and_legacy_detection_work() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    store.save(&key, &session(7)).await.unwrap();
    assert_eq!(store.load(&key).await.unwrap().unwrap().last_pkid, 7);
    assert_eq!(store.inner.legacy_inspection_count(), 0);
    let legacy = root.path().join(legacy_example_filename(&key));
    std::fs::create_dir(&legacy).unwrap();
    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        CheckpointState::Present
    );
    assert_eq!(store.load(&key).await.unwrap().unwrap().last_pkid, 7);
    assert_eq!(store.inner.legacy_inspection_count(), 0);
    std::fs::remove_dir(&legacy).unwrap();
    store.clear(&key).await.unwrap();

    std::fs::write(&legacy, b"legacy").unwrap();
    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        CheckpointState::LegacyDetected
    );
    assert_eq!(store.inner.legacy_inspection_count(), 1);
    let error = store.load(&key).await.unwrap_err();
    assert_eq!(store.inner.legacy_inspection_count(), 2);
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::LegacyCheckpointDetected { diagnostic_path }) if diagnostic_path == &legacy
    ));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn checkpoint_fixture_is_byte_for_byte_stable() {
    const EXPECTED: &[u8] = &[
        0x52, 0x55, 0x4d, 0x51, 0x53, 0x45, 0x53, 0x53, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x2c, 0x52, 0x4d, 0x51, 0x53, 0x45, 0x53, 0x53, 0x05, 0x00, 0x01, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x06, 0x63, 0x6c, 0x69, 0x65, 0x6e, 0x74, 0x00, 0x01, 0x00, 0x00, 0x00,
        0x3c, 0x01, 0x00, 0x0a, 0x01, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x68, 0x6c, 0xc3, 0xbe,
    ];
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    std::fs::write(store.checkpoint_path(&key).unwrap(), EXPECTED).unwrap();
    assert_eq!(store.load(&key).await.unwrap().unwrap().last_pkid, 7);
    store.save(&key, &session(7)).await.unwrap();
    let actual = std::fs::read(store.checkpoint_path(&key).unwrap()).unwrap();
    assert_eq!(actual, EXPECTED);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn envelope_and_codec_errors_remain_distinct() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    store.save(&key, &session(7)).await.unwrap();
    let path = store.checkpoint_path(&key).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[18] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    let error = store.load(&key).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::FileStore(
            AtomicBlobStoreError::ChecksumMismatch { .. }
        ))
    ));

    bytes[18] ^= 1;
    bytes[18] = 0;
    let offset = bytes.len() - 4;
    let checksum = crc32c::crc32c(&bytes[..offset]);
    bytes[offset..].copy_from_slice(&checksum.to_be_bytes());
    std::fs::write(path, bytes).unwrap();
    let error = store.load(&key).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::SessionDecode(
            SessionDecodeError::InvalidMagic
        ))
    ));
}
