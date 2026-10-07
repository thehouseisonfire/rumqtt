use bytes::Bytes;
use rumqttc_wrapper_core::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const DEADLINE: Duration = Duration::from_secs(3);

struct MemoryIo {
    read: Arc<tokio::sync::Mutex<tokio::io::ReadHalf<DuplexStream>>>,
    write: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<DuplexStream>>>,
}
impl TransportIo for MemoryIo {
    fn read(&self, max: usize) -> TransportIoFuture<Bytes> {
        let read = self.read.clone();
        Box::pin(async move {
            let mut data = vec![0; max];
            let count = read.lock().await.read(&mut data).await?;
            data.truncate(count);
            Ok(data.into())
        })
    }
    fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.write(&bytes).await })
    }
    fn flush(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.flush().await })
    }
    fn shutdown(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.shutdown().await })
    }
}
struct Connector(tokio::sync::mpsc::UnboundedSender<DuplexStream>);
impl TransportConnector for Connector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let (client, server) = tokio::io::duplex(16384);
        self.0.send(server).unwrap();
        let (read, write) = tokio::io::split(client);
        Box::pin(async move {
            Ok(TransportConnection {
                io: Arc::new(MemoryIo {
                    read: Arc::new(tokio::sync::Mutex::new(read)),
                    write: Arc::new(tokio::sync::Mutex::new(write)),
                }),
                mode: TransportMode::Base,
                network_handling: NetworkHandling::NotApplicable,
            })
        })
    }
}
fn configuration(
    policy: PublishAdmissionPolicy,
    count: usize,
    bytes: usize,
) -> (
    ClientConfig,
    tokio::sync::mpsc::UnboundedReceiver<DuplexStream>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut config = ClientConfig::v5("admission", "memory", 1883);
    config.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(Connector(tx)),
        mode: TransportMode::Base,
    });
    config.common.request_channel_capacity = 8;
    if let ProtocolConfig::V5(v5) = &mut config.protocol {
        v5.publish_admission_policy = policy;
        v5.publish_budget = PublishBudgetLimits {
            max_outstanding: count,
            max_bytes: bytes,
        };
        v5.clean_start = false;
        v5.connect_properties.session_expiry_interval = Some(60);
    }
    (config, rx)
}
fn start(
    policy: PublishAdmissionPolicy,
    count: usize,
    bytes: usize,
) -> (
    NativeClient,
    tokio::sync::mpsc::UnboundedReceiver<DuplexStream>,
) {
    let (config, rx) = configuration(policy, count, bytes);
    (NativeClient::start(config).unwrap(), rx)
}
fn publish(qos: QoS, retain: bool) -> Command {
    Command::Publish(PublishCommand {
        topic: "a".into(),
        payload: Bytes::from_static(b"data"),
        qos,
        retain,
        protocol: PublishProtocolOptions::VersionNeutral,
    })
}
async fn frame(stream: &mut DuplexStream) -> Vec<u8> {
    tokio::time::timeout(DEADLINE, async {
        let mut data = vec![stream.read_u8().await.unwrap()];
        let mut length = 0;
        for shift in [0, 7, 14, 21] {
            let byte = stream.read_u8().await.unwrap();
            data.push(byte);
            length |= usize::from(byte & 127) << shift;
            if byte < 128 {
                let start = data.len();
                data.resize(start + length, 0);
                stream.read_exact(&mut data[start..]).await.unwrap();
                return data;
            }
        }
        panic!("invalid frame")
    })
    .await
    .unwrap()
}
async fn server(rx: &mut tokio::sync::mpsc::UnboundedReceiver<DuplexStream>) -> DuplexStream {
    let mut stream = tokio::time::timeout(DEADLINE, rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame(&mut stream).await[0], 0x10);
    stream
}
async fn connected(events: &mut EventConsumer) {
    tokio::time::timeout(DEADLINE, async {
        while !matches!(
            events.recv_async().await.unwrap().unwrap(),
            WrapperEvent::Connected { .. }
        ) {}
    })
    .await
    .unwrap();
}
async fn result(admission: &Admission) -> Result<Completion> {
    tokio::time::timeout(DEADLINE, admission.completion.wait_async())
        .await
        .unwrap()
}
fn finish(client: NativeClient) {
    client.closer().close_now(DEADLINE).unwrap();
    client.join(DEADLINE).unwrap();
    drop(client);
}

