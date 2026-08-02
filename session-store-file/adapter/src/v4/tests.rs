use atomic_blob_store::AtomicBlobStoreError;
use rumqttc_v4::{
    PersistedAckMode, PersistedPubRel, PersistedRequest, PersistedSession, SessionDecodeError,
    SessionStore, SessionStoreKey,
};

use super::*;

fn session(last_pkid: u16) -> PersistedSession {
    PersistedSession {
        format_version: 2,
        client_id: "client".to_owned(),
        clean_session: false,
        max_inflight: 10,
        ack_mode: PersistedAckMode::Automatic,
        replay: vec![PersistedRequest::PubRel(PersistedPubRel {
            pkid: last_pkid,
        })],
        incoming_qos2: Vec::new(),
    }
}

#[test]
fn golden_key_bytes_filename_and_namespace_are_stable() {
    let key = SessionStoreKey::new("scope", "client");
    assert_eq!(
        encode_session_store_key(&key).unwrap(),
        b"\x01\x04\x00\x00\x00\x05scope\x00\x00\x00\x06client"
    );
    assert_eq!(
        session_filename(&key).unwrap(),
        "411b1d64b8f554f204d7fdf4578ac7fe7b3b550f157be36f4bf5816ff77bb46e.session"
    );
    assert_eq!(SessionFileStore::namespace_name(), Path::new("v4"));
    assert_ne!(
        encode_session_store_key(&SessionStoreKey::new("ab", "c")).unwrap(),
        encode_session_store_key(&SessionStoreKey::new("a", "bc")).unwrap()
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn existing_checkpoint_round_trip_works() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    store.save(&key, &session(7)).await.unwrap();
    assert_eq!(store.load(&key).await.unwrap().unwrap(), session(7));
    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        CheckpointState::Present
    );
    assert_eq!(store.load(&key).await.unwrap().unwrap(), session(7));
    store.clear(&key).await.unwrap();
    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        CheckpointState::Absent
    );
    assert!(store.load(&key).await.unwrap().is_none());
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn checkpoint_fixture_is_byte_for_byte_stable() {
    const EXPECTED: &[u8] = &[
        0x52, 0x55, 0x4d, 0x51, 0x53, 0x45, 0x53, 0x53, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x25, 0x52, 0x4d, 0x51, 0x53, 0x45, 0x53, 0x53, 0x04, 0x00, 0x02, 0x00, 0x02,
        0x00, 0x00, 0x00, 0x06, 0x63, 0x6c, 0x69, 0x65, 0x6e, 0x74, 0x00, 0x00, 0x0a, 0x01, 0x00,
        0x00, 0x00, 0x01, 0x02, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x6a, 0x63, 0xf7, 0x10,
    ];
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
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
