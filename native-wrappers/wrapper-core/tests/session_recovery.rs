#[path = "support/custom_transport.rs"]
mod custom_transport;
mod support;
use rumqttc_wrapper_core::*;
use std::io::Write;
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;
use support::{Broker, DEADLINE, accept, frame, start, terminal, until};

fn config(mqtt5: bool, persistent: bool, port: u16) -> ClientConfig {
    let mut config = support::config(mqtt5, port);
    match &mut config.protocol {
        ProtocolConfig::V4(v4) => v4.clean_session = !persistent,
        ProtocolConfig::V5(v5) => {
            v5.clean_start = !persistent;
            v5.connect_properties.session_expiry_interval = Some(if persistent { 60 } else { 0 });
        }
    }
    config
}
fn connack(socket: &mut impl Write, mqtt5: bool, session: bool) {
    socket
        .write_all(if mqtt5 {
            if session {
                &[0x20, 3, 1, 0, 0]
            } else {
                &[0x20, 3, 0, 0, 0]
            }
        } else if session {
            &[0x20, 2, 1, 0]
        } else {
            &[0x20, 2, 0, 0]
        })
        .unwrap();
}
fn subscribe() -> Command {
    Command::Subscribe(SubscribeCommand {
        filters: vec![Subscription {
            filter: "discarded".into(),
            qos: QoS::AtLeastOnce,
            protocol: SubscriptionProtocolOptions::VersionNeutral,
        }],
        protocol: SubscribeProtocolOptions::VersionNeutral,
    })
}

#[test]
fn recovery_hides_candidate_connections_discards_queues_and_preserves_session_policy() {
    for mqtt5 in [false, true] {
        for persistent in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (entered_tx, entered) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let broker = Broker::spawn(move || {
                let mut candidate = accept(&listener);
                assert_eq!(frame(&mut candidate)[0], 0x10);
                entered_tx.send(()).unwrap();
                released.recv_timeout(DEADLINE).unwrap();
                connack(&mut candidate, mqtt5, false);
                // The request barrier prevents every queued application packet from being sent.
                assert_eq!(frame(&mut candidate)[0], 0xe0);
                drop(candidate);
                let mut fresh = accept(&listener);
                let connect = frame(&mut fresh);
                assert_ne!(connect[9] & 2, 0, "recovery must request a fresh session");
                connack(&mut fresh, mqtt5, false);
                if !mqtt5 && persistent {
                    assert_eq!(frame(&mut fresh)[0], 0xe0);
                    drop(fresh);
                    fresh = accept(&listener);
                    let connect = frame(&mut fresh);
                    assert_eq!(connect[9] & 2, 0, "final v4 connection must be persistent");
                    connack(&mut fresh, false, false);
                }
                assert_eq!(frame(&mut fresh)[0], 0xe0);
            });
            let mut client = start(config(mqtt5, persistent, port)).unwrap();
            let mut events = client.take_events().unwrap();
            entered.recv_timeout(DEADLINE).unwrap();
            let discarded = client.handle().try_admit(subscribe()).unwrap();
            let publish = client
                .handle()
                .try_admit(Command::Publish(PublishCommand {
                    topic: "discarded".into(),
                    payload: b"old".as_slice().into(),
                    qos: QoS::AtMostOnce,
                    retain: false,
                    protocol: PublishProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
            assert!(client.connection().try_wait().is_none());
            assert_eq!(
                recovery.completion.recovery_snapshot().unwrap().phase,
                RecoveryPhase::Quiescing
            );
            assert_eq!(
                client
                    .handle()
                    .try_admit(Command::RecoverSession)
                    .unwrap_err()
                    .recovery_failure(),
                Some(RecoveryFailure::InProgress)
            );
            let blocked = client.handle().try_admit(subscribe()).unwrap_err();
            assert_eq!(blocked.delivery_status(), DeliveryStatus::NotAdmitted);
            assert_eq!(blocked.kind(), ErrorKind::Backpressure);
            assert_eq!(
                recovery
                    .completion
                    .wait_timeout(Duration::from_millis(1))
                    .unwrap_err()
                    .kind(),
                ErrorKind::Timeout
            );
            release.send(()).unwrap();
            assert_eq!(terminal(&recovery).unwrap(), Completion::SessionRecovered);
            assert!(
                terminal(&discarded)
                    .unwrap_err()
                    .message()
                    .contains("session reset")
            );
            let failed = terminal(&publish).unwrap_err();
            assert_eq!(failed.publish_failure(), Some(PublishFailure::SessionReset));
            assert_eq!(
                failed.delivery_status(),
                if mqtt5 {
                    DeliveryStatus::Rejected
                } else {
                    DeliveryStatus::Ambiguous
                }
            );
            let snapshot = recovery.completion.recovery_snapshot().unwrap();
            assert_eq!(snapshot.phase, RecoveryPhase::Completed);
            assert!(
                snapshot.abandonment_committed
                    && snapshot.checkpoint_cleared
                    && snapshot.fresh_established
            );
            assert_eq!(snapshot.raw_session_present, Some(false));
            assert!(client.connection().try_wait().unwrap().is_ok());
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            assert_eq!(
                client
                    .handle()
                    .try_admit(Command::RecoverSession)
                    .unwrap_err()
                    .recovery_failure(),
                Some(RecoveryFailure::Unavailable)
            );
            // Keep the broker alive until the client closes, while proving native polling resumed.
            // The configured keepalive is shortened below by this fixture's default.
            client.closer().close(DEADLINE).unwrap();
            broker.join();
            drop(client);
            assert_eq!(recovery.completion.recovery_snapshot().unwrap(), snapshot);
        }
    }
}

struct FailingAttempt {
    entered: mpsc::Sender<()>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    calls: Arc<AtomicUsize>,
    failure: TransportFailure,
}
impl TransportConnector for FailingAttempt {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let release = self.release.lock().unwrap().take();
        let failure = self.failure;
        self.entered.send(()).unwrap();
        Box::pin(async move {
            if let Some(release) = release {
                let _ = release.await;
            }
            Err(failure)
        })
    }
}

#[test]
fn recovery_overtaking_a_terminal_transport_failure_does_not_abandon_or_retry() {
    for mqtt5 in [false, true] {
        for classified in [false, true] {
            for failure in [TransportFailure::Composition, TransportFailure::Panic] {
                let (entered_tx, entered) = mpsc::channel();
                let (release, released) = tokio::sync::oneshot::channel();
                let calls = Arc::new(AtomicUsize::new(0));
                let clears = Arc::new(AtomicUsize::new(0));
                let mut settings = config(mqtt5, true, 1883);
                settings.common.connector = Some(TransportConnectorConfig {
                    connector: Arc::new(FailingAttempt {
                        entered: entered_tx,
                        release: Mutex::new(Some(released)),
                        calls: calls.clone(),
                        failure,
                    }),
                    mode: TransportMode::Base,
                });
                if classified {
                    settings.common.reconnect =
                        ReconnectPolicy::Classified(ReconnectConfig::default());
                }
                let store = Some(SessionStoreConfig::new(
                    Arc::new(ClearStore {
                        fail: false,
                        clears: clears.clone(),
                        release: Mutex::new(None),
                        entered: None,
                    }),
                    "terminal-attempt",
                ));
                match &mut settings.protocol {
                    ProtocolConfig::V4(v4) => v4.session_store = store,
                    ProtocolConfig::V5(v5) => v5.session_store = store,
                }
                let client = start(settings).unwrap();
                entered.recv_timeout(DEADLINE).unwrap();
                let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
                release.send(()).unwrap();

                let error = terminal(&recovery).unwrap_err();
                assert_eq!(error.transport_failure(), Some(failure));
                assert!(!error.retryable());
                assert_eq!(error.recovery_failure(), Some(RecoveryFailure::Transition));
                client.join(DEADLINE).unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(clears.load(Ordering::SeqCst), 0);
                let progress = recovery.completion.recovery_snapshot().unwrap();
                assert_eq!(progress.failure_phase, Some(RecoveryPhase::Quiescing));
                assert!(!progress.abandonment_committed && !progress.checkpoint_cleared);
            }
        }
    }
}

#[test]
fn recovery_overtaking_a_classified_terminal_connack_does_not_abandon_or_retry() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            frame(&mut socket);
            entered_tx.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            socket
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0x87, 0]
                } else {
                    &[0x20, 2, 0, 5]
                })
                .unwrap();
            finished.recv_timeout(DEADLINE).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        });
        let clears = Arc::new(AtomicUsize::new(0));
        let mut settings = config(mqtt5, true, port);
        settings.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig::default());
        let store = Some(SessionStoreConfig::new(
            Arc::new(ClearStore {
                fail: false,
                clears: clears.clone(),
                release: Mutex::new(None),
                entered: None,
            }),
            "terminal-connack",
        ));
        match &mut settings.protocol {
            ProtocolConfig::V4(v4) => v4.session_store = store,
            ProtocolConfig::V5(v5) => v5.session_store = store,
        }
        let client = start(settings).unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        release.send(()).unwrap();
        let error = terminal(&recovery).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Authentication);
        assert!(!error.retryable());
        assert_eq!(error.broker_reason(), Some(if mqtt5 { 0x87 } else { 5 }));
        assert_eq!(error.recovery_failure(), Some(RecoveryFailure::Transition));
        client.join(DEADLINE).unwrap();
        assert_eq!(clears.load(Ordering::SeqCst), 0);
        let progress = recovery.completion.recovery_snapshot().unwrap();
        assert!(!progress.abandonment_committed && !progress.checkpoint_cleared);
        finished_tx.send(()).unwrap();
        broker.join();
    }
}