#[tokio::test]
async fn strict_waits_for_connack_and_known_rejections_are_not_admitted() {
    let (mut client, mut rx) = start(
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
        4,
        128,
    );
    let mut events = client.take_events().unwrap();
    let handle = client.handle();
    let mut stream = server(&mut rx).await;
    for (qos, retain) in [
        (QoS::AtLeastOnce, false),
        (QoS::ExactlyOnce, false),
        (QoS::AtMostOnce, true),
    ] {
        let error = handle.try_admit(publish(qos, retain)).unwrap_err();
        assert_eq!(
            error.publish_failure(),
            Some(PublishFailure::CapabilitiesPending)
        );
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
        assert!(error.retryable());
    }
    let ordinary = handle.try_admit(publish(QoS::AtMostOnce, false)).unwrap();
    let waiting = handle.admit_async(publish(QoS::AtLeastOnce, false));
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    stream
        .write_all(&[0x20, 7, 0, 0, 4, 0x24, 0, 0x25, 0])
        .await
        .unwrap();
    connected(&mut events).await;
    let error = waiting.await.unwrap_err();
    assert_eq!(error.publish_failure(), Some(PublishFailure::MaximumQos));
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    assert!(!error.retryable());
    assert_eq!(frame(&mut stream).await[0] >> 4, 3);
    assert!(result(&ordinary).await.is_ok());
    finish(client);
}

#[tokio::test]
async fn deferred_local_rejections_complete_without_broker_reasons_and_release_budget() {
    let (mut client, mut rx) = start(PublishAdmissionPolicy::EventLoopValidated, 4, 128);
    let mut events = client.take_events().unwrap();
    let handle = client.handle();
    let mut stream = server(&mut rx).await;
    let qos = handle.try_admit(publish(QoS::ExactlyOnce, false)).unwrap();
    let retained = handle.try_admit(publish(QoS::AtMostOnce, true)).unwrap();
    stream
        .write_all(&[0x20, 7, 0, 0, 4, 0x24, 0, 0x25, 0])
        .await
        .unwrap();
    connected(&mut events).await;
    for (admission, failure) in [
        (&qos, PublishFailure::MaximumQos),
        (&retained, PublishFailure::RetainUnavailable),
    ] {
        let error = result(admission).await.unwrap_err();
        assert_eq!(error.publish_failure(), Some(failure));
        assert_eq!(error.delivery_status(), DeliveryStatus::Rejected);
        assert_eq!(error.broker_reason(), None);
        assert!(!error.retryable());
    }
    // Deferral also applies while connected; unrelated valid work still flushes.
    let rejected = handle.try_admit(publish(QoS::AtLeastOnce, false)).unwrap();
    assert_eq!(
        result(&rejected).await.unwrap_err().delivery_status(),
        DeliveryStatus::Rejected
    );
    let valid = handle.try_admit(publish(QoS::AtMostOnce, false)).unwrap();
    assert_eq!(frame(&mut stream).await[0] >> 4, 3);
    assert!(result(&valid).await.is_ok());
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    finish(client);
}

