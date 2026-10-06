use futures_util::FutureExt;
use rumqttc::{
    AsyncClient, Client, ClientError, Event, EventLoop, Incoming, MqttOptions, Outgoing,
    PersistedAckMode, PersistedPublish, PersistedQoS, PersistedRequest, PersistedSession,
    PublishAdmissionPolicy, PublishBudgetError, PublishBudgetLimits, PublishOptions, QoS,
    RedirectClientId, RedirectDecision, RedirectPolicy, RedirectSession, RedirectTargetProfile,
    SessionStore, SessionStoreError, SessionStoreKey, Transport,
};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{Notify, mpsc};

const DEADLINE: Duration = Duration::from_secs(3);
type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

fn memory_options() -> (MqttOptions, mpsc::UnboundedReceiver<DuplexStream>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut options = MqttOptions::new("budget-regression", "origin.example");
    options.set_socket_connector(move |_, _| {
        let (client, server) = tokio::io::duplex(4096);
        tx.send(server).unwrap();
        async move { Ok(client) }
    });
    (options, rx)
}

async fn frame(stream: &mut DuplexStream) -> Vec<u8> {
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
    panic!("invalid remaining length")
}

async fn handshake(rx: &mut mpsc::UnboundedReceiver<DuplexStream>, resumed: bool) -> DuplexStream {
    let mut stream = rx.recv().await.unwrap();
    assert_eq!(frame(&mut stream).await[0], 0x10);
    stream
        .write_all(&[0x20, 3, u8::from(resumed), 0, 0])
        .await
        .unwrap();
    stream
}

async fn connect(
    driver: &mut EventLoop,
    rx: &mut mpsc::UnboundedReceiver<DuplexStream>,
) -> DuplexStream {
    let (event, stream) = tokio::time::timeout(DEADLINE, async {
        tokio::join!(driver.poll(), handshake(rx, false))
    })
    .await
    .unwrap();
    assert!(matches!(
        event.unwrap(),
        Event::Incoming(Incoming::ConnAck(_))
    ));
    stream
}

#[tokio::test]
async fn rendezvous_publish_started_before_receiver_readiness_completes() {
    for policy in [
        PublishAdmissionPolicy::EventLoopValidated,
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
    ] {
        for budgeted in [false, true] {
            let (options, mut rx) = memory_options();
            let mut builder = AsyncClient::builder(options)
                .capacity(0)
                .publish_admission_policy(policy);
            if budgeted {
                builder = builder.publish_budget(PublishBudgetLimits {
                    max_outstanding: 1,
                    max_bytes: 5,
                });
            }
            let (client, mut driver) = builder.build();
            let mut stream = connect(&mut driver, &mut rx).await;
            for tracked in [false, true, false, true] {
                let mut publish = Box::pin(async {
                    if tracked {
                        Some(
                            client
                                .publish_tracked("a", "data", PublishOptions::new(QoS::AtMostOnce))
                                .await
                                .unwrap(),
                        )
                    } else {
                        client
                            .publish("a", "data", PublishOptions::new(QoS::AtMostOnce))
                            .await
                            .unwrap();
                        None
                    }
                });
                assert!(publish.as_mut().now_or_never().is_none());
                let (notice, event) =
                    tokio::time::timeout(DEADLINE, async { tokio::join!(publish, driver.poll()) })
                        .await
                        .expect("arming the receiver must wake a waiting publisher");
                assert!(matches!(
                    event.unwrap(),
                    Event::Outgoing(Outgoing::Publish(_))
                ));
                assert_eq!(frame(&mut stream).await[0], 0x30);
                if let Some(notice) = notice {
                    notice.wait_async().await.unwrap();
                }
                if let Some(snapshot) = client.publish_budget_snapshot() {
                    assert_eq!((snapshot.outstanding, snapshot.retained_bytes), (0, 0));
                }
            }
        }
    }
}

