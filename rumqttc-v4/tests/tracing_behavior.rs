#[path = "support/tracing_capture.rs"]
mod tracing_capture;

use bytes::BytesMut;
use rumqttc::mqttbytes::v4::Packet;
use rumqttc::{
    AckMode, ConnectionError, Event, EventLoop, MqttOptions, PersistedAckMode, PersistedPublish,
    PersistedQoS, PersistedRequest, PersistedSession, ProtocolViolation, SessionStore,
    SessionStoreError, SessionStoreKey, StateError,
};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tracing::instrument::WithSubscriber;
use tracing::{Level, Subscriber};
use tracing_capture::{Capture, Value, assert_no_event, event_position, only_event};
use tracing_subscriber::prelude::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct MemorySessionStore(Arc<Mutex<Option<PersistedSession>>>);

impl MemorySessionStore {
    fn new(session: PersistedSession) -> Self {
        Self(Arc::new(Mutex::new(Some(session))))
    }
}

impl SessionStore for MemorySessionStore {
    fn load<'a>(
        &'a self,
        _key: &'a SessionStoreKey,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PersistedSession>, SessionStoreError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(self.0.lock().unwrap().clone()) })
    }

    fn save<'a>(
        &'a self,
        _key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(session.clone());
            Ok(())
        })
    }

    fn clear<'a>(
        &'a self,
        _key: &'a SessionStoreKey,
    ) -> Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>> {
        Box::pin(async {
            *self.0.lock().unwrap() = None;
            Ok(())
        })
    }
}

fn persisted_qos1_session(client_id: &str) -> PersistedSession {
    PersistedSession {
        format_version: 1,
        client_id: client_id.to_owned(),
        clean_session: false,
        max_inflight: 100,
        ack_mode: PersistedAckMode::Automatic,
        last_pkid: 1,
        last_puback: 0,
        replay: vec![PersistedRequest::Publish(PersistedPublish {
            dup: true,
            qos: PersistedQoS::AtLeastOnce,
            retain: false,
            topic: b"behavior/tracing".to_vec(),
            pkid: 1,
            payload: b"restored".to_vec(),
        })],
        incoming_qos2: Vec::new(),
    }
}

async fn read_packet(peer: &mut DuplexStream) -> Packet {
    let first = peer.read_u8().await.unwrap();
    let mut multiplier = 1usize;
    let mut remaining_len = 0usize;
    let mut encoded_len = Vec::new();
    loop {
        let byte = peer.read_u8().await.unwrap();
        encoded_len.push(byte);
        remaining_len += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining_len];
    peer.read_exact(&mut body).await.unwrap();

    let mut bytes = BytesMut::from(&[first][..]);
    bytes.extend_from_slice(&encoded_len);
    bytes.extend_from_slice(&body);
    Packet::read(&mut bytes, 1024).unwrap()
}

fn subscriber(capture: Capture) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry().with(capture)
}

#[tokio::test]
async fn failed_initial_attempt_emits_attempt_and_failed_only() {
    let capture = Capture::default();
    let mut options = MqttOptions::new("trace-failure-v4", "localhost");
    options.set_socket_connector(|_, _| async {
        Err::<DuplexStream, _>(io::Error::new(io::ErrorKind::ConnectionRefused, "expected"))
    });
    let mut eventloop = EventLoop::new(options, 1);

    tokio::time::timeout(
        TEST_TIMEOUT,
        async move {
            assert!(matches!(
                eventloop.poll().await,
                Err(ConnectionError::Io(error)) if error.kind() == io::ErrorKind::ConnectionRefused
            ));
        }
        .with_subscriber(subscriber(capture.clone())),
    )
    .await
    .expect("connection failure path deadlocked");

    let events = capture.events();
    let attempt = only_event(&events, "mqtt.connection_attempt");
    let failed = only_event(&events, "mqtt.connection_attempt_failed");
    attempt.assert_metadata("mqtt.connection_attempt", Level::DEBUG);
    failed.assert_metadata("mqtt.connection_attempt_failed", Level::WARN);
    assert_eq!(attempt.field("protocol"), &Value::Str("v4".to_owned()));
    assert_eq!(failed.field("protocol"), &Value::Str("v4".to_owned()));
    assert_eq!(failed.field("phase"), &Value::Str("transport".to_owned()));
    assert_eq!(failed.field("error_kind"), &Value::Str("io".to_owned()));
    assert_eq!(attempt.field("reconnect"), &Value::Bool(false));
    for field in [
        "attempt_id",
        "connection_generation",
        "attempt_in_generation",
    ] {
        assert!(matches!(attempt.field(field), Value::U64(_)));
        assert_eq!(attempt.field(field), failed.field(field));
    }
    assert_no_event(&events, "mqtt.connection_established");
    assert_no_event(&events, "mqtt.connection_lost");
    assert_no_event(&events, "mqtt.protocol_violation");
}