#[tokio::test]
async fn replay_keeps_capacity_and_previously_sent_rejection_is_ambiguous() {
    let (mut client, mut rx) = start(PublishAdmissionPolicy::EventLoopValidated, 1, 5);
    let mut events = client.take_events().unwrap();
    let handle = client.handle();
    let mut stream = server(&mut rx).await;
    stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    connected(&mut events).await;
    let original = handle.try_admit(publish(QoS::AtLeastOnce, false)).unwrap();
    assert_eq!(frame(&mut stream).await[0], 0x32);
    for _ in 0..3 {
        drop(stream);
        stream = server(&mut rx).await;
        assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 1);
        let error = handle
            .try_admit(publish(QoS::AtLeastOnce, false))
            .unwrap_err();
        assert_eq!(
            error.publish_failure(),
            Some(PublishFailure::CountExhausted)
        );
        stream.write_all(&[0x20, 3, 1, 0, 0]).await.unwrap();
        connected(&mut events).await;
        assert_eq!(frame(&mut stream).await[0], 0x3a);
    }
    drop(stream);
    stream = server(&mut rx).await;
    stream
        .write_all(&[0x20, 5, 1, 0, 2, 0x24, 0])
        .await
        .unwrap();
    connected(&mut events).await;
    let error = result(&original).await.unwrap_err();
    assert_eq!(error.publish_failure(), Some(PublishFailure::MaximumQos));
    assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
    assert_eq!(error.broker_reason(), None);
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    finish(client);
}

struct PublishProducers {
    stop: Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<usize>>,
}

impl PublishProducers {
    fn start(handle: &ClientHandle, command: &Command) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..2)
            .map(|_| {
                let stop = stop.clone();
                let handle = handle.clone();
                let command = command.clone();
                std::thread::spawn(move || {
                    let mut admitted = 0;
                    while !stop.load(Ordering::Acquire) {
                        match handle.try_admit(command.clone()) {
                            Ok(admission) => {
                                drop(admission);
                                admitted += 1;
                            }
                            Err(error) if error.kind() == ErrorKind::Backpressure => {
                                assert!(matches!(
                                    error.publish_failure(),
                                    Some(
                                        PublishFailure::CapabilitiesPending
                                            | PublishFailure::RequestChannelFull
                                            | PublishFailure::CountExhausted
                                            | PublishFailure::BytesExhausted
                                    )
                                ));
                                assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
                            }
                            Err(error) => panic!("unexpected producer result: {error}"),
                        }
                        let usage = handle.publish_budget_snapshot().unwrap();
                        assert!(usage.outstanding <= usage.limits.max_outstanding);
                        assert!(usage.retained_bytes <= usage.limits.max_bytes);
                        std::thread::yield_now();
                    }
                    admitted
                })
            })
            .collect();
        Self { stop, threads }
    }

    fn finish(mut self) -> usize {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.threads
            .drain(..)
            .map(|thread| thread.join().unwrap())
            .sum()
    }
}

