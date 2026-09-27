use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use rumqttc::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::Semaphore;

const LIMIT: Duration = Duration::from_secs(3);

#[derive(Debug)]
struct ControlledStore {
    checkpoint: Mutex<Option<PersistedSession>>,
    calls: Mutex<Vec<&'static str>>,
    fail_clear: AtomicBool,
    block_clear: AtomicBool,
    started: Semaphore,
    release: Semaphore,
}

impl ControlledStore {
    fn seeded() -> Arc<Self> {
        Arc::new(Self {
            checkpoint: Mutex::new(Some(PersistedSession {
                format_version: 2,
                client_id: "compatibility".into(),
                clean_session: false,
                max_inflight: 100,
                ack_mode: PersistedAckMode::Automatic,
                replay: vec![PersistedRequest::Publish(PersistedPublish {
                    dup: true,
                    qos: PersistedQoS::AtLeastOnce,
                    retain: false,
                    topic: b"old".to_vec(),
                    pkid: 7,
                    payload: b"stale".to_vec(),
                })],
                incoming_qos2: vec![],
            })),
            calls: Mutex::default(),
            fail_clear: AtomicBool::new(false),
            block_clear: AtomicBool::new(false),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }

    fn record(&self, operation: &'static str) {
        self.calls.lock().unwrap().push(operation);
    }
}

impl SessionStore for ControlledStore {
    fn load<'a>(
        &'a self,
        key: &'a SessionStoreKey,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PersistedSession>, SessionStoreError>> + Send + 'a>,
    > {
        Box::pin(async move {
            assert_eq!(key.client_id(), "compatibility");
            assert_eq!(key.scope(), "test-scope");
            self.record("load");
            Ok(self.checkpoint.lock().unwrap().clone())
        })
    }

    fn save<'a>(
        &'a self,
        _key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
        Box::pin(async move {
            self.record("save");
            *self.checkpoint.lock().unwrap() = Some(session.clone());
            Ok(())
        })
    }

    fn clear<'a>(
        &'a self,
        key: &'a SessionStoreKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(key.client_id(), "compatibility");
            assert_eq!(key.scope(), "test-scope");
            self.record("clear");
            self.started.add_permits(1);
            if self.block_clear.load(Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            if self.fail_clear.swap(false, Ordering::SeqCst) {
                return Err(Box::new(std::io::Error::other("clear failed")) as SessionStoreError);
            }
            *self.checkpoint.lock().unwrap() = None;
            Ok(())
        })
    }
}

async fn read_packet(peer: &mut DuplexStream) -> Packet {
    let first = peer.read_u8().await.unwrap();
    let mut bytes = BytesMut::from(&[first][..]);
    let mut remaining = 0;
    let mut multiplier = 1;
    loop {
        let byte = peer.read_u8().await.unwrap();
        bytes.extend_from_slice(&[byte]);
        remaining += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining];
    peer.read_exact(&mut body).await.unwrap();
    bytes.extend_from_slice(&body);
    Packet::read(&mut bytes, 4096).unwrap()
}

fn options(store: Option<Arc<ControlledStore>>) -> (MqttOptions, flume::Receiver<DuplexStream>) {
    let (tx, rx) = flume::unbounded();
    let mut options = MqttOptions::new("compatibility", "localhost");
    options
        .protocol_compatibility_mut()
        .set_session_present_mismatch(SessionPresentMismatchPolicy::AcceptAsClean);
    if let Some(store) = &store {
        options
            .set_session_store_arc(store.clone())
            .set_session_store_scope("test-scope");
    }
    options.set_socket_connector(move |_, _| {
        let tx = tx.clone();
        let store = store.clone();
        async move {
            if let Some(store) = store {
                store.record("connect");
            }
            let (client, peer) = tokio::io::duplex(4096);
            tx.send(peer).unwrap();
            Ok(client)
        }
    });
    (options, rx)
}

fn broker(rx: flume::Receiver<DuplexStream>, flags: Vec<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for flag in flags {
            let mut peer = rx.recv_async().await.unwrap();
            assert!(matches!(read_packet(&mut peer).await, Packet::Connect(_)));
            peer.write_all(&[0x20, 2, u8::from(flag), 0]).await.unwrap();
            // Keep the candidate transport alive until the client commits or drops it.
            peer.read_to_end(&mut Vec::new()).await.unwrap();
        }
    })
}

async fn poll(eventloop: &mut EventLoop) -> Result<Event, ConnectionError> {
    tokio::time::timeout(LIMIT, eventloop.poll())
        .await
        .expect("poll deadline")
}

