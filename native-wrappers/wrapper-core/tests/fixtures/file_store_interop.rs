//! Native Rust half of the C process-restart interoperability fixture.
use std::path::Path;

macro_rules! cross_open {
    ($function:ident, $protocol:ident, $native:ident) => {
        fn $function(root: &Path) {
            use rumqttc_session_store_file::$protocol::BlockingSessionFileStore;
            use $native::{PersistedAckMode, PersistedQoS, PersistedRequest};

            let client_id = concat!("native-store-restart-", stringify!($protocol));
            let key = $native::SessionStoreKey::new("file-restart", client_id);
            let store = BlockingSessionFileStore::open(root).unwrap();
            let mut session = store.load(&key).unwrap().expect("C checkpoint is absent");
            assert_eq!(session.client_id, client_id);
            assert_eq!(session.ack_mode, PersistedAckMode::Automatic);
            assert_eq!(session.replay.len(), 3);
            assert_eq!(session.incoming_qos2.len(), 1);
            assert_eq!(session.incoming_qos2[0].pkid, 100);

            let mut qos1 = false;
            let mut qos2 = false;
            let mut subscribe = false;
            for request in &mut session.replay {
                match request {
                    PersistedRequest::Publish(publish) => {
                        assert!(publish.dup);
                        assert_ne!(publish.pkid, 0);
                        assert_eq!(publish.payload, b"persist");
                        match publish.qos {
                            PersistedQoS::AtLeastOnce => {
                                assert!(!qos1);
                                qos1 = true;
                                assert_eq!(publish.topic, b"rumqttc/native/restart/qos1");
                            }
                            PersistedQoS::ExactlyOnce => {
                                assert!(!qos2);
                                qos2 = true;
                                assert_eq!(publish.topic, b"rumqttc/native/restart/qos2");
                            }
                            _ => panic!("unexpected persisted QoS"),
                        }
                        // The broker requires this marker after C recovery, proving
                        // that it consumed the Rust writer's replacement checkpoint.
                        publish.payload = b"rust-ok".to_vec();
                    }
                    PersistedRequest::Subscribe(request) => {
                        assert!(!subscribe);
                        subscribe = true;
                        assert_ne!(request.pkid, 0);
                        assert_eq!(request.filters.len(), 1);
                        assert_eq!(request.filters[0].path, "rumqttc/native/restart/incoming");
                        assert_eq!(request.filters[0].qos, PersistedQoS::ExactlyOnce);
                    }
                    _ => panic!("unexpected persisted request"),
                }
            }
            assert!(qos1 && qos2 && subscribe);
            store.save(&key, &session).unwrap();
            assert_eq!(store.load(&key).unwrap().as_ref(), Some(&session));
            store.close().unwrap();
        }
    };
}

cross_open!(v4, v4, rumqttc_v4);
cross_open!(v5, v5, rumqttc_v5);

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let protocol = arguments.next().expect("protocol argument is required");
    let root = arguments.next().expect("root argument is required");
    assert!(arguments.next().is_none(), "unexpected argument");
    if protocol == "v4" {
        v4(Path::new(&root));
    } else if protocol == "v5" {
        v5(Path::new(&root));
    } else {
        panic!("expected v4 or v5");
    }
}