struct ClearStore {
    fail: bool,
    clears: Arc<AtomicUsize>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    entered: Option<mpsc::Sender<()>>,
}

struct LossCheckpointStore {
    armed: AtomicBool,
    entered: mpsc::Sender<()>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl SessionStore for LossCheckpointStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        Box::pin(async { Ok(None) })
    }
    fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
        let release = if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(release) = release {
                release.await.unwrap();
            }
            Ok(())
        })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

struct AttemptCancellation(mpsc::Sender<()>);
impl Drop for AttemptCancellation {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
struct RecoveryConnector {
    initial: Arc<dyn TransportConnector>,
    calls: Arc<AtomicUsize>,
    entered: mpsc::Sender<()>,
    cancelled: mpsc::Sender<()>,
}
impl TransportConnector for RecoveryConnector {
    fn connect(&self, request: TransportRequest) -> TransportFuture {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return self.initial.connect(request);
        }
        self.entered.send(()).unwrap();
        let cancellation = AttemptCancellation(self.cancelled.clone());
        Box::pin(async move {
            let _cancellation = cancellation;
            std::future::pending().await
        })
    }
}
struct LossRecovery {
    client: NativeClient,
    recovery: Admission,
    calls: Arc<AtomicUsize>,
    fresh: mpsc::Receiver<()>,
    cancelled: mpsc::Receiver<()>,
    _events: EventConsumer,
    finished: mpsc::Sender<()>,
    broker: Broker,
}
impl LossRecovery {
    fn finish(self) {
        self.client.join(DEADLINE).unwrap();
        self.finished.send(()).unwrap();
        self.broker.join();
    }
}
fn recover_during_loss(mqtt5: bool, policy: ReconnectConfig) -> LossRecovery {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (lose_tx, lose) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut socket = accept(&listener);
        frame(&mut socket);
        connack(&mut socket, mqtt5, false);
        lose.recv_timeout(DEADLINE).unwrap();
        drop(socket);
        finished.recv_timeout(DEADLINE).unwrap();
    });
    let (checkpoint_tx, checkpoint) = mpsc::channel();
    let (release_checkpoint, released) = tokio::sync::oneshot::channel();
    let store = Arc::new(LossCheckpointStore {
        armed: AtomicBool::new(false),
        entered: checkpoint_tx,
        release: Mutex::new(Some(released)),
    });
    let mut settings = config(mqtt5, true, port);
    settings.common.connection_timeout = Duration::from_secs(60);
    let (initial, _) = custom_transport::configured();
    let calls = Arc::new(AtomicUsize::new(0));
    let (fresh_tx, fresh) = mpsc::channel();
    let (cancelled_tx, cancelled) = mpsc::channel();
    settings.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(RecoveryConnector {
            initial: initial.connector,
            calls: calls.clone(),
            entered: fresh_tx,
            cancelled: cancelled_tx,
        }),
        mode: TransportMode::Base,
    });
    settings.common.reconnect = ReconnectPolicy::Classified(policy);
    let persistence = Some(SessionStoreConfig::new(store.clone(), "loss-recovery"));
    match &mut settings.protocol {
        ProtocolConfig::V4(v4) => v4.session_store = persistence,
        ProtocolConfig::V5(v5) => v5.session_store = persistence,
    }
    let mut client = start(settings).unwrap();
    let mut events = client.take_events().unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    store.armed.store(true, Ordering::SeqCst);
    lose_tx.send(()).unwrap();
    checkpoint.recv_timeout(DEADLINE).unwrap();
    let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
    release_checkpoint.send(()).unwrap();
    LossRecovery {
        client,
        recovery,
        calls,
        fresh,
        cancelled,
        _events: events,
        finished: finished_tx,
        broker,
    }
}

