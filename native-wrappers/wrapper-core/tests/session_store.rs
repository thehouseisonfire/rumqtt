use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc_wrapper_core::{
    ClientConfig, Command, NativeClient, ProtocolConfig, SessionCheckpoint, SessionStore,
    SessionStoreConfig, SessionStoreKey, StoreFailure, StoreFuture, WrapperEvent,
};

#[derive(Default)]
struct MemoryStore {
    checkpoint: Mutex<Option<SessionCheckpoint>>,
    failure: Option<StoreFailure>,
    load_entered: Option<std::sync::mpsc::Sender<()>>,
}

impl SessionStore for MemoryStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        if let Some(entered) = &self.load_entered {
            entered.send(()).unwrap();
        }
        let checkpoint = self.checkpoint.lock().unwrap().clone();
        let failure = self.failure;
        Box::pin(async move {
            match failure {
                Some(StoreFailure::Panic) => panic!("host secret must not be logged"),
                Some(StoreFailure::Timeout) => std::future::pending().await,
                Some(StoreFailure::Save | StoreFailure::Clear) => Ok(checkpoint),
                Some(failure) => Err(failure),
                None => Ok(checkpoint),
            }
        })
    }
    fn save(&self, _: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
        if self.failure == Some(StoreFailure::Save) {
            return Box::pin(async { Err(StoreFailure::Save) });
        }
        *self.checkpoint.lock().unwrap() = Some(checkpoint);
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        if self.failure == Some(StoreFailure::Clear) {
            return Box::pin(async { Err(StoreFailure::Clear) });
        }
        *self.checkpoint.lock().unwrap() = None;
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn checkpoint_and_clear_failures_terminate_and_release_the_store() {
    use rumqttc_wrapper_core::{BrokerTarget, PublishCommand, PublishProtocolOptions, QoS};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    for mqtt5 in [false, true] {
        for failure in [StoreFailure::Save, StoreFailure::Clear] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut header = [0; 2];
                socket.read_exact(&mut header).unwrap();
                assert_eq!(header[0], 0x10);
                assert!(header[1] < 128);
                socket
                    .read_exact(&mut vec![0; usize::from(header[1])])
                    .unwrap();
                socket
                    .write_all(if mqtt5 {
                        &[0x20, 3, 0, 0, 0]
                    } else {
                        &[0x20, 2, 0, 0]
                    })
                    .unwrap();
                // Persistence must fail closed. No publish may reach the broker.
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 0);
            });
            let store = Arc::new(MemoryStore {
                failure: Some(failure),
                ..Default::default()
            });
            let weak = Arc::downgrade(&store);
            let mut config = config(mqtt5, store);
            config.common.broker = BrokerTarget::Tcp {
                host: "127.0.0.1".into(),
                port,
            };
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let mut publish = None;
            loop {
                match events
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .unwrap()
                {
                    WrapperEvent::Connected { .. } => {
                        publish = Some(
                            client
                                .handle()
                                .try_admit(Command::Publish(PublishCommand {
                                    topic: "persisted".into(),
                                    payload: b"payload".as_slice().into(),
                                    qos: QoS::AtLeastOnce,
                                    retain: false,
                                    protocol: PublishProtocolOptions::VersionNeutral,
                                }))
                                .unwrap(),
                        );
                    }
                    WrapperEvent::DriverTerminated(error) => {
                        assert_eq!(error.store_failure(), Some(failure));
                        break;
                    }
                    _ => {}
                }
            }
            client.join(Duration::from_secs(3)).unwrap();
            drop(publish);
            assert!(weak.upgrade().is_none());
            broker.join().unwrap();
        }
    }
}

fn config(mqtt5: bool, store: Arc<dyn SessionStore>) -> ClientConfig {
    let mut config = if mqtt5 {
        ClientConfig::v5("persist", "127.0.0.1", 1)
    } else {
        ClientConfig::v4("persist", "127.0.0.1", 1)
    };
    let mut store = SessionStoreConfig::new(store, "tenant");
    store.timeout = Duration::from_millis(50);
    match &mut config.protocol {
        ProtocolConfig::V4(v4) => {
            v4.clean_session = false;
            v4.session_store = Some(store);
        }
        ProtocolConfig::V5(v5) => {
            v5.clean_start = false;
            v5.connect_properties.session_expiry_interval = Some(60);
            v5.session_store = Some(store);
        }
    }
    config
}

