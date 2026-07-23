#![cfg(all(feature = "v4", feature = "v5", any(unix, windows)))]

use rumqttc_session_store_file::{CheckpointState, v4, v5};
use rumqttc_v4::{PersistedAckMode as V4AckMode, PersistedSession as V4Session};
use rumqttc_v4::{SessionStore as V4SessionStore, SessionStoreKey as V4Key};
use rumqttc_v5::{PersistedAckMode as V5AckMode, PersistedSession as V5Session};
use rumqttc_v5::{SessionStore as V5SessionStore, SessionStoreKey as V5Key};
use std::time::Duration;

#[tokio::test]
async fn both_protocols_are_type_safe_and_namespace_isolated() {
    let root = tempfile::tempdir().unwrap();
    let v4_store = v4::SessionFileStore::open(root.path()).await.unwrap();
    let v5_store = v5::SessionFileStore::open(root.path()).await.unwrap();
    let v4_key = V4Key::new("scope", "client");
    let v5_key = V5Key::new("scope", "client");
    let v4_session = V4Session {
        format_version: 1,
        client_id: "client".into(),
        clean_session: false,
        max_inflight: 10,
        ack_mode: V4AckMode::Automatic,
        last_pkid: 4,
        last_puback: 0,
        replay: Vec::new(),
        incoming_qos2: Vec::new(),
    };
    let v5_session = V5Session {
        format_version: 1,
        client_id: "client".into(),
        clean_start: false,
        session_expiry_interval: Some(60),
        outgoing_inflight_upper_limit: Some(10),
        ack_mode: V5AckMode::Automatic,
        last_pkid: 5,
        last_puback: 0,
        replay: Vec::new(),
        incoming_qos2: Vec::new(),
    };

    v4_store.save(&v4_key, &v4_session).await.unwrap();
    assert_eq!(
        v4_store.inspect(&v4_key).await.unwrap().state,
        CheckpointState::Present
    );
    assert_eq!(
        v5_store.inspect(&v5_key).await.unwrap().state,
        CheckpointState::Absent
    );
    v5_store.save(&v5_key, &v5_session).await.unwrap();

    assert_eq!(v4_store.load(&v4_key).await.unwrap().unwrap().last_pkid, 4);
    assert_eq!(v5_store.load(&v5_key).await.unwrap().unwrap().last_pkid, 5);
    assert_ne!(
        v4_store.checkpoint_path(&v4_key).unwrap(),
        v5_store.checkpoint_path(&v5_key).unwrap()
    );

    v4_store.operator_clear(&v4_key).await.unwrap();
    assert_eq!(
        v4_store.inspect(&v4_key).await.unwrap().state,
        CheckpointState::Absent
    );
    assert_eq!(
        v5_store.inspect(&v5_key).await.unwrap().state,
        CheckpointState::Present
    );
    assert!(v4_store.quarantine(&v4_key).await.is_err());
    assert_eq!(
        v5_store.inspect(&v5_key).await.unwrap().state,
        CheckpointState::Present
    );
    let quarantine = v5_store.quarantine(&v5_key).await.unwrap();
    assert!(quarantine.diagnostic_path.exists());
    assert_eq!(
        v5_store.inspect(&v5_key).await.unwrap().state,
        CheckpointState::Absent
    );
    assert!(
        v4_store
            .cleanup_stale_temporary_files_before_use(Duration::ZERO)
            .await
            .is_err()
    );
    assert!(
        v5_store
            .cleanup_stale_temporary_files_before_use(Duration::ZERO)
            .await
            .is_err()
    );
}