#[test]
fn recovery_admitted_during_connection_loss_preserves_the_retry_budget() {
    for mqtt5 in [false, true] {
        let fixture = recover_during_loss(
            mqtt5,
            ReconnectConfig {
                budget: RetryBudget::Limited(0),
                stability_interval: Duration::from_secs(60),
                ..ReconnectConfig::default()
            },
        );
        assert_eq!(
            terminal(&fixture.recovery).unwrap_err().code(),
            ErrorCode::ReconnectExhausted
        );
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            1,
            "recovery must not grant another attempt"
        );
        assert!(
            fixture
                .recovery
                .completion
                .recovery_snapshot()
                .unwrap()
                .checkpoint_cleared
        );
        assert!(
            fixture
                .client
                .handle()
                .reconnect_diagnostics()
                .last_failure
                .is_some()
        );
        fixture.finish();
    }
}

#[test]
fn recovery_admitted_during_connection_loss_preserves_backoff() {
    for mqtt5 in [false, true] {
        let delay = Duration::from_secs(60);
        let fixture = recover_during_loss(
            mqtt5,
            ReconnectConfig {
                initial_delay: delay,
                maximum_delay: delay,
                jitter: ReconnectJitter::None,
                budget: RetryBudget::Unlimited,
                stability_interval: delay,
                ..ReconnectConfig::default()
            },
        );
        let deadline = std::time::Instant::now() + DEADLINE;
        while !fixture
            .recovery
            .completion
            .recovery_snapshot()
            .unwrap()
            .checkpoint_cleared
        {
            assert!(
                std::time::Instant::now() < deadline,
                "abandonment did not finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let retry = fixture.client.handle().reconnect_diagnostics();
        assert_eq!(retry.phase, ReconnectPhase::Waiting);
        assert!(
            retry
                .remaining_delay
                .is_some_and(|remaining| remaining > Duration::ZERO)
        );
        assert!(retry.last_failure.is_some());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        let shutdown = fixture
            .client
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        terminal(&shutdown).unwrap();
        assert_eq!(
            terminal(&fixture.recovery).unwrap_err().recovery_failure(),
            Some(RecoveryFailure::Interrupted)
        );
        fixture.finish();
    }
}

#[test]
fn immediate_shutdown_cancels_recovery_after_connection_loss() {
    for mqtt5 in [false, true] {
        let fixture = recover_during_loss(
            mqtt5,
            ReconnectConfig {
                initial_delay: Duration::ZERO,
                maximum_delay: Duration::ZERO,
                budget: RetryBudget::Unlimited,
                ..ReconnectConfig::default()
            },
        );
        fixture.fresh.recv_timeout(DEADLINE).unwrap();
        let shutdown = fixture
            .client
            .handle()
            .try_admit(Command::ImmediateDisconnect)
            .unwrap();
        fixture
            .cancelled
            .recv_timeout(DEADLINE)
            .expect("recovery transport was not cancelled");
        terminal(&shutdown).unwrap();
        assert_eq!(
            terminal(&fixture.recovery).unwrap_err().recovery_failure(),
            Some(RecoveryFailure::Interrupted)
        );
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        fixture.finish();
    }
}

impl SessionStore for ClearStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        Box::pin(async { Ok(None) })
    }
    fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = &self.entered {
            entered.send(()).unwrap();
        }
        let fail = self.fail;
        let release = self.release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(release) = release {
                let _ = release.await;
            }
            if fail {
                Err(StoreFailure::Clear)
            } else {
                Ok(())
            }
        })
    }
}

#[test]
fn failed_checkpoint_clear_is_terminal_and_retains_partial_progress() {
    for mqtt5 in [false, true] {
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let clears = Arc::new(AtomicUsize::new(0));
        let mut config = config(mqtt5, true, 1883);
        config.common.request_channel_capacity = 1;
        config.common.connector = Some(TransportConnectorConfig {
            connector: Arc::new(FailingAttempt {
                entered: entered_tx,
                release: Mutex::new(Some(released)),
                calls: calls.clone(),
                failure: TransportFailure::Connect,
            }),
            mode: TransportMode::Base,
        });
        let store = Some(SessionStoreConfig::new(
            Arc::new(ClearStore {
                fail: true,
                clears: clears.clone(),
                release: Mutex::new(None),
                entered: None,
            }),
            "recovery",
        ));
        match &mut config.protocol {
            ProtocolConfig::V4(v4) => v4.session_store = store,
            ProtocolConfig::V5(v5) => v5.session_store = store,
        }
        let mut client = start(config).unwrap();
        let mut events = client.take_events().unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        assert_eq!(
            client
                .handle()
                .try_admit(Command::RecoverSession)
                .unwrap_err()
                .recovery_failure(),
            Some(RecoveryFailure::InProgress)
        );
        release.send(()).unwrap();
        let error = terminal(&recovery).unwrap_err();
        assert_eq!(error.store_failure(), Some(StoreFailure::Clear));
        assert_eq!(error.recovery_failure(), Some(RecoveryFailure::Persistence));
        let snapshot = recovery.completion.recovery_snapshot().unwrap();
        assert_eq!(snapshot.phase, RecoveryPhase::Failed);
        assert_eq!(
            snapshot.failure_phase,
            Some(RecoveryPhase::ClearingCheckpoint)
        );
        assert!(snapshot.abandonment_committed);
        assert!(!snapshot.checkpoint_cleared && !snapshot.fresh_established);
        until(&mut events, |event| {
            matches!(event, WrapperEvent::DriverTerminated(_))
        });
        client.join(DEADLINE).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(clears.load(Ordering::SeqCst), 1);
        assert!(client.handle().try_admit(Command::RecoverSession).is_err());
    }
}

#[test]
fn recovery_uses_remaining_reconnect_budget_without_an_extra_free_attempt() {
    for mqtt5 in [false, true] {
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut config = config(mqtt5, false, 1883);
        config.common.connector = Some(TransportConnectorConfig {
            connector: Arc::new(FailingAttempt {
                entered: entered_tx,
                release: Mutex::new(Some(released)),
                calls: calls.clone(),
                failure: TransportFailure::Connect,
            }),
            mode: TransportMode::Base,
        });
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            budget: RetryBudget::Limited(0),
            ..ReconnectConfig::default()
        });
        let client = start(config).unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        release.send(()).unwrap();
        assert_eq!(
            terminal(&recovery).unwrap_err().code(),
            ErrorCode::ReconnectExhausted
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            recovery
                .completion
                .recovery_snapshot()
                .unwrap()
                .checkpoint_cleared
        );
        client.join(DEADLINE).unwrap();
    }
}