#[test]
fn callback_failure_panic_and_timeout_are_typed_and_release_the_owner() {
    for mqtt5 in [false, true] {
        for failure in [
            StoreFailure::Load,
            StoreFailure::Panic,
            StoreFailure::Timeout,
        ] {
            let store = Arc::new(MemoryStore {
                failure: Some(failure),
                ..Default::default()
            });
            let weak = Arc::downgrade(&store);
            let mut client = NativeClient::start(config(mqtt5, store)).unwrap();
            let mut events = client.take_events().unwrap();
            let Some(WrapperEvent::DriverTerminated(error)) =
                events.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("missing terminal failure")
            };
            assert_eq!(error.store_failure(), Some(failure));
            assert!(!format!("{error:?}").contains("host secret"));
            client.join(Duration::from_secs(2)).unwrap();
            assert!(weak.upgrade().is_none());
        }
    }
}

#[test]
fn duplicate_key_is_rejected_until_driver_releases_lease() {
    for mqtt5 in [false, true] {
        let store = Arc::new(MemoryStore::default());
        let config = config(mqtt5, store);
        let mut first = NativeClient::start(config.clone()).unwrap();
        let _events = first.take_events().unwrap();
        let error = NativeClient::start(config.clone()).unwrap_err();
        assert_eq!(error.store_failure(), Some(StoreFailure::InUse));
        first
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        first.join(Duration::from_secs(2)).unwrap();
        let mut second = NativeClient::start(config).unwrap();
        let _events = second.take_events().unwrap();
        second
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        second.join(Duration::from_secs(2)).unwrap();
    }
}

