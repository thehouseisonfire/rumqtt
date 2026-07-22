use rumqttc::SessionStoreKey;
#[cfg(any(unix, windows))]
use rumqttc::{PersistedAckMode, PersistedSession, SessionDecodeError, SessionStore};
#[cfg(any(unix, windows))]
use rumqttc_session_store_file_core_next::FileStoreError;

use super::*;

#[cfg(any(unix, windows))]
fn session(client_id: &str, last_pkid: u16) -> PersistedSession {
    PersistedSession {
        format_version: 1,
        client_id: client_id.to_owned(),
        clean_session: false,
        max_inflight: 10,
        ack_mode: PersistedAckMode::Automatic,
        last_pkid,
        last_puback: 0,
        replay: Vec::new(),
        incoming_qos2: Vec::new(),
    }
}

#[test]
fn golden_key_bytes_and_filename_are_stable() {
    let key = SessionStoreKey::new("scope", "client");
    assert_eq!(
        encode_session_store_key(&key).unwrap(),
        b"\x01\x04\x00\x00\x00\x05scope\x00\x00\x00\x06client"
    );
    assert_eq!(
        session_filename(&key).unwrap(),
        "411b1d64b8f554f204d7fdf4578ac7fe7b3b550f157be36f4bf5816ff77bb46e.session"
    );
}

#[test]
fn key_encoding_is_unambiguous_safe_and_protocol_tagged() {
    let identifiers = [
        "a/b",
        "a\\b",
        "..",
        "x:y",
        "客户端",
        "CON",
        "client",
        "Client",
    ];
    for identifier in identifiers {
        let key = SessionStoreKey::new("scope", identifier);
        let bytes = encode_session_store_key(&key).unwrap();
        assert_eq!(&bytes[..2], &[1, 4]);
        let filename = session_filename(&key).unwrap();
        assert_eq!(filename.len(), 72);
        assert!(!filename.contains(identifier));
        assert!(!filename.contains('/'));
        assert!(!filename.contains('\\'));
    }
    let long = "z".repeat(128 * 1024);
    let key = SessionStoreKey::new("scope", &long);
    assert_eq!(
        encode_session_store_key(&key).unwrap().len(),
        long.len() + 15
    );
    assert_eq!(
        encode_session_store_key(&SessionStoreKey::new("ab", "c")).unwrap(),
        encode_session_store_key(&SessionStoreKey::new("ab", "c")).unwrap()
    );
    assert_ne!(
        encode_session_store_key(&SessionStoreKey::new("ab", "c")).unwrap(),
        encode_session_store_key(&SessionStoreKey::new("a", "bc")).unwrap()
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn exact_legacy_file_is_reported_without_scanning_or_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    let legacy = root.path().join(legacy_example_filename(&key));
    std::fs::write(&legacy, b"legacy bytes").unwrap();

    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        rumqttc_session_store_file_core_next::CheckpointState::LegacyDetected
    );
    let error = store.load(&key).await.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<SessionFileStoreError>(), Some(SessionFileStoreError::LegacyCheckpointDetected { diagnostic_path }) if diagnostic_path == &legacy)
    );
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy bytes");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn unrepresentable_legacy_filename_does_not_break_a_fresh_key() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("", "x".repeat(128));
    assert!(legacy_example_filename(&key).len() > 255);

    assert_eq!(
        store.inspect(&key).await.unwrap().state,
        CheckpointState::Absent
    );
    assert_eq!(store.load(&key).await.unwrap(), None);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn adapter_round_trip_replace_clear_missing_and_semantics_remain_external() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    assert_eq!(store.load(&key).await.unwrap(), None);
    store.save(&key, &session("client", 1)).await.unwrap();
    assert_eq!(store.load(&key).await.unwrap().unwrap().last_pkid, 1);
    store
        .save(&key, &session("different-client", 2))
        .await
        .unwrap();
    let loaded = store.load(&key).await.unwrap().unwrap();
    assert_eq!(loaded.client_id, "different-client");
    assert_eq!(loaded.last_pkid, 2);
    store.clear(&key).await.unwrap();
    assert_eq!(store.load(&key).await.unwrap(), None);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn envelope_and_codec_errors_remain_distinct() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionFileStore::open(root.path()).await.unwrap();
    let key = SessionStoreKey::new("scope", "client");
    store.save(&key, &session("client", 1)).await.unwrap();
    let path = store.checkpoint_path(&key).unwrap();

    let mut bytes = std::fs::read(&path).unwrap();
    bytes[18] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    let error = store.load(&key).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::FileStore(
            FileStoreError::ChecksumMismatch { .. }
        ))
    ));

    store.save(&key, &session("client", 1)).await.unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[18] ^= 1;
    recompute_outer_crc(&mut bytes);
    std::fs::write(&path, &bytes).unwrap();
    let error = store.load(&key).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::SessionDecode(
            SessionDecodeError::InvalidMagic
        ))
    ));

    store.save(&key, &session("client", 1)).await.unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[18 + 8] = 0;
    bytes[18 + 9] = 2;
    recompute_outer_crc(&mut bytes);
    std::fs::write(path, &bytes).unwrap();
    let error = store.load(&key).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SessionFileStoreError>(),
        Some(SessionFileStoreError::SessionDecode(
            SessionDecodeError::UnsupportedCodecVersion { actual: 2 }
        ))
    ));
}

#[cfg(any(unix, windows))]
fn recompute_outer_crc(bytes: &mut [u8]) {
    let checksum = crc32c::crc32c(&bytes[..bytes.len() - 4]);
    let offset = bytes.len() - 4;
    bytes[offset..].copy_from_slice(&checksum.to_be_bytes());
}