impl Drop for PublishProducers {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep producer, reconnect and wakeup assertions together"
)]
async fn active_producers_and_failed_connections_preserve_large_publish_budgets() {
    fn identifier(frame: &[u8]) -> [u8; 2] {
        let mut packet = bytes::BytesMut::from(frame);
        let rumqttc_v5::Packet::Publish(publish) =
            rumqttc_v5::Packet::read(&mut packet, None).unwrap()
        else {
            unreachable!()
        };
        publish.pkid.to_be_bytes()
    }
    let properties = V5OutgoingPublishProperties {
        response_topic: Some("response".into()),
        correlation_data: Some(Bytes::from(vec![0; 64])),
        content_type: Some("c".repeat(8192)),
        user_properties: vec![
            ("key".into(), "v".repeat(8192)),
            (String::new(), String::new()),
        ],
        ..Default::default()
    };
    let charged_bytes =
        1 + 8192 + 8 + 64 + 8192 + 3 + 8192 + 2 * std::mem::size_of::<(String, String)>();
    let command = Command::Publish(PublishCommand {
        topic: "a".into(),
        payload: Bytes::from(vec![0; 8192]),
        qos: QoS::AtLeastOnce,
        retain: false,
        protocol: PublishProtocolOptions::V5(properties),
    });
    for policy in [
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
        PublishAdmissionPolicy::EventLoopValidated,
    ] {
        for count_limit in [3, 4] {
            // Three messages fit. The fourth is rejected by either the count or
            // byte limit; the one-slot native channel is smaller than both limits.
            let (mut config, mut rx) = configuration(policy, count_limit, charged_bytes * 3);
            config.common.request_channel_capacity = 1;
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let handle = client.handle();
            let mut stream = server(&mut rx).await;
            stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
            connected(&mut events).await;
            let producers = PublishProducers::start(&handle, &command);
            let mut originals = Vec::new();
            for _ in 0..3 {
                let packet = frame(&mut stream).await;
                assert_eq!(packet[0], 0x32);
                originals.push(packet);
            }
            let assert_usage = || {
                let usage = handle.publish_budget_snapshot().unwrap();
                assert_eq!(
                    (usage.outstanding, usage.retained_bytes),
                    (3, charged_bytes * 3)
                );
            };
            assert_usage();
            // Cancelling a blocked admission leaves every existing reservation intact.
            {
                let wait = handle.admit_async(command.clone());
                tokio::pin!(wait);
                assert!(
                    tokio::time::timeout(Duration::from_millis(10), &mut wait)
                        .await
                        .is_err()
                );
            }
            assert_usage();
            for generation in 0..3 {
                drop(stream); // No ACKs: all previously transmitted work must survive.
                stream = server(&mut rx).await;
                assert_usage();
                // Also fail establishment while producers remain active and the
                // native channel/replay queues keep transferring retained work.
                if generation != 1 {
                    drop(stream);
                    stream = server(&mut rx).await;
                    assert_usage();
                }
                let error = handle.try_admit(command.clone()).unwrap_err();
                assert_eq!(
                    error.publish_failure(),
                    Some(match policy {
                        PublishAdmissionPolicy::RequireNegotiatedCapabilities =>
                            PublishFailure::CapabilitiesPending,
                        PublishAdmissionPolicy::EventLoopValidated if count_limit == 3 =>
                            PublishFailure::CountExhausted,
                        PublishAdmissionPolicy::EventLoopValidated =>
                            PublishFailure::BytesExhausted,
                    })
                );
                stream.write_all(&[0x20, 3, 1, 0, 0]).await.unwrap();
                connected(&mut events).await;
                let mut replayed = std::collections::BTreeSet::new();
                for _ in 0..3 {
                    let mut packet = frame(&mut stream).await;
                    assert_eq!(packet[0], 0x3a);
                    packet[0] &= !8;
                    let index = originals
                        .iter()
                        .position(|original| original == &packet)
                        .unwrap();
                    assert!(replayed.insert(index));
                }
                assert_eq!(replayed.len(), 3);
                assert_usage();
            }
            assert_eq!(producers.finish(), 3);
            let wait = handle.admit_async(command.clone());
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut wait)
                    .await
                    .is_err()
            );
            // One ACK must wake admission without exceeding either bound.
            let first = &originals[0];
            let id = identifier(first);
            stream.write_all(&[0x40, 2, id[0], id[1]]).await.unwrap();
            let admitted = tokio::time::timeout(DEADLINE, wait).await.unwrap().unwrap();
            assert_usage();
            let mut packet = bytes::BytesMut::from(frame(&mut stream).await.as_slice());
            let rumqttc_v5::Packet::Publish(publish) =
                rumqttc_v5::Packet::read(&mut packet, None).unwrap()
            else {
                unreachable!()
            };
            for original in &originals[1..] {
                let id = identifier(original);
                stream.write_all(&[0x40, 2, id[0], id[1]]).await.unwrap();
            }
            let id = publish.pkid.to_be_bytes();
            stream.write_all(&[0x40, 2, id[0], id[1]]).await.unwrap();
            assert!(result(&admitted).await.is_ok());
            assert_eq!(handle.publish_budget_snapshot().unwrap().retained_bytes, 0);
            assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
            finish(client);
        }
    }
}