#[test]
fn shutdown_during_clear_retains_progress_and_never_reopens_admission() {
    for mqtt5 in [false, true] {
        let modes = if cfg!(feature = "ordered-shutdown") {
            3
        } else {
            2
        };
        for mode in 0..modes {
            let graceful = mode != 0;
            let (attempt_tx, attempt) = mpsc::channel();
            let (release_attempt, attempted) = tokio::sync::oneshot::channel();
            let (clear_tx, clearing) = mpsc::channel();
            let (release_clear, cleared) = tokio::sync::oneshot::channel();
            let calls = Arc::new(AtomicUsize::new(0));
            let mut config = config(mqtt5, true, 1883);
            config.common.connector = Some(TransportConnectorConfig {
                connector: Arc::new(FailingAttempt {
                    entered: attempt_tx,
                    release: Mutex::new(Some(attempted)),
                    calls: calls.clone(),
                    failure: TransportFailure::Connect,
                }),
                mode: TransportMode::Base,
            });
            let store = Some(SessionStoreConfig::new(
                Arc::new(ClearStore {
                    fail: false,
                    clears: Arc::new(AtomicUsize::new(0)),
                    release: Mutex::new(Some(cleared)),
                    entered: Some(clear_tx),
                }),
                "recovery",
            ));
            match &mut config.protocol {
                ProtocolConfig::V4(v4) => v4.session_store = store,
                ProtocolConfig::V5(v5) => v5.session_store = store,
            }
            let client = start(config).unwrap();
            attempt.recv_timeout(DEADLINE).unwrap();
            let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
            release_attempt.send(()).unwrap();
            clearing.recv_timeout(DEADLINE).unwrap();
            assert_eq!(
                recovery.completion.recovery_snapshot().unwrap().phase,
                RecoveryPhase::ClearingCheckpoint
            );
            let shutdown = client
                .handle()
                .try_admit(match mode {
                    0 => Command::ImmediateDisconnect,
                    1 => Command::GracefulDisconnect {
                        timeout: Some(DEADLINE),
                    },
                    _ => Command::OrderedDisconnect {
                        timeout: Some(Duration::from_millis(30)),
                    },
                })
                .unwrap();
            if graceful {
                release_clear.send(()).unwrap();
            }
            if mode == 2 {
                assert!(terminal(&shutdown).is_err());
            } else {
                assert!(terminal(&shutdown).is_ok());
            }
            let error = terminal(&recovery).unwrap_err();
            assert_eq!(error.recovery_failure(), Some(RecoveryFailure::Interrupted));
            let snapshot = recovery.completion.recovery_snapshot().unwrap();
            assert_eq!(snapshot.phase, RecoveryPhase::Interrupted);
            assert!(!snapshot.fresh_established);
            assert_eq!(snapshot.checkpoint_cleared, graceful);
            client.join(DEADLINE).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(client.handle().try_admit(subscribe()).is_err());
        }
    }
}