fn assert_accepted(eventloop: &EventLoop, event: Event) {
    assert!(matches!(event, Event::Incoming(Packet::ConnAck(c)) if c.session_present));
    let diagnostics = eventloop.diagnostics();
    assert!(diagnostics.connected);
    let session = diagnostics.session.connack.unwrap();
    assert!(session.raw_session_present);
    assert!(!session.session_resumed);
    assert_eq!(
        session.diagnostic,
        Some(ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean)
    );
    assert!(!diagnostics.session.session_store_clear_pending);
}

#[tokio::test]
async fn default_policy_still_rejects_invalid_clean_session_connack() {
    let (mut options, rx) = options(None);
    options.set_protocol_compatibility(ProtocolCompatibility::default());
    let task = broker(rx, vec![true]);
    let mut eventloop = EventLoop::new(options, 10);
    assert!(matches!(
        poll(&mut eventloop).await,
        Err(ConnectionError::SessionStateMismatch { .. })
    ));
    assert!(!eventloop.diagnostics().connected);
    assert!(eventloop.diagnostics().session.connack.is_none());
    tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn accepted_connection_is_published_only_after_checkpoint_clear_succeeds() {
    let store = ControlledStore::seeded();
    store.block_clear.store(true, Ordering::SeqCst);
    let (options, rx) = options(Some(store.clone()));
    let task = broker(rx, vec![true]);
    let mut eventloop = EventLoop::new(options, 10);
    let mut candidate = Box::pin(eventloop.poll());
    tokio::select! {
        result = &mut candidate => panic!("connected before clear: {result:?}"),
        permit = store.started.acquire() => permit.unwrap().forget(),
        () = tokio::time::sleep(LIMIT) => panic!("clear did not start"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut candidate)
            .await
            .is_err()
    );
    assert!(store.checkpoint.lock().unwrap().is_some());
    assert_eq!(*store.calls.lock().unwrap(), ["connect", "clear"]);
    store.release.add_permits(1);
    let event = tokio::time::timeout(LIMIT, candidate)
        .await
        .unwrap()
        .unwrap();
    assert_accepted(&eventloop, event);
    assert!(store.checkpoint.lock().unwrap().is_none());
    drop(eventloop);
    tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn failed_clear_blocks_checkpoint_reload_and_retries_before_next_connect() {
    let store = ControlledStore::seeded();
    store.fail_clear.store(true, Ordering::SeqCst);
    let (options, rx) = options(Some(store.clone()));
    let task = broker(rx, vec![true, false]);
    let mut eventloop = EventLoop::new(options, 10);
    assert!(matches!(
        poll(&mut eventloop).await,
        Err(ConnectionError::SessionStore(_))
    ));
    let diagnostics = eventloop.diagnostics();
    assert!(!diagnostics.connected);
    assert!(diagnostics.session.connack.is_none());
    assert!(diagnostics.session.session_store_clear_pending);
    assert!(store.checkpoint.lock().unwrap().is_some());
    // Even a switch back to persistence cannot reload the stale checkpoint.
    eventloop.mqtt_options.set_clean_session(false);
    assert!(
        matches!(poll(&mut eventloop).await.unwrap(), Event::Incoming(Packet::ConnAck(c)) if !c.session_present)
    );
    assert_eq!(
        *store.calls.lock().unwrap(),
        ["connect", "clear", "clear", "connect", "clear"]
    );
    assert!(store.checkpoint.lock().unwrap().is_none());
    assert!(eventloop.pending_is_empty());
    assert!(!eventloop.diagnostics().session.session_store_clear_pending);
    drop(eventloop);
    tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn cancelled_clear_keeps_obligation_and_candidate_connection_is_discarded() {
    let store = ControlledStore::seeded();
    store.block_clear.store(true, Ordering::SeqCst);
    let (options, rx) = options(Some(store.clone()));
    let task = broker(rx, vec![true, true]);
    let mut eventloop = EventLoop::new(options, 10);
    let mut candidate = Box::pin(eventloop.poll());
    tokio::select! {
        result = &mut candidate => panic!("connected before clear: {result:?}"),
        permit = store.started.acquire() => permit.unwrap().forget(),
        () = tokio::time::sleep(LIMIT) => panic!("clear did not start"),
    }
    drop(candidate);
    assert!(eventloop.diagnostics().session.session_store_clear_pending);
    assert!(eventloop.diagnostics().session.connack.is_none());
    assert!(!eventloop.diagnostics().connected);
    assert!(store.checkpoint.lock().unwrap().is_some());
    store.block_clear.store(false, Ordering::SeqCst);
    let event = poll(&mut eventloop).await.unwrap();
    assert_accepted(&eventloop, event);
    assert_eq!(
        *store.calls.lock().unwrap(),
        ["connect", "clear", "clear", "connect", "clear"]
    );
    assert!(store.checkpoint.lock().unwrap().is_none());
    drop(eventloop);
    tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn persistent_then_invalid_clean_then_persistent_connections_do_not_resurrect_old_work() {
    let store = ControlledStore::seeded();
    let (mut options, rx) = options(Some(store.clone()));
    options.set_clean_session(false);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    let task = tokio::spawn(async move {
        for generation in 0..3 {
            let mut peer = rx.recv_async().await.unwrap();
            assert!(
                matches!(read_packet(&mut peer).await, Packet::Connect(c) if c.clean_session == (generation == 1))
            );
            peer.write_all(&[0x20, 2, u8::from(generation != 2), 0])
                .await
                .unwrap();
            let Packet::Publish(publish) = read_packet(&mut peer).await else {
                panic!("expected publish")
            };
            if generation == 0 {
                assert_eq!(publish.topic.as_ref(), b"old");
                assert_eq!(publish.pkid, 7);
                assert!(publish.dup);
            } else {
                assert_eq!(publish.topic.as_ref(), b"fresh");
                assert_eq!(publish.pkid, 1);
                assert!(!publish.dup);
                peer.write_all(&[0x40, 2, 0, 1]).await.unwrap();
            }
            if generation == 1 {
                let Packet::Subscribe(subscribe) = read_packet(&mut peer).await else {
                    panic!("expected resubscribe")
                };
                peer.write_all(&[
                    0x90,
                    3,
                    (subscribe.pkid >> 8) as u8,
                    subscribe.pkid as u8,
                    0,
                ])
                .await
                .unwrap();
            }
            peer.read_to_end(&mut Vec::new()).await.unwrap();
        }
    });
    for generation in 0..3 {
        let event = poll(&mut eventloop).await.unwrap();
        if generation == 1 {
            assert_accepted(&eventloop, event);
            assert!(store.checkpoint.lock().unwrap().is_none());
            assert!(eventloop.pending_is_empty());
        } else {
            assert!(matches!(event, Event::Incoming(Packet::ConnAck(_))));
        }
        if generation != 0 {
            client
                .try_publish("fresh", "new", PublishOptions::new(QoS::AtLeastOnce))
                .unwrap();
        }
        loop {
            let event = poll(&mut eventloop).await.unwrap();
            if generation == 0 && matches!(event, Event::Outgoing(Outgoing::Publish(7))) {
                break;
            }
            if generation != 0 && matches!(event, Event::Incoming(Packet::PubAck(_))) {
                break;
            }
        }
        if generation == 1 {
            let fresh = !eventloop
                .diagnostics()
                .session
                .connack
                .unwrap()
                .session_resumed;
            assert!(fresh);
            client.try_subscribe("desired", QoS::AtMostOnce).unwrap();
            while !matches!(
                poll(&mut eventloop).await.unwrap(),
                Event::Incoming(Packet::SubAck(_))
            ) {}
        }
        eventloop.clean();
        assert!(eventloop.diagnostics().session.connack.is_none());
        eventloop.mqtt_options.set_clean_session(generation == 0);
    }
    drop(eventloop);
    tokio::time::timeout(LIMIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn compatibility_does_not_relax_refusal_or_malformed_connack_validation() {
    for packet in [[0x20, 2, 3, 0], [0x20, 2, 1, 5], [0x20, 2, 0, 5]] {
        let (options, rx) = options(None);
        let broker = tokio::spawn(async move {
            let mut peer = rx.recv_async().await.unwrap();
            read_packet(&mut peer).await;
            peer.write_all(&packet).await.unwrap();
            peer.read_to_end(&mut Vec::new()).await.unwrap();
        });
        let mut eventloop = EventLoop::new(options, 10);
        let error = poll(&mut eventloop).await.unwrap_err();
        if packet[2] == 0 {
            assert!(matches!(
                error,
                ConnectionError::ConnectionRefused(ConnectReturnCode::NotAuthorized)
            ));
        } else {
            assert!(matches!(error, ConnectionError::MqttState(_)));
        }
        assert!(eventloop.diagnostics().session.connack.is_none());
        assert!(!eventloop.diagnostics().connected);
        tokio::time::timeout(LIMIT, broker).await.unwrap().unwrap();
    }
}