#[tokio::test]
async fn blocking_rendezvous_publish_wakes_when_receiver_becomes_ready() {
    let (options, mut rx) = memory_options();
    let (client, mut driver) = tokio::task::spawn_blocking(move || {
        let (client, connection) = Client::builder(options)
            .capacity(0)
            .publish_budget(PublishBudgetLimits {
                max_outstanding: 1,
                max_bytes: 5,
            })
            .build();
        (client, connection.eventloop)
    })
    .await
    .unwrap();
    let mut stream = connect(&mut driver, &mut rx).await;
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    let sender = tokio::task::spawn_blocking(move || {
        done_tx
            .send(client.publish("a", "data", PublishOptions::new(QoS::AtMostOnce)))
            .unwrap();
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut done_rx)
            .await
            .is_err()
    );
    let (result, event) =
        tokio::time::timeout(DEADLINE, async { tokio::join!(done_rx, driver.poll()) })
            .await
            .expect("receiver readiness must wake blocking publishers too");
    result.unwrap().unwrap();
    assert!(matches!(
        event.unwrap(),
        Event::Outgoing(Outgoing::Publish(_))
    ));
    assert_eq!(frame(&mut stream).await[0], 0x30);
    sender.await.unwrap();
}

#[tokio::test]
async fn cancelled_rendezvous_receive_rearms_publish_progress() {
    let (options, mut rx) = memory_options();
    let (client, mut driver) = AsyncClient::builder(options)
        .capacity(0)
        .publish_budget(PublishBudgetLimits {
            max_outstanding: 1,
            max_bytes: 5,
        })
        .build();
    let mut stream = connect(&mut driver, &mut rx).await;
    let mut publish = Box::pin(client.publish("a", "data", PublishOptions::new(QoS::AtMostOnce)));
    assert!(publish.as_mut().now_or_never().is_none());
    // Arm and cancel the receive before the publisher can use its rendezvous slot.
    assert!(driver.poll().now_or_never().is_none());
    assert!(publish.as_mut().now_or_never().is_none());
    let (result, event) =
        tokio::time::timeout(DEADLINE, async { tokio::join!(publish, driver.poll()) })
            .await
            .expect("rearming after cancellation must notify readiness again");
    result.unwrap();
    assert!(matches!(
        event.unwrap(),
        Event::Outgoing(Outgoing::Publish(_))
    ));
    assert_eq!(frame(&mut stream).await[0], 0x30);
    assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
}

#[derive(Debug)]
struct ScopedStore {
    target: Option<PersistedSession>,
    fail_load: AtomicBool,
    entered: mpsc::UnboundedSender<SessionStoreKey>,
    release: Notify,
}

impl SessionStore for ScopedStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        Box::pin(async move {
            self.entered.send(key.clone()).unwrap();
            if key.scope() == "target" || key.client_id() == "target-client" {
                self.release.notified().await;
                if self.fail_load.swap(false, Ordering::Relaxed) {
                    return Err(std::io::Error::other("load failed").into());
                }
                Ok(self.target.clone())
            } else {
                Ok(None)
            }
        })
    }
    fn save<'a>(&'a self, _: &'a SessionStoreKey, _: &'a PersistedSession) -> StoreFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn clear<'a>(&'a self, _: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn stored_publish(client_id: &str) -> PersistedSession {
    PersistedSession {
        format_version: 2,
        client_id: client_id.to_owned(),
        clean_start: false,
        session_expiry_interval: Some(60),
        outgoing_inflight_upper_limit: None,
        ack_mode: PersistedAckMode::Automatic,
        replay: vec![PersistedRequest::Publish(PersistedPublish {
            dup: true,
            qos: PersistedQoS::AtLeastOnce,
            retain: false,
            topic: b"a".to_vec(),
            pkid: 1,
            payload: b"data".to_vec(),
            properties: None,
        })],
        incoming_qos2: Vec::new(),
    }
}