#[derive(Default)]
struct MemoryStore(Mutex<Option<SessionCheckpoint>>);
impl SessionStore for MemoryStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        let checkpoint = self.0.lock().unwrap().clone();
        Box::pin(async move { Ok(checkpoint) })
    }
    fn save(&self, _: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
        *self.0.lock().unwrap() = Some(checkpoint);
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        *self.0.lock().unwrap() = None;
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn abandonment_retires_inflight_qos2_subscriptions_and_incoming_ownership() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, released) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut old = accept(&listener);
            frame(&mut old);
            if mqtt5 {
                // Allow a manual topic alias on the old connection.
                old.write_all(&[0x20, 6, 0, 0, 3, 0x22, 0, 1]).unwrap();
            } else {
                connack(&mut old, false, false);
            }
            for _ in 0..4 {
                let packet = frame(&mut old);
                if packet[0] & 6 == 4 && packet[0] >> 4 == 3 {
                    let n = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
                    old.write_all(&[0x50, 2, packet[4 + n], packet[5 + n]])
                        .unwrap();
                }
            }
            assert_eq!(frame(&mut old)[0], 0x62);
            old.write_all(if mqtt5 {
                &[0x34, 7, 0, 1, b'i', 0, 10, 0, b'x']
            } else {
                &[0x34, 6, 0, 1, b'i', 0, 10, b'x']
            })
            .unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            drop(old);
            let mut fresh = accept(&listener);
            assert_ne!(frame(&mut fresh)[9] & 2, 0);
            connack(&mut fresh, mqtt5, false);
            if !mqtt5 {
                assert_eq!(frame(&mut fresh)[0], 0xe0);
                drop(fresh);
                fresh = accept(&listener);
                assert_eq!(frame(&mut fresh)[9] & 2, 0);
                connack(&mut fresh, false, false);
            }
            // Only a new application publish may appear; no old PUBLISH/PUBREL or subscriptions.
            let packet = frame(&mut fresh);
            assert_eq!(packet[0], 0x32);
            let n = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
            assert_eq!(&packet[4..4 + n], b"fresh");
            support::puback(
                &mut fresh,
                u16::from_be_bytes([packet[4 + n], packet[5 + n]]),
            );
            assert_eq!(frame(&mut fresh)[0], 0xe0);
            drop(fresh);
            let mut restarted = accept(&listener);
            assert_eq!(frame(&mut restarted)[9] & 2, 0);
            connack(&mut restarted, mqtt5, true);
            let packet = frame(&mut restarted);
            assert_eq!(packet[0], 0x32);
            let n = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
            assert_eq!(
                &packet[4..4 + n],
                b"restart",
                "abandoned checkpoint must not replay after restart"
            );
            support::puback(
                &mut restarted,
                u16::from_be_bytes([packet[4 + n], packet[5 + n]]),
            );
            assert_eq!(frame(&mut restarted)[0], 0xe0);
        });
        let store = Arc::new(MemoryStore::default());
        let mut config = config(mqtt5, true, port);
        config.common.ack_mode = AckMode::Manual;
        config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::from_millis(200),
            maximum_delay: Duration::from_millis(200),
            jitter: ReconnectJitter::None,
            ..ReconnectConfig::default()
        });
        let persistence = Some(SessionStoreConfig::new(store.clone(), "recovery"));
        match &mut config.protocol {
            ProtocolConfig::V4(v4) => v4.session_store = persistence,
            ProtocolConfig::V5(v5) => v5.session_store = persistence,
        }
        let restart_config = config.clone();
        let mut client = start(config).unwrap();
        let mut events = support::connected(&mut client);
        let mut discarded = vec![client.handle().try_admit(subscribe()).unwrap()];
        discarded.push(
            client
                .handle()
                .try_admit(Command::Unsubscribe(UnsubscribeCommand {
                    filters: vec!["old".into()],
                    protocol: UnsubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap(),
        );
        for qos in [QoS::AtLeastOnce, QoS::ExactlyOnce] {
            discarded.push(
                client
                    .handle()
                    .try_admit(Command::Publish(PublishCommand {
                        topic: "old".into(),
                        payload: b"old".as_slice().into(),
                        qos,
                        retain: false,
                        protocol: if mqtt5 {
                            PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                                topic_alias: Some(1),
                                ..V5OutgoingPublishProperties::default()
                            })
                        } else {
                            PublishProtocolOptions::VersionNeutral
                        },
                    }))
                    .unwrap(),
            );
        }
        let token = match until(&mut events, |event| {
            matches!(event, WrapperEvent::IncomingPublish(_))
        }) {
            WrapperEvent::IncomingPublish(publish) => publish.ack_token.unwrap(),
            _ => unreachable!(),
        };
        assert!(store.0.lock().unwrap().is_some());
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        assert_eq!(terminal(&recovery).unwrap(), Completion::SessionRecovered);
        for operation in discarded {
            let error = terminal(&operation).unwrap_err();
            assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        }
        assert!(
            client
                .handle()
                .try_admit(Command::Acknowledge(token))
                .is_err()
        );
        if mqtt5 {
            assert_eq!(
                client
                    .handle()
                    .publish_budget_snapshot()
                    .unwrap()
                    .outstanding,
                0
            );
            assert_eq!(
                client
                    .handle()
                    .try_admit(Command::Publish(PublishCommand {
                        topic: String::new(),
                        payload: b"old".as_slice().into(),
                        qos: QoS::AtLeastOnce,
                        retain: false,
                        protocol: PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                            topic_alias: Some(1),
                            ..V5OutgoingPublishProperties::default()
                        }),
                    }))
                    .unwrap_err()
                    .publish_failure(),
                Some(PublishFailure::TopicAliasMaximum)
            );
        }
        let fresh = client
            .handle()
            .try_admit(Command::Publish(PublishCommand {
                topic: "fresh".into(),
                payload: b"new".as_slice().into(),
                qos: QoS::AtLeastOnce,
                retain: false,
                protocol: PublishProtocolOptions::VersionNeutral,
            }))
            .unwrap();
        assert!(terminal(&fresh).is_ok());
        client.closer().close(DEADLINE).unwrap();
        drop(client);
        let mut restarted = start(restart_config).unwrap();
        let _events = support::connected(&mut restarted);
        let publish = restarted
            .handle()
            .try_admit(Command::Publish(PublishCommand {
                topic: "restart".into(),
                payload: b"new".as_slice().into(),
                qos: QoS::AtLeastOnce,
                retain: false,
                protocol: PublishProtocolOptions::VersionNeutral,
            }))
            .unwrap();
        assert!(terminal(&publish).is_ok());
        restarted.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn unexpected_session_present_is_terminal_at_each_fresh_boundary() {
    // v4 persistent's final boundary must be fresh even with lenient clean-CONNACK policy.
    for (mqtt5, final_persistent) in [(false, false), (false, true), (true, false)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut candidate = accept(&listener);
            frame(&mut candidate);
            entered_tx.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            connack(&mut candidate, mqtt5, false);
            assert_eq!(frame(&mut candidate)[0], 0xe0);
            drop(candidate);
            let mut fresh = accept(&listener);
            assert_ne!(frame(&mut fresh)[9] & 2, 0);
            if final_persistent {
                connack(&mut fresh, false, false);
                assert_eq!(frame(&mut fresh)[0], 0xe0);
                drop(fresh);
                fresh = accept(&listener);
                assert_eq!(frame(&mut fresh)[9] & 2, 0);
            }
            connack(&mut fresh, mqtt5, true);
        });
        let mut settings = config(mqtt5, final_persistent, port);
        if final_persistent && let ProtocolConfig::V4(v4) = &mut settings.protocol {
            v4.session_present_mismatch_policy = SessionPresentMismatchPolicy::AcceptAsClean;
        }
        let mut client = start(settings).unwrap();
        let mut events = client.take_events().unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        release.send(()).unwrap();
        let error = terminal(&recovery).unwrap_err();
        assert_eq!(
            error.recovery_failure(),
            Some(RecoveryFailure::Establishment)
        );
        let progress = recovery.completion.recovery_snapshot().unwrap();
        assert_eq!(progress.phase, RecoveryPhase::Failed);
        assert!(progress.abandonment_committed && progress.checkpoint_cleared);
        assert!(!progress.fresh_established);
        client.join(DEADLINE).unwrap();
        while let Ok(Some(event)) = events.try_recv() {
            assert!(!matches!(event, WrapperEvent::Connected { .. }));
        }
        assert!(client.handle().try_admit(Command::RecoverSession).is_err());
        broker.join();
    }
}

#[test]
fn retrying_fresh_establishment_preserves_transient_and_long_term_session_policy() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut candidate = accept(&listener);
            frame(&mut candidate);
            entered_tx.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            connack(&mut candidate, mqtt5, false);
            assert_eq!(frame(&mut candidate)[0], 0xe0);
            drop(candidate);
            let mut fresh = accept(&listener);
            assert_ne!(frame(&mut fresh)[9] & 2, 0);
            if !mqtt5 {
                connack(&mut fresh, false, false);
                assert_eq!(frame(&mut fresh)[0], 0xe0);
                drop(fresh);
                fresh = accept(&listener);
                assert_eq!(frame(&mut fresh)[9] & 2, 0);
            }
            // A failed v5 fresh attempt repeats Clean Start. A failed v4 persistent
            // attempt retries persistent mode without repeating its completed clean reset.
            drop(fresh);
            let mut recovered = accept(&listener);
            assert_eq!(frame(&mut recovered)[9] & 2 != 0, mqtt5);
            connack(&mut recovered, mqtt5, false);
            // Ordinary reconnect after recovery uses the original persistent session policy.
            drop(recovered);
            let mut resumed = accept(&listener);
            assert_eq!(frame(&mut resumed)[9] & 2, 0);
            connack(&mut resumed, mqtt5, false);
            assert_eq!(frame(&mut resumed)[0], 0xe0);
        });
        let mut settings = config(mqtt5, true, port);
        settings.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
            initial_delay: Duration::from_millis(20),
            maximum_delay: Duration::from_millis(20),
            jitter: ReconnectJitter::None,
            ..Default::default()
        });
        let mut client = start(settings).unwrap();
        let mut events = client.take_events().unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        release.send(()).unwrap();
        assert_eq!(terminal(&recovery).unwrap(), Completion::SessionRecovered);
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn racing_producers_are_either_discarded_or_definitely_not_admitted() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let broker = Broker::spawn(move || {
            let mut candidate = accept(&listener);
            frame(&mut candidate);
            entered_tx.send(()).unwrap();
            released.recv_timeout(DEADLINE).unwrap();
            connack(&mut candidate, mqtt5, false);
            assert_eq!(frame(&mut candidate)[0], 0xe0);
            drop(candidate);
            let mut fresh = accept(&listener);
            assert_ne!(frame(&mut fresh)[9] & 2, 0);
            connack(&mut fresh, mqtt5, false);
            assert_eq!(frame(&mut fresh)[0], 0xe0);
        });
        let mut client = start(config(mqtt5, false, port)).unwrap();
        let mut events = client.take_events().unwrap();
        entered.recv_timeout(DEADLINE).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let producers: Vec<_> = (0..8)
            .map(|_| {
                let handle = client.handle();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    handle.try_admit(subscribe())
                })
            })
            .collect();
        barrier.wait();
        let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
        let outcomes: Vec<_> = producers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        // Dropping the only user observer must not reopen admission or cancel the transaction.
        drop(recovery);
        assert_eq!(
            client
                .handle()
                .try_admit(subscribe())
                .unwrap_err()
                .delivery_status(),
            DeliveryStatus::NotAdmitted
        );
        release.send(()).unwrap();
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        for outcome in outcomes {
            match outcome {
                Ok(operation) => assert!(
                    terminal(&operation)
                        .unwrap_err()
                        .message()
                        .contains("session reset")
                ),
                Err(error) => {
                    assert_eq!(error.kind(), ErrorKind::Backpressure);
                    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
                    assert_eq!(error.recovery_failure(), Some(RecoveryFailure::InProgress));
                }
            }
        }
        client.closer().close(DEADLINE).unwrap();
        broker.join();
    }
}