#[tokio::test]
async fn restored_connection_duplicate_connack_emits_wired_lifecycle_in_order() {
    let capture = Capture::default();
    let (peer_tx, peer_rx) = flume::bounded(1);
    let mut options = MqttOptions::new("trace-lifecycle-v4", "localhost");
    options.set_clean_session(false);
    options.set_ack_mode(AckMode::Automatic);
    options.set_session_store(MemorySessionStore::new(persisted_qos1_session(
        "trace-lifecycle-v4",
    )));
    options.set_socket_connector(move |_, _| {
        let peer_tx = peer_tx.clone();
        async move {
            let (client, peer) = tokio::io::duplex(1024);
            peer_tx.send(peer).unwrap();
            Ok(client)
        }
    });
    let mut eventloop = EventLoop::new(options, 1);

    tokio::time::timeout(
        TEST_TIMEOUT,
        async move {
            let broker = tokio::spawn(async move {
                let mut peer = peer_rx.recv_async().await.unwrap();
                assert!(matches!(read_packet(&mut peer).await, Packet::Connect(_)));
                let connack = [0x20, 0x02, 0x01, 0x00];
                peer.write_all(&connack).await.unwrap();
                assert!(matches!(
                    read_packet(&mut peer).await,
                    Packet::Publish(publish) if publish.pkid == 1 && publish.dup
                ));
                peer.write_all(&connack).await.unwrap();
                std::future::pending::<()>().await;
            });

            assert!(matches!(
                eventloop.poll().await.unwrap(),
                Event::Incoming(Packet::ConnAck(connack)) if connack.session_present
            ));

            let mut replay_was_sent = false;
            loop {
                match eventloop.poll().await {
                    Ok(Event::Outgoing(rumqttc::Outgoing::Publish(1))) => {
                        replay_was_sent = true;
                    }
                    Ok(_) => continue,
                    Err(ConnectionError::MqttState(StateError::ProtocolViolation(
                        ProtocolViolation::DuplicateConnAck,
                    ))) => {
                        assert!(replay_was_sent, "restored publish was not replayed");
                        break;
                    }
                    Err(error) => panic!("unexpected event-loop error: {error:?}"),
                }
            }
            broker.abort();
        }
        .with_subscriber(subscriber(capture.clone())),
    )
    .await
    .expect("restored protocol-violation path deadlocked");

    assert_lifecycle_events(&capture.events(), "v4");
}

fn assert_lifecycle_events(events: &[tracing_capture::CapturedEvent], protocol: &str) {
    let restored = only_event(events, "mqtt.session_restored");
    let attempt = only_event(events, "mqtt.connection_attempt");
    let established = only_event(events, "mqtt.connection_established");
    let violation = only_event(events, "mqtt.protocol_violation");
    let lost = only_event(events, "mqtt.connection_lost");
    let replay = only_event(events, "mqtt.replay_prepared");

    restored.assert_metadata("mqtt.session_restored", Level::INFO);
    attempt.assert_metadata("mqtt.connection_attempt", Level::DEBUG);
    established.assert_metadata("mqtt.connection_established", Level::INFO);
    violation.assert_metadata("mqtt.protocol_violation", Level::ERROR);
    lost.assert_metadata("mqtt.connection_lost", Level::WARN);
    replay.assert_metadata("mqtt.replay_prepared", Level::INFO);

    for event in [restored, attempt, established, violation, lost, replay] {
        assert_eq!(event.field("protocol"), &Value::Str(protocol.to_owned()));
    }
    for field in [
        "attempt_id",
        "connection_generation",
        "attempt_in_generation",
    ] {
        assert!(matches!(attempt.field(field), Value::U64(_)));
        assert_eq!(attempt.field(field), established.field(field));
        assert_eq!(attempt.field(field), lost.field(field));
    }
    assert_eq!(attempt.field("attempt_id"), &Value::U64(1));
    assert_eq!(attempt.field("connection_generation"), &Value::U64(0));
    assert_eq!(attempt.field("attempt_in_generation"), &Value::U64(1));
    assert_eq!(established.field("session_present"), &Value::Bool(true));
    assert_eq!(restored.field("replay_count"), &Value::U64(1));
    assert_eq!(restored.field("connection_generation"), &Value::U64(0));
    assert_eq!(
        violation.field("violation_kind"),
        &Value::Str("duplicate_connack".to_owned())
    );
    assert_eq!(lost.field("error_kind"), &Value::Str("protocol".to_owned()));
    assert_eq!(replay.field("replay_count"), &Value::U64(1));
    assert_eq!(replay.field("connection_generation"), &Value::U64(1));
    assert_eq!(replay.field("reconnect"), &Value::Bool(true));

    let order = [
        "mqtt.session_restored",
        "mqtt.connection_attempt",
        "mqtt.connection_established",
        "mqtt.protocol_violation",
        "mqtt.connection_lost",
        "mqtt.replay_prepared",
    ]
    .map(|name| event_position(events, name));
    assert!(
        order
            .windows(2)
            .all(|positions| positions[0] < positions[1])
    );
}