#[tokio::test]
async fn cancelled_wait_and_dropped_observer_do_not_change_admitted_work() {
    let (mut client, mut rx) = start(PublishAdmissionPolicy::EventLoopValidated, 1, 5);
    let mut events = client.take_events().unwrap();
    let handle = client.handle();
    let mut stream = server(&mut rx).await;
    let original = handle.try_admit(publish(QoS::AtLeastOnce, false)).unwrap();
    drop(original);
    {
        let waiting = handle.admit_async(publish(QoS::AtLeastOnce, false));
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
    }
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 1);
    stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    connected(&mut events).await;
    let packet = frame(&mut stream).await;
    let waiting = handle.admit_async(publish(QoS::AtLeastOnce, false));
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    stream
        .write_all(&[0x40, 2, packet[5], packet[6]])
        .await
        .unwrap();
    let admitted = tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 1);
    let packet = frame(&mut stream).await;
    stream
        .write_all(&[0x40, 2, packet[5], packet[6]])
        .await
        .unwrap();
    assert!(result(&admitted).await.is_ok());
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    finish(client);
}

#[tokio::test]
async fn shutdown_wakes_blocked_publish_admission() {
    let (client, mut rx) = start(PublishAdmissionPolicy::EventLoopValidated, 1, 5);
    let handle = client.handle();
    let _stream = server(&mut rx).await;
    handle.try_admit(publish(QoS::AtLeastOnce, false)).unwrap();
    let waiting = handle.admit_async(publish(QoS::AtLeastOnce, false));
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    let error = tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Shutdown);
    client.join(DEADLINE).unwrap();
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
}

#[tokio::test]
async fn qos2_keeps_its_reservation_until_pubcomp() {
    let (mut client, mut rx) = start(PublishAdmissionPolicy::EventLoopValidated, 1, 5);
    let mut events = client.take_events().unwrap();
    let handle = client.handle();
    let mut stream = server(&mut rx).await;
    stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    connected(&mut events).await;
    let original = handle.try_admit(publish(QoS::ExactlyOnce, false)).unwrap();
    let packet = frame(&mut stream).await;
    stream
        .write_all(&[0x50, 2, packet[5], packet[6]])
        .await
        .unwrap();
    assert_eq!(frame(&mut stream).await[0], 0x62);
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 1);
    assert_eq!(
        handle
            .try_admit(publish(QoS::AtMostOnce, false))
            .unwrap_err()
            .publish_failure(),
        Some(PublishFailure::CountExhausted)
    );
    stream
        .write_all(&[0x70, 2, packet[5], packet[6]])
        .await
        .unwrap();
    assert!(result(&original).await.is_ok());
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    finish(client);
}