#[derive(Clone, Copy)]
enum RetirementFailure {
    Write,
    Flush,
}

struct RetirementFailureConnector {
    inner: Arc<dyn TransportConnector>,
    calls: Arc<AtomicUsize>,
    failing_attempt: usize,
    failure: RetirementFailure,
}
impl TransportConnector for RetirementFailureConnector {
    fn connect(&self, request: TransportRequest) -> TransportFuture {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let connection = self.inner.connect(request);
        let failing_attempt = self.failing_attempt;
        let failure = self.failure;
        Box::pin(async move {
            let mut connection = connection.await?;
            if attempt == failing_attempt {
                connection.io = Arc::new(RetirementFailureIo {
                    inner: connection.io,
                    failure,
                    disconnect_written: AtomicBool::new(false),
                });
            }
            Ok(connection)
        })
    }
}

struct RetirementFailureIo {
    inner: Arc<dyn TransportIo>,
    failure: RetirementFailure,
    disconnect_written: AtomicBool,
}
impl TransportIo for RetirementFailureIo {
    fn read(&self, max: usize) -> TransportIoFuture<bytes::Bytes> {
        self.inner.read(max)
    }
    fn write(&self, bytes: bytes::Bytes) -> TransportIoFuture<usize> {
        if bytes.first() == Some(&0xe0) {
            if matches!(self.failure, RetirementFailure::Write) {
                return Box::pin(async { Err(TransportFailure::Io.into_io()) });
            }
            self.disconnect_written.store(true, Ordering::SeqCst);
        }
        self.inner.write(bytes)
    }
    fn flush(&self) -> TransportIoFuture<()> {
        if self.disconnect_written.swap(false, Ordering::SeqCst)
            && matches!(self.failure, RetirementFailure::Flush)
        {
            return Box::pin(async { Err(TransportFailure::Io.into_io()) });
        }
        self.inner.flush()
    }
    fn shutdown(&self) -> TransportIoFuture<()> {
        self.inner.shutdown()
    }
}

#[test]
fn failed_abandonment_preserves_the_transport_error_without_retrying_establishment() {
    for mqtt5 in [false, true] {
        for classified in [false, true] {
            for failure in [RetirementFailure::Write, RetirementFailure::Flush] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let (handshake_tx, handshake) = mpsc::channel();
                let (release, released) = mpsc::channel();
                let (finished_tx, finished) = mpsc::channel();
                let broker = Broker::spawn(move || {
                    let mut socket = accept(&listener);
                    frame(&mut socket);
                    handshake_tx.send(()).unwrap();
                    released.recv_timeout(DEADLINE).unwrap();
                    connack(&mut socket, mqtt5, false);
                    if matches!(failure, RetirementFailure::Flush) {
                        assert_eq!(frame(&mut socket)[0], 0xe0);
                    }
                    finished.recv_timeout(DEADLINE).unwrap();
                    listener.set_nonblocking(true).unwrap();
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                });
                let mut settings = config(mqtt5, true, port);
                if classified {
                    settings.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
                        initial_delay: Duration::ZERO,
                        maximum_delay: Duration::ZERO,
                        budget: RetryBudget::Unlimited,
                        ..ReconnectConfig::default()
                    });
                }
                let calls = Arc::new(AtomicUsize::new(0));
                let (inner, _) = custom_transport::configured_with_write_chunk(usize::MAX);
                settings.common.connector = Some(TransportConnectorConfig {
                    connector: Arc::new(RetirementFailureConnector {
                        inner: inner.connector,
                        calls: calls.clone(),
                        failing_attempt: 1,
                        failure,
                    }),
                    mode: TransportMode::Base,
                });
                // Finish the candidate's native CONNACK checkpoint before retirement.
                // Any subsequent clear would belong to abandonment, which must not commit.
                let clears = Arc::new(AtomicUsize::new(0));
                let (checkpoint_tx, checkpoint) = mpsc::channel();
                let (release_checkpoint, checkpoint_released) = tokio::sync::oneshot::channel();
                let store = Some(SessionStoreConfig::new(
                    Arc::new(ClearStore {
                        fail: false,
                        clears: clears.clone(),
                        release: Mutex::new(Some(checkpoint_released)),
                        entered: Some(checkpoint_tx),
                    }),
                    "failed-abandonment",
                ));
                match &mut settings.protocol {
                    ProtocolConfig::V4(v4) => v4.session_store = store,
                    ProtocolConfig::V5(v5) => v5.session_store = store,
                }
                let mut client = start(settings).unwrap();
                let mut events = client.take_events().unwrap();
                handshake.recv_timeout(DEADLINE).unwrap();
                let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
                release.send(()).unwrap();
                checkpoint.recv_timeout(DEADLINE).unwrap();
                let candidate_clears = clears.load(Ordering::SeqCst);
                release_checkpoint.send(()).unwrap();

                let error = terminal(&recovery).unwrap_err();
                assert_eq!(error.transport_failure(), Some(TransportFailure::Io));
                assert_eq!(error.kind(), ErrorKind::Network);
                assert!(error.retryable());
                assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
                assert_eq!(error.recovery_failure(), Some(RecoveryFailure::Transition));
                let progress = recovery.completion.recovery_snapshot().unwrap();
                assert_eq!(progress.phase, RecoveryPhase::Failed);
                assert_eq!(progress.failure_phase, Some(RecoveryPhase::Abandoning));
                assert!(
                    !progress.abandonment_committed
                        && !progress.checkpoint_cleared
                        && !progress.fresh_established
                );
                client.join(DEADLINE).unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(client.handle().reconnect_diagnostics().cycles_started, 1);
                assert_eq!(clears.load(Ordering::SeqCst), candidate_clears);
                let event = until(&mut events, |event| {
                    assert!(!matches!(event, WrapperEvent::Connected { .. }));
                    matches!(event, WrapperEvent::DriverTerminated(_))
                });
                let WrapperEvent::DriverTerminated(error) = event else {
                    unreachable!()
                };
                assert_eq!(error.transport_failure(), Some(TransportFailure::Io));
                finished_tx.send(()).unwrap();
                broker.join();
            }
        }
    }
}

