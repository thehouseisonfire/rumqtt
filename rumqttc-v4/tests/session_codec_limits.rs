use rumqttc::{PersistedAckMode, PersistedSession, SessionDecodeError};
#[test]
fn bounded_codecs_preserve_the_format_and_reject_before_large_allocations() {
    let session = PersistedSession {
        format_version: 2,
        client_id: "limits".into(),
        clean_session: false,
        max_inflight: 10,
        ack_mode: PersistedAckMode::Automatic,
        replay: vec![],
        incoming_qos2: vec![],
    };
    let bytes = session.encode().unwrap();
    let bounded = session.encode_with_limit(bytes.len()).unwrap();
    assert!(bounded.capacity() <= bytes.len());
    assert_eq!(bounded, bytes);
    assert!(session.encode_with_limit(bytes.len() - 1).is_err());
    assert!(session.encode_with_limit(0).is_err());
    assert!(matches!(
        PersistedSession::decode_with_memory_limit(&bytes, 0),
        Err(SessionDecodeError::LimitExceeded { .. })
    ));
    assert_eq!(
        PersistedSession::decode_with_memory_limit(&bytes, 1024).unwrap(),
        session
    );
}