#[test]
fn pending_load_is_cancelled_by_immediate_shutdown() {
    for mqtt5 in [false, true] {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let store = Arc::new(MemoryStore {
            failure: Some(StoreFailure::Timeout),
            load_entered: Some(entered_tx),
            ..Default::default()
        });
        let weak = Arc::downgrade(&store);
        let mut config = config(mqtt5, store);
        match &mut config.protocol {
            ProtocolConfig::V4(v4) => {
                v4.session_store.as_mut().unwrap().timeout = Duration::from_secs(60);
            }
            ProtocolConfig::V5(v5) => {
                v5.session_store.as_mut().unwrap().timeout = Duration::from_secs(60);
            }
        }
        let mut client = NativeClient::start(config).unwrap();
        let _events = client.take_events().unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        client
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        client.join(Duration::from_secs(2)).unwrap();
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn malformed_envelopes_are_rejected_without_network_io() {
    for mqtt5 in [false, true] {
        let protocol = if mqtt5 { 5 } else { 4 };
        for (bytes, expected) in [
            (vec![0; 65], StoreFailure::Oversized),
            (vec![0; 8], StoreFailure::Corrupt),
            (
                [b"RMWC".as_slice(), &[0, 2, protocol, 0]].concat(),
                StoreFailure::Version,
            ),
            (
                [b"RMWC".as_slice(), &[0, 1, 9, 0]].concat(),
                StoreFailure::Protocol,
            ),
            (
                [b"RMWC".as_slice(), &[0, 1, protocol, 0]].concat(),
                StoreFailure::Corrupt,
            ),
        ] {
            let store = Arc::new(MemoryStore {
                checkpoint: Mutex::new(Some(SessionCheckpoint(bytes.into()))),
                failure: None,
                ..Default::default()
            });
            let mut config = config(mqtt5, store);
            match &mut config.protocol {
                ProtocolConfig::V4(v4) => {
                    v4.session_store.as_mut().unwrap().max_checkpoint_size = 64
                }
                ProtocolConfig::V5(v5) => {
                    v5.session_store.as_mut().unwrap().max_checkpoint_size = 64
                }
            }
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let Some(WrapperEvent::DriverTerminated(error)) =
                events.recv_timeout(Duration::from_secs(2)).unwrap()
            else {
                panic!("missing terminal failure")
            };
            assert_eq!(error.store_failure(), Some(expected));
            client.join(Duration::from_secs(2)).unwrap();
        }
    }
}

#[test]
fn restart_replays_unacknowledged_publish_with_original_packet_identifier() {
    use rumqttc_wrapper_core::{BrokerTarget, PublishCommand, PublishProtocolOptions, QoS};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        let header = byte[0];
        let mut length = 0;
        let mut shift = 0;
        loop {
            stream.read_exact(&mut byte).unwrap();
            length |= usize::from(byte[0] & 127) << shift;
            if byte[0] < 128 {
                break;
            }
            shift += 7;
            assert!(shift < 28);
        }
        assert!(length < 65536);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        (header, body)
    }

    for mqtt5 in [false, true] {
        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            let store = Arc::new(MemoryStore::default());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (observed_tx, observed_rx) = std::sync::mpsc::channel();
            let broker = std::thread::spawn(move || {
                let mut first_id = None;
                for generation in 0..2 {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    assert_eq!(frame(&mut socket).0, 0x10);
                    let session = u8::from(generation == 1);
                    if mqtt5 {
                        socket.write_all(&[0x20, 3, session, 0, 0]).unwrap();
                    } else {
                        socket.write_all(&[0x20, 2, session, 0]).unwrap();
                    }
                    let (header, body) = frame(&mut socket);
                    assert_eq!(header >> 4, 3);
                    assert_eq!(header & 8 != 0, generation == 1);
                    let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
                    let id = [body[topic_len + 2], body[topic_len + 3]];
                    if generation == 0 {
                        first_id = Some(id);
                    } else {
                        assert_eq!(Some(id), first_id);
                        let ack = if qos == QoS::AtLeastOnce { 0x40 } else { 0x50 };
                        socket.write_all(&[ack, 2, id[0], id[1]]).unwrap();
                        if qos == QoS::ExactlyOnce {
                            assert_eq!(frame(&mut socket).0, 0x62);
                            socket.write_all(&[0x70, 2, id[0], id[1]]).unwrap();
                        }
                    }
                    observed_tx.send(()).unwrap();
                    assert_eq!(frame(&mut socket).0, 0xe0);
                }
            });
            let mut config = config(mqtt5, store.clone());
            config.common.broker = BrokerTarget::Tcp {
                host: "127.0.0.1".into(),
                port,
            };
            let mut first = NativeClient::start(config.clone()).unwrap();
            let mut events = first.take_events().unwrap();
            assert!(matches!(
                events.recv_timeout(Duration::from_secs(2)).unwrap(),
                Some(WrapperEvent::Connected { .. })
            ));
            let operation = first
                .handle()
                .try_admit(Command::Publish(PublishCommand {
                    topic: "persisted".into(),
                    payload: b"payload".as_slice().into(),
                    qos,
                    retain: false,
                    protocol: PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            first
                .handle()
                .try_admit(Command::ImmediateDisconnect)
                .unwrap();
            first.join(Duration::from_secs(2)).unwrap();
            assert!(
                operation
                    .completion
                    .wait_timeout(Duration::from_secs(1))
                    .is_err()
            );
            assert!(store.checkpoint.lock().unwrap().is_some());
            let mut second = NativeClient::start(config).unwrap();
            let mut events = second.take_events().unwrap();
            assert!(matches!(
                events.recv_timeout(Duration::from_secs(2)).unwrap(),
                Some(WrapperEvent::Connected {
                    session_present: true,
                    ..
                })
            ));
            observed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            second
                .handle()
                .try_admit(Command::GracefulDisconnect {
                    timeout: Some(Duration::from_secs(2)),
                })
                .unwrap();
            second.join(Duration::from_secs(3)).unwrap();
            broker.join().unwrap();
        }
    }
}