fn persistent_client(
    policy: PublishAdmissionPolicy,
    store: Arc<ScopedStore>,
    bytes: usize,
) -> (
    AsyncClient,
    EventLoop,
    mpsc::UnboundedReceiver<DuplexStream>,
) {
    let (mut options, rx) = memory_options();
    options
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60))
        .set_session_store_arc(store)
        .set_session_store_scope("origin");
    let (client, driver) = AsyncClient::builder(options)
        .publish_admission_policy(policy)
        .publish_budget(PublishBudgetLimits {
            max_outstanding: 1,
            max_bytes: bytes,
        })
        .build();
    (client, driver, rx)
}

#[tokio::test]
async fn fresh_load_errors_keep_admission_closed_and_empty_store_reopens_it() {
    for policy in [
        PublishAdmissionPolicy::EventLoopValidated,
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
    ] {
        for (checkpoint, fail_load) in [
            (None, false),
            (None, true),
            (Some(stored_publish("budget-regression")), false),
        ] {
            let (entered, mut loads) = mpsc::unbounded_channel();
            let over_budget = checkpoint.is_some();
            let store = Arc::new(ScopedStore {
                target: checkpoint,
                fail_load: AtomicBool::new(fail_load),
                entered,
                release: Notify::new(),
            });
            let (client, mut driver, mut rx) = persistent_client(policy, store.clone(), 4);
            let _origin = connect(&mut driver, &mut rx).await;
            loads.recv().await.unwrap();
            driver.clean();
            driver.options.set_session_store_scope("target");
            let mut reconnect = Box::pin(driver.poll());
            assert!(reconnect.as_mut().now_or_never().is_none());
            assert_eq!(loads.recv().await.unwrap().scope(), "target");
            assert!(client.publish_budget_snapshot().unwrap().recovery_pending);
            store.release.notify_one();
            if fail_load || over_budget {
                let error = tokio::time::timeout(DEADLINE, reconnect)
                    .await
                    .unwrap()
                    .unwrap_err();
                if over_budget {
                    assert!(matches!(
                        error,
                        rumqttc::ConnectionError::SessionRestore(
                            rumqttc::SessionRestoreError::PublishBudgetExceeded
                        )
                    ));
                } else {
                    assert!(matches!(error, rumqttc::ConnectionError::SessionStore(_)));
                }
                assert!(client.publish_budget_snapshot().unwrap().recovery_pending);
                assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
                assert!(matches!(
                    client.try_publish("b", "new", PublishOptions::new(QoS::AtMostOnce)),
                    Err(ClientError::PublishBudget {
                        reason: PublishBudgetError::RecoveryPending,
                        ..
                    })
                ));
                if over_budget {
                    continue;
                }
                // The next load returns no checkpoint. It must reopen admission after the retry.
                reconnect = Box::pin(driver.poll());
                assert!(reconnect.as_mut().now_or_never().is_none());
                assert_eq!(loads.recv().await.unwrap().scope(), "target");
                store.release.notify_one();
            }
            let (event, mut target) = tokio::time::timeout(DEADLINE, async {
                tokio::join!(reconnect, handshake(&mut rx, false))
            })
            .await
            .unwrap();
            assert!(matches!(
                event.unwrap(),
                Event::Incoming(Incoming::ConnAck(_))
            ));
            assert!(!client.publish_budget_snapshot().unwrap().recovery_pending);
            client
                .try_publish("b", "new", PublishOptions::new(QoS::AtMostOnce))
                .unwrap();
            assert!(matches!(
                driver.poll().await.unwrap(),
                Event::Outgoing(Outgoing::Publish(_))
            ));
            assert_eq!(frame(&mut target).await[0], 0x30);
            assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
        }
    }
}