struct MemoryStore {
    checkpoint: std::sync::Mutex<Option<SessionCheckpoint>>,
    gate: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
    entered: tokio::sync::Notify,
    saved: tokio::sync::mpsc::UnboundedSender<SessionCheckpoint>,
}
impl SessionStore for MemoryStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        let checkpoint = self.checkpoint.lock().unwrap().clone();
        let gate = self.gate.lock().unwrap().take();
        self.entered.notify_one();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            Ok(checkpoint)
        })
    }
    fn save(&self, _: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
        *self.checkpoint.lock().unwrap() = Some(checkpoint.clone());
        self.saved.send(checkpoint).unwrap();
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        *self.checkpoint.lock().unwrap() = None;
        Box::pin(async { Ok(()) })
    }
}
fn with_store(
    bytes: usize,
    store: Arc<MemoryStore>,
) -> (
    ClientConfig,
    tokio::sync::mpsc::UnboundedReceiver<DuplexStream>,
) {
    let (mut config, rx) = configuration(PublishAdmissionPolicy::EventLoopValidated, 1, bytes);
    if let ProtocolConfig::V5(v5) = &mut config.protocol {
        v5.session_store = Some(SessionStoreConfig::new(store, "admission-memory"));
    }
    (config, rx)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep checkpoint creation and recovery assertions together"
)]
async fn recovery_gates_admission_and_over_budget_restore_preserves_checkpoint_for_retry() {
    let (saved_tx, mut saved_rx) = tokio::sync::mpsc::unbounded_channel();
    let store = Arc::new(MemoryStore {
        checkpoint: std::sync::Mutex::new(None),
        gate: std::sync::Mutex::new(None),
        entered: tokio::sync::Notify::new(),
        saved: saved_tx,
    });
    let (config, mut rx) = with_store(5, store.clone());
    let mut first = NativeClient::start(config).unwrap();
    let mut events = first.take_events().unwrap();
    let mut stream = server(&mut rx).await;
    stream.write_all(&[0x20, 3, 0, 0, 0]).await.unwrap();
    connected(&mut events).await;
    let observer = first
        .handle()
        .try_admit(publish(QoS::AtLeastOnce, false))
        .unwrap();
    assert_eq!(frame(&mut stream).await[0], 0x32);
    tokio::time::timeout(DEADLINE, async {
        loop {
            let saved = saved_rx.recv().await.unwrap();
            if saved.0.windows(4).any(|bytes| bytes == b"data") {
                break;
            }
        }
    })
    .await
    .unwrap();
    finish(first);
    drop(observer);
    let checkpoint = store.checkpoint.lock().unwrap().clone().unwrap();
    // Consume the first load notification before installing the second barrier.
    store.entered.notified().await;
    let release = Arc::new(tokio::sync::Notify::new());
    *store.gate.lock().unwrap() = Some(release.clone());
    let (config, rx) = with_store(4, store.clone());
    let mut failed = NativeClient::start(config).unwrap();
    let mut events = failed.take_events().unwrap();
    tokio::time::timeout(DEADLINE, store.entered.notified())
        .await
        .unwrap();
    let handle = failed.handle();
    assert!(handle.publish_budget_snapshot().unwrap().recovery_pending);
    let error = handle
        .try_admit(Command::Publish(PublishCommand {
            topic: "a".into(),
            payload: Bytes::new(),
            qos: QoS::AtLeastOnce,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        }))
        .unwrap_err();
    assert_eq!(
        error.publish_failure(),
        Some(PublishFailure::RecoveryPending)
    );
    release.notify_one();
    let terminal = tokio::time::timeout(DEADLINE, async {
        loop {
            if let WrapperEvent::DriverTerminated(error) =
                events.recv_async().await.unwrap().unwrap()
            {
                break error;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        terminal.publish_failure(),
        Some(PublishFailure::RestoreBudgetExceeded)
    );
    assert_eq!(
        terminal.store_failure(),
        Some(StoreFailure::PublishBudgetExceeded)
    );
    assert!(!terminal.retryable());
    failed.join(DEADLINE).unwrap();
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    assert_eq!(*store.checkpoint.lock().unwrap(), Some(checkpoint));
    assert!(rx.is_empty()); // Preflight failed before invoking any connector.
    let (config, mut rx) = with_store(5, store.clone());
    let mut restored = NativeClient::start(config).unwrap();
    let mut events = restored.take_events().unwrap();
    let mut stream = server(&mut rx).await;
    let handle = restored.handle();
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 1);
    stream.write_all(&[0x20, 3, 1, 0, 0]).await.unwrap();
    connected(&mut events).await;
    let replay = frame(&mut stream).await;
    assert_eq!(replay[0], 0x3a);
    let waiting = handle.admit_async(publish(QoS::AtLeastOnce, false));
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    stream
        .write_all(&[0x40, 2, replay[5], replay[6]])
        .await
        .unwrap();
    let admitted = tokio::time::timeout(DEADLINE, waiting)
        .await
        .unwrap()
        .unwrap();
    let packet = frame(&mut stream).await;
    stream
        .write_all(&[0x40, 2, packet[5], packet[6]])
        .await
        .unwrap();
    assert!(result(&admitted).await.is_ok());
    assert_eq!(handle.publish_budget_snapshot().unwrap().outstanding, 0);
    finish(restored);
}