#[test]
fn failed_recovery_candidate_retirement_fails_graceful_shutdown() {
    for mqtt5 in [false, true] {
        for failing_attempt in 1..=if mqtt5 { 2 } else { 3 } {
            for failure in [RetirementFailure::Write, RetirementFailure::Flush] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let (handshake_tx, handshake) = mpsc::channel();
                let (release, released) = mpsc::channel();
                let (finished_tx, finished) = mpsc::channel();
                let broker = Broker::spawn(move || {
                    for attempt in 1..=failing_attempt {
                        let mut socket = accept(&listener);
                        frame(&mut socket);
                        handshake_tx.send(attempt).unwrap();
                        released.recv_timeout(DEADLINE).unwrap();
                        connack(&mut socket, mqtt5, false);
                        if attempt < failing_attempt || matches!(failure, RetirementFailure::Flush)
                        {
                            assert_eq!(frame(&mut socket)[0], 0xe0);
                        }
                        if attempt == failing_attempt {
                            finished.recv_timeout(DEADLINE).unwrap();
                        }
                    }
                    listener.set_nonblocking(true).unwrap();
                    assert_eq!(
                        listener.accept().unwrap_err().kind(),
                        std::io::ErrorKind::WouldBlock
                    );
                });
                let mut settings = config(mqtt5, true, port);
                let calls = Arc::new(AtomicUsize::new(0));
                let (inner, _) = custom_transport::configured_with_write_chunk(usize::MAX);
                settings.common.connector = Some(TransportConnectorConfig {
                    connector: Arc::new(RetirementFailureConnector {
                        inner: inner.connector,
                        calls: calls.clone(),
                        failing_attempt,
                        failure,
                    }),
                    mode: TransportMode::Base,
                });
                let mut client = start(settings).unwrap();
                let mut events = client.take_events().unwrap();
                assert_eq!(handshake.recv_timeout(DEADLINE).unwrap(), 1);
                let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
                for attempt in 2..=failing_attempt {
                    release.send(()).unwrap();
                    assert_eq!(handshake.recv_timeout(DEADLINE).unwrap(), attempt);
                }
                let shutdown = client
                    .handle()
                    .try_admit(Command::GracefulDisconnect {
                        timeout: Some(DEADLINE),
                    })
                    .unwrap();
                release.send(()).unwrap();

                let error = terminal(&shutdown)
                    .expect_err("failed retirement must not report graceful success");
                assert_eq!(error.kind(), ErrorKind::Network);
                assert_eq!(error.transport_failure(), Some(TransportFailure::Io));
                assert!(error.retryable());
                let recovery_error = terminal(&recovery).unwrap_err();
                assert_eq!(recovery_error.kind(), error.kind());
                assert_eq!(
                    recovery_error.transport_failure(),
                    error.transport_failure()
                );
                assert_eq!(
                    recovery_error.recovery_failure(),
                    Some(RecoveryFailure::Interrupted)
                );
                let progress = recovery.completion.recovery_snapshot().unwrap();
                assert_eq!(progress.phase, RecoveryPhase::Interrupted);
                assert_eq!(progress.abandonment_committed, failing_attempt > 1);
                assert_eq!(progress.checkpoint_cleared, failing_attempt > 1);
                assert!(!progress.fresh_established);
                client.join(DEADLINE).unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), failing_attempt);
                loop {
                    let event = events
                        .recv_timeout(DEADLINE)
                        .unwrap()
                        .expect("driver must report failure");
                    match event {
                        WrapperEvent::Connected { .. } => {
                            panic!("retired candidate must stay hidden")
                        }
                        WrapperEvent::DriverTerminated(error) => {
                            assert_eq!(error.kind(), ErrorKind::Network);
                            assert_eq!(error.transport_failure(), Some(TransportFailure::Io));
                            break;
                        }
                        _ => {}
                    }
                }
                finished_tx.send(()).unwrap();
                broker.join();
            }
        }
    }
}