#[tokio::test]
async fn ordinary_reconnect_keeps_loaded_session_admission_open_without_reloading() {
    for policy in [
        PublishAdmissionPolicy::EventLoopValidated,
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
    ] {
        let (entered, mut loads) = mpsc::unbounded_channel();
        let store = Arc::new(ScopedStore {
            target: None,
            fail_load: AtomicBool::new(false),
            entered,
            release: Notify::new(),
        });
        let (client, mut driver, mut rx) = persistent_client(policy, store, 5);
        let _origin = connect(&mut driver, &mut rx).await;
        loads.recv().await.unwrap();
        driver.clean();
        assert!(!client.publish_budget_snapshot().unwrap().recovery_pending);
        client
            .try_publish("a", "data", PublishOptions::new(QoS::AtMostOnce))
            .unwrap();
        let mut stream = connect(&mut driver, &mut rx).await;
        assert!(
            loads.try_recv().is_err(),
            "loaded sessions must not reload checkpoints on ordinary reconnects"
        );
        assert!(!client.publish_budget_snapshot().unwrap().recovery_pending);
        assert!(matches!(
            driver.poll().await.unwrap(),
            Event::Outgoing(Outgoing::Publish(_))
        ));
        assert_eq!(frame(&mut stream).await[0], 0x30);
        assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
    }
}

#[tokio::test]
async fn scope_changing_redirect_gates_new_publishes_until_checkpoint_is_reserved() {
    session_recovery(
        "target",
        RedirectClientId::Reuse,
        "budget-regression",
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn client_id_changing_redirect_gates_new_publishes_until_checkpoint_is_reserved() {
    session_recovery(
        "origin",
        RedirectClientId::Replace("target-client".to_owned()),
        "target-client",
        true,
        false,
    )
    .await;
}

#[tokio::test]
async fn public_store_scope_change_gates_new_publishes_until_checkpoint_is_reserved() {
    session_recovery(
        "target",
        RedirectClientId::Reuse,
        "budget-regression",
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn public_client_id_change_gates_new_publishes_until_checkpoint_is_reserved() {
    session_recovery(
        "origin",
        RedirectClientId::Replace("target-client".to_owned()),
        "target-client",
        false,
        false,
    )
    .await;
}

#[tokio::test]
async fn cancelled_session_key_recovery_keeps_publish_admission_closed_until_retry_succeeds() {
    session_recovery(
        "target",
        RedirectClientId::Reuse,
        "budget-regression",
        false,
        true,
    )
    .await;
}

async fn session_recovery(
    scope: &'static str,
    client_id: RedirectClientId,
    checkpoint_client_id: &str,
    redirect: bool,
    cancel_load: bool,
) {
    for policy in [
        PublishAdmissionPolicy::EventLoopValidated,
        PublishAdmissionPolicy::RequireNegotiatedCapabilities,
    ] {
        let (entered, mut loads) = mpsc::unbounded_channel();
        let store = Arc::new(ScopedStore {
            target: Some(stored_publish(checkpoint_client_id)),
            fail_load: AtomicBool::new(false),
            entered,
            release: Notify::new(),
        });
        let (mut options, mut rx) = memory_options();
        options
            .set_clean_start(false)
            .set_session_expiry_interval(Some(60))
            .set_session_store_arc(store.clone())
            .set_session_store_scope("origin")
            .set_redirect_policy(RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), {
                let client_id = client_id.clone();
                move |context| {
                    RedirectDecision::follow(
                        RedirectTargetProfile::isolated(
                            context.references[0].clone(),
                            Transport::tcp(),
                        )
                        .unwrap()
                        .client_id(client_id.clone())
                        .session(RedirectSession::Reuse {
                            store_scope: scope.to_owned(),
                        }),
                    )
                }
            }));
        let (client, mut driver) = AsyncClient::builder(options)
            .publish_admission_policy(policy)
            .publish_budget(PublishBudgetLimits {
                max_outstanding: 1,
                max_bytes: 5,
            })
            .build();
        let mut origin = connect(&mut driver, &mut rx).await;
        assert_eq!(loads.recv().await.unwrap().scope(), "origin");
        assert!(!client.publish_budget_snapshot().unwrap().recovery_pending);

        if redirect {
            let reference = b"target.example";
            let mut redirect = vec![
                0xe0,
                (5 + reference.len()) as u8,
                0x9c,
                (3 + reference.len()) as u8,
                0x1c,
                0,
                reference.len() as u8,
            ];
            redirect.extend(reference);
            origin.write_all(&redirect).await.unwrap();
            tokio::time::timeout(DEADLINE, async {
                loop {
                    if matches!(driver.poll().await.unwrap(), Event::Redirect(_)) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            // The gate must already be closed when the redirect event is returned, before load starts.
            assert!(client.publish_budget_snapshot().unwrap().recovery_pending);
            assert!(matches!(
                client.try_publish("b", "new", PublishOptions::new(QoS::AtMostOnce)),
                Err(ClientError::PublishBudget {
                    reason: PublishBudgetError::RecoveryPending,
                    ..
                })
            ));
        } else {
            driver.clean();
            driver.options.set_session_store_scope(scope);
            if let RedirectClientId::Replace(id) = &client_id {
                driver.options.set_client_id(id.clone());
            }
        }
        if cancel_load {
            let mut reconnect = Box::pin(driver.poll());
            assert!(reconnect.as_mut().now_or_never().is_none());
            assert_eq!(loads.recv().await.unwrap().scope(), scope);
            drop(reconnect);
            assert!(client.publish_budget_snapshot().unwrap().recovery_pending);
            assert!(matches!(
                client.try_publish("b", "new", PublishOptions::new(QoS::AtMostOnce)),
                Err(ClientError::PublishBudget {
                    reason: PublishBudgetError::RecoveryPending,
                    ..
                })
            ));
        }
        let mut reconnect = Box::pin(driver.poll());
        assert!(reconnect.as_mut().now_or_never().is_none());
        let key = loads.recv().await.unwrap();
        assert_eq!(key.scope(), scope);
        assert_eq!(key.client_id(), checkpoint_client_id);
        // A suspended store callback must also leave producer admission gated.
        assert!(matches!(
            client.try_publish("b", "new", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::PublishBudget {
                reason: PublishBudgetError::RecoveryPending,
                ..
            })
        ));
        assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
        let mut waiting =
            Box::pin(client.publish("b", "new", PublishOptions::new(QoS::AtMostOnce)));
        assert!(waiting.as_mut().now_or_never().is_none());
        store.release.notify_one();
        let (event, mut target) = tokio::time::timeout(DEADLINE, async {
            tokio::join!(reconnect, handshake(&mut rx, true))
        })
        .await
        .unwrap();
        assert!(matches!(
            event.unwrap(),
            Event::Incoming(Incoming::ConnAck(_))
        ));
        let snapshot = client.publish_budget_snapshot().unwrap();
        assert_eq!(
            (
                snapshot.outstanding,
                snapshot.retained_bytes,
                snapshot.recovery_pending
            ),
            (1, 5, false)
        );
        assert!(waiting.as_mut().now_or_never().is_none());
        assert!(matches!(
            driver.poll().await.unwrap(),
            Event::Outgoing(Outgoing::Publish(1))
        ));
        let replay = frame(&mut target).await;
        assert_eq!(replay[0], 0x3a);
        target.write_all(&[0x40, 2, 0, 1]).await.unwrap();
        assert!(matches!(
            driver.poll().await.unwrap(),
            Event::Incoming(Incoming::PubAck(_))
        ));
        tokio::time::timeout(DEADLINE, waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            driver.poll().await.unwrap(),
            Event::Outgoing(Outgoing::Publish(_))
        ));
        assert_eq!(frame(&mut target).await[0], 0x30);
        assert_eq!(client.publish_budget_snapshot().unwrap().outstanding, 0);
    }
}