#[test]
fn shutdown_during_recovery_handshakes_retires_candidates_with_committed_options() {
    for (mqtt5, options, fail_clear) in [
        (false, None, false),
        (true, None, false),
        (
            true,
            Some(V5DisconnectOptions {
                session_expiry_interval: Some(0),
                ..Default::default()
            }),
            false,
        ),
        (
            true,
            Some(V5DisconnectOptions {
                reason_code: 4,
                session_expiry_interval: Some(0),
                reason_string: Some("operator shutdown".into()),
                user_properties: vec![("source".into(), "recovery".into())],
                ..Default::default()
            }),
            false,
        ),
        (
            true,
            Some(V5DisconnectOptions {
                session_expiry_interval: Some(0),
                ..Default::default()
            }),
            true,
        ),
    ] {
        for shutdown_at_fresh in [false, true] {
            if !shutdown_at_fresh && (!mqtt5 || fail_clear) {
                continue;
            }
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (candidate_tx, candidate) = mpsc::channel();
            let (release_candidate, candidate_released) = mpsc::channel();
            let (fresh_tx, fresh) = mpsc::channel();
            let (release_fresh, fresh_released) = mpsc::channel();
            let (finished_tx, finished) = mpsc::channel();
            let expected = options.clone();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                frame(&mut socket);
                candidate_tx.send(()).unwrap();
                candidate_released.recv_timeout(DEADLINE).unwrap();
                connack(&mut socket, mqtt5, false);
                if shutdown_at_fresh {
                    assert_eq!(frame(&mut socket)[0], 0xe0);
                    drop(socket);
                    socket = accept(&listener);
                    assert_ne!(frame(&mut socket)[9] & 2, 0);
                    fresh_tx.send(()).unwrap();
                    fresh_released.recv_timeout(DEADLINE).unwrap();
                    connack(&mut socket, mqtt5, false);
                }
                let mut packet = frame(&mut socket);
                assert_eq!(packet[0], 0xe0);
                if let Some(expected) = expected {
                    let rumqttc_v5::Packet::Disconnect(disconnect) =
                        rumqttc_v5::Packet::read(&mut packet, None).unwrap()
                    else {
                        panic!("expected DISCONNECT");
                    };
                    assert_eq!(disconnect.reason_code as u8, expected.reason_code);
                    let properties = disconnect
                        .properties
                        .expect("shutdown properties must be sent");
                    assert_eq!(
                        properties.session_expiry_interval,
                        expected.session_expiry_interval
                    );
                    assert_eq!(properties.reason_string, expected.reason_string);
                    assert_eq!(properties.user_properties, expected.user_properties);
                }
                finished.recv_timeout(DEADLINE).unwrap();
                listener.set_nonblocking(true).unwrap();
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            });
            let mut settings = config(mqtt5, true, port);
            let clears = Arc::new(AtomicUsize::new(0));
            if options.is_some() {
                let ProtocolConfig::V5(v5) = &mut settings.protocol else {
                    unreachable!()
                };
                v5.session_store = Some(SessionStoreConfig::new(
                    Arc::new(RetirementStore {
                        clears: clears.clone(),
                        fail_clear,
                    }),
                    "retirement",
                ));
            }
            if options
                .as_ref()
                .is_some_and(|options| options.reason_code == 4)
            {
                settings.common.last_will = Some(LastWillConfig {
                    topic: "recovery/will".into(),
                    payload: "shutdown".into(),
                    qos: QoS::AtMostOnce,
                    retain: false,
                    protocol: LastWillProtocolOptions::VersionNeutral,
                });
            }
            let has_properties = options.is_some();
            let mut client = start(settings).unwrap();
            let mut events = client.take_events().unwrap();
            candidate.recv_timeout(DEADLINE).unwrap();
            let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
            if shutdown_at_fresh {
                release_candidate.send(()).unwrap();
                fresh.recv_timeout(DEADLINE).unwrap();
            }
            let shutdown = client
                .handle()
                .try_admit(Command::GracefulDisconnectWithOptions {
                    timeout: Some(DEADLINE),
                    protocol: options.clone().map_or(
                        DisconnectProtocolOptions::VersionNeutral,
                        DisconnectProtocolOptions::V5,
                    ),
                })
                .unwrap();
            assert_eq!(
                client
                    .handle()
                    .try_admit(Command::RecoverSession)
                    .unwrap_err()
                    .recovery_failure(),
                Some(RecoveryFailure::Unavailable)
            );
            if shutdown_at_fresh {
                release_fresh.send(()).unwrap();
            } else {
                release_candidate.send(()).unwrap();
            }
            let closed = terminal(&shutdown);
            if fail_clear {
                assert_eq!(closed.unwrap_err().kind(), ErrorKind::Persistence);
            } else {
                assert!(closed.is_ok());
            }
            if has_properties {
                assert_eq!(
                    clears.load(Ordering::SeqCst),
                    if shutdown_at_fresh { 4 } else { 2 },
                    "retirement must clear the appropriate checkpoint"
                );
            }
            assert_eq!(
                terminal(&recovery).unwrap_err().recovery_failure(),
                Some(RecoveryFailure::Interrupted)
            );
            let progress = recovery.completion.recovery_snapshot().unwrap();
            assert_eq!(progress.phase, RecoveryPhase::Interrupted);
            assert!(
                progress.abandonment_committed
                    && progress.checkpoint_cleared
                    && !progress.fresh_established
            );
            client.join(DEADLINE).unwrap();
            while let Ok(Some(event)) = events.try_recv() {
                assert!(!matches!(event, WrapperEvent::Connected { .. }));
            }
            finished_tx.send(()).unwrap();
            broker.join();
        }
    }
}

struct RetirementStore {
    clears: Arc<AtomicUsize>,
    fail_clear: bool,
}
impl SessionStore for RetirementStore {
    fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        Box::pin(async { Ok(None) })
    }
    fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
        Box::pin(async { Ok(()) })
    }
    fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
        // Candidate CONNACK, abandonment, and fresh CONNACK each clear once.
        // The fourth clear belongs to the committed zero-expiry DISCONNECT.
        let retirement = self.clears.fetch_add(1, Ordering::SeqCst) == 3;
        let fail = self.fail_clear && retirement;
        Box::pin(async move {
            if fail {
                Err(StoreFailure::Clear)
            } else {
                Ok(())
            }
        })
    }
}

struct CountRedirects(Arc<AtomicUsize>);
impl RedirectAuthority for CountRedirects {
    fn decide(
        &self,
        request: Arc<RedirectRequest>,
    ) -> std::result::Result<RedirectResponse, RedirectDecisionFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(RedirectResponse::reject(request))
    }
}

#[test]
fn fresh_recovery_rejects_redirects_before_invoking_or_applying_another_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let broker = Broker::spawn(move || {
        let mut candidate = accept(&listener);
        frame(&mut candidate);
        entered_tx.send(()).unwrap();
        released.recv_timeout(DEADLINE).unwrap();
        connack(&mut candidate, true, false);
        assert_eq!(frame(&mut candidate)[0], 0xe0);
        drop(candidate);
        let mut fresh = accept(&listener);
        frame(&mut fresh);
        // Use Another Server with an advertised address. Policy must not run.
        fresh
            .write_all(b"\x20\x11\x00\x9c\x0e\x1c\x00\x0bexample:1883")
            .unwrap();
    });
    let redirects = Arc::new(AtomicUsize::new(0));
    let mut settings = config(true, true, port);
    let ProtocolConfig::V5(v5) = &mut settings.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Application(RedirectAuthorityConfig {
        authority: Arc::new(CountRedirects(redirects.clone())),
        max_attempts: 2,
        decision_timeout: DEADLINE,
    });
    let client = start(settings).unwrap();
    entered.recv_timeout(DEADLINE).unwrap();
    let recovery = client.handle().try_admit(Command::RecoverSession).unwrap();
    release.send(()).unwrap();
    assert_eq!(
        terminal(&recovery).unwrap_err().recovery_failure(),
        Some(RecoveryFailure::Establishment)
    );
    let progress = recovery.completion.recovery_snapshot().unwrap();
    assert!(progress.checkpoint_cleared && !progress.fresh_established);
    assert_eq!(redirects.load(Ordering::SeqCst), 0);
    client.join(DEADLINE).unwrap();
    broker.join();
}
