use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rumqttc::{
    AsyncAuthChallenge, AsyncAuthContext, AsyncAuthenticator, AuthAction, AuthError, AuthEvent,
    AuthFuture, ConnectionError, Event, EventLoop, MqttOptions,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn invalid_initial_authentication_properties_fail_and_reset_the_exchange() {
    #[derive(Clone, Debug, Default)]
    struct Authority {
        failures: Arc<std::sync::Mutex<Vec<AuthError>>>,
    }
    impl Authority {
        fn properties(&self) -> rumqttc::AuthProperties {
            rumqttc::AuthProperties {
                method: Some("different".into()),
                ..Default::default()
            }
        }
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            assert_eq!(context.kind, rumqttc::AuthExchangeKind::InitialConnect);
            assert!(matches!(challenge, AsyncAuthChallenge::Start));
            let properties = self.properties();
            Box::pin(async move { Ok(AuthAction::Send(properties)) })
        }
        fn failure(&self, context: AsyncAuthContext, error: AuthError) {
            assert_eq!(context.kind, rumqttc::AuthExchangeKind::InitialConnect);
            assert_eq!(context.method, "test");
            self.failures.lock().unwrap().push(error);
        }
    }
    impl rumqttc::Authenticator for Authority {
        fn start(
            &mut self,
            _: rumqttc::AuthContext<'_>,
        ) -> Result<Option<rumqttc::AuthProperties>, AuthError> {
            Ok(Some(self.properties()))
        }
        fn continue_auth(
            &mut self,
            _: rumqttc::AuthContext<'_>,
            _: Option<rumqttc::AuthProperties>,
        ) -> Result<AuthAction, AuthError> {
            panic!("invalid Start must not continue")
        }
        fn success(
            &mut self,
            _: rumqttc::AuthContext<'_>,
            _: Option<rumqttc::AuthProperties>,
        ) -> Result<(), AuthError> {
            panic!("invalid Start must not succeed")
        }
        fn failure(&mut self, context: rumqttc::AuthContext<'_>, error: AuthError) {
            assert_eq!(context.kind, rumqttc::AuthExchangeKind::InitialConnect);
            assert_eq!(context.method, "test");
            self.failures.lock().unwrap().push(error);
        }
    }

    // Both authority APIs must terminate invalid Start responses before reconnecting.
    for asynchronous in [true, false] {
        let authority = Authority::default();
        let mut options = MqttOptions::new("auth", "localhost");
        options.set_authentication_method(Some("test".into()));
        if asynchronous {
            options.set_async_authenticator(Arc::new(authority.clone()));
        } else {
            options.set_authenticator(Arc::new(std::sync::Mutex::new(authority.clone())));
        }
        let (peers_tx, mut peers_rx) = tokio::sync::mpsc::unbounded_channel();
        options.set_socket_connector(move |_, _| {
            let (client, peer) = tokio::io::duplex(1024);
            peers_tx.send(peer).unwrap();
            async move { Ok(client) }
        });
        let mut eventloop = EventLoop::new(options, 1);
        for attempt in 1..=2 {
            let result = tokio::time::timeout(Duration::from_secs(1), eventloop.poll())
                .await
                .unwrap();
            assert!(matches!(
                result,
                Err(ConnectionError::MqttState(rumqttc::StateError::AuthError(
                    _
                )))
            ));
            assert!(!eventloop.diagnostics().connected);
            let mut peer = peers_rx.recv().await.unwrap();
            // Invalid properties must be rejected before CONNECT reaches the broker.
            assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
            assert_eq!(authority.failures.lock().unwrap().len(), attempt);
            assert!(matches!(
                authority.failures.lock().unwrap().last(),
                Some(AuthError::Failed(message)) if message.contains("does not match")
            ));
            let events: Vec<_> = eventloop.state.events.drain(..).collect();
            assert!(matches!(
                events.as_slice(),
                [
                    Event::Auth(AuthEvent::Started {
                        kind: rumqttc::AuthExchangeKind::InitialConnect,
                        ..
                    }),
                    Event::Auth(AuthEvent::Failed {
                        kind: rumqttc::AuthExchangeKind::InitialConnect,
                        reason: rumqttc::AuthFailureReason::AuthenticationFailed(_),
                        ..
                    }),
                ]
            ));
            // A reset exchange cannot produce another notification during later cleanup.
            eventloop
                .state
                .abort_authentication(AuthError::Failed("cleanup".into()));
            assert_eq!(authority.failures.lock().unwrap().len(), attempt);
            assert!(eventloop.state.events.is_empty());
        }
    }
}

#[tokio::test]
async fn final_authentication_verification_uses_the_original_connection_deadline() {
    #[derive(Debug, Default)]
    struct Authority {
        success: AtomicUsize,
        failed: AtomicUsize,
        cancelled: Arc<AtomicUsize>,
    }
    struct Cancelled(Arc<AtomicUsize>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            match challenge {
                AsyncAuthChallenge::Start => Box::pin(async {
                    // Consume most of the connection budget before receiving CONNACK.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok(AuthAction::Complete)
                }),
                AsyncAuthChallenge::Success { .. } => {
                    self.success.fetch_add(1, Ordering::Relaxed);
                    let cancelled = Cancelled(self.cancelled.clone());
                    Box::pin(async move {
                        let _cancelled = cancelled;
                        std::future::pending().await
                    })
                }
                AsyncAuthChallenge::Continue { .. } => panic!("unexpected continuation"),
            }
        }
        fn failure(&self, _: AsyncAuthContext, _: AuthError) {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    let (client, mut peer) = tokio::io::duplex(1024);
    let client = Arc::new(std::sync::Mutex::new(Some(client)));
    let authority = Arc::new(Authority::default());
    let mut options = MqttOptions::new("auth", "localhost");
    options
        .set_connect_timeout(Duration::from_millis(200))
        .set_authentication_method(Some("test".into()))
        .set_async_authenticator(authority.clone())
        .set_socket_connector(move |_, _| {
            let client = client.lock().unwrap().take().unwrap();
            async move { Ok(client) }
        });
    let mut eventloop = EventLoop::new(options, 1);
    let broker = async {
        assert_eq!(peer.read_u8().await.unwrap(), 0x10);
        let mut remaining = 0usize;
        for shift in [0, 7, 14, 21] {
            let byte = peer.read_u8().await.unwrap();
            remaining |= usize::from(byte & 127) << shift;
            if byte < 128 {
                break;
            }
        }
        peer.read_exact(&mut vec![0; remaining]).await.unwrap();
        peer.write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .await
            .unwrap();
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
    };
    // Restarting the 200 ms budget for Success would exceed this bound too.
    let (result, ()) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(300), eventloop.poll()),
        broker,
    );
    assert!(matches!(
        result.expect("final verification escaped the connection deadline"),
        Err(ConnectionError::Timeout(_))
    ));
    assert_eq!(authority.success.load(Ordering::Relaxed), 1);
    assert_eq!(authority.failed.load(Ordering::Relaxed), 1);
    assert_eq!(authority.cancelled.load(Ordering::Relaxed), 1);
    assert!(
        !eventloop
            .state
            .events
            .iter()
            .any(|event| matches!(event, Event::Auth(AuthEvent::Succeeded { .. })))
    );
}

#[tokio::test]
async fn late_ready_final_authentication_cannot_commit_connection_success() {
    #[derive(Debug, Default)]
    struct Authority {
        slow_construction: bool,
        success: AtomicUsize,
        failed: AtomicUsize,
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            let is_success = matches!(challenge, AsyncAuthChallenge::Success { .. });
            if is_success {
                self.success.fetch_add(1, Ordering::Relaxed);
                if self.slow_construction {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            let slow_poll = is_success && !self.slow_construction;
            Box::pin(async move {
                if slow_poll {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Ok(AuthAction::Complete)
            })
        }
        fn failure(&self, _: AsyncAuthContext, _: AuthError) {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Both constructing and polling a future can consume the remaining budget
    // without yielding, so the timeout must also reject an immediately ready result.
    for slow_construction in [false, true] {
        let (client, mut peer) = tokio::io::duplex(1024);
        let client = Arc::new(std::sync::Mutex::new(Some(client)));
        let authority = Arc::new(Authority {
            slow_construction,
            ..Default::default()
        });
        let mut options = MqttOptions::new("auth", "localhost");
        options
            .set_connect_timeout(Duration::from_millis(200))
            .set_authentication_method(Some("test".into()))
            .set_async_authenticator(authority.clone())
            .set_socket_connector(move |_, _| {
                let client = client.lock().unwrap().take().unwrap();
                async move { Ok(client) }
            });
        let mut eventloop = EventLoop::new(options, 1);
        let broker = async {
            assert_eq!(peer.read_u8().await.unwrap(), 0x10);
            let remaining = peer.read_u8().await.unwrap();
            assert!(remaining < 128);
            peer.read_exact(&mut vec![0; usize::from(remaining)])
                .await
                .unwrap();
            peer.write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(2), eventloop.poll()),
            broker,
        );
        assert!(matches!(result.unwrap(), Err(ConnectionError::Timeout(_))));
        assert!(!eventloop.diagnostics().connected);
        assert_eq!(authority.success.load(Ordering::Relaxed), 1);
        assert_eq!(authority.failed.load(Ordering::Relaxed), 1);
        assert!(!eventloop.state.events.iter().any(|event| matches!(
            event,
            Event::Auth(AuthEvent::Succeeded { .. })
                | Event::Incoming(rumqttc::Incoming::ConnAck(_))
        )));
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn cancelling_reauthentication_poll_closes_connection_and_resolves_notice() {
    #[derive(Clone, Copy, Debug)]
    enum Stage {
        Start,
        Continue,
        Success,
    }
    #[derive(Debug)]
    struct Authority {
        stage: Stage,
        entered: Arc<tokio::sync::Notify>,
        dropped: Arc<AtomicUsize>,
        failed: AtomicUsize,
    }
    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl AsyncAuthenticator for Authority {
        fn respond(&self, context: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            let deferred = context.kind == rumqttc::AuthExchangeKind::Reauthentication
                && matches!(
                    (self.stage, challenge),
                    (Stage::Start, AsyncAuthChallenge::Start)
                        | (Stage::Continue, AsyncAuthChallenge::Continue { .. })
                        | (Stage::Success, AsyncAuthChallenge::Success { .. })
                );
            if deferred {
                let entered = self.entered.clone();
                let dropped = Dropped(self.dropped.clone());
                Box::pin(async move {
                    let _dropped = dropped;
                    entered.notify_one();
                    std::future::pending().await
                })
            } else {
                Box::pin(async { Ok(AuthAction::Complete) })
            }
        }
        fn failure(&self, context: AsyncAuthContext, _: AuthError) {
            assert_eq!(context.kind, rumqttc::AuthExchangeKind::Reauthentication);
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }
    async fn read_frame(peer: &mut tokio::io::DuplexStream) -> u8 {
        let packet_type = peer.read_u8().await.unwrap();
        let remaining = peer.read_u8().await.unwrap();
        assert!(remaining < 128);
        peer.read_exact(&mut vec![0; usize::from(remaining)])
            .await
            .unwrap();
        packet_type
    }

    for stage in [Stage::Start, Stage::Continue, Stage::Success] {
        let (socket, mut peer) = tokio::io::duplex(1024);
        let socket = Arc::new(std::sync::Mutex::new(Some(socket)));
        let entered = Arc::new(tokio::sync::Notify::new());
        let authority = Arc::new(Authority {
            stage,
            entered: entered.clone(),
            dropped: Arc::new(AtomicUsize::new(0)),
            failed: AtomicUsize::new(0),
        });
        let mut options = MqttOptions::new("auth", "localhost");
        options
            .set_keep_alive(0)
            .set_authentication_method(Some("test".into()))
            .set_async_authenticator(authority.clone())
            .set_socket_connector(move |_, _| {
                let socket = socket.lock().unwrap().take().unwrap();
                async move { Ok(socket) }
            });
        let (client, mut eventloop) = rumqttc::AsyncClient::builder(options).capacity(4).build();
        peer.write_all(b"\x20\x0a\x00\x00\x07\x15\x00\x04test")
            .await
            .unwrap();
        eventloop.poll().await.unwrap();
        assert!(eventloop.diagnostics().connected);
        assert_eq!(read_frame(&mut peer).await, 0x10);
        eventloop.state.events.clear();
        let notice = client.reauth_tracked(None).await.unwrap();
        let broker = async {
            if !matches!(stage, Stage::Start) {
                assert_eq!(read_frame(&mut peer).await, 0xf0);
                let mut packet = bytes::BytesMut::new();
                rumqttc::Auth::new(
                    if matches!(stage, Stage::Continue) {
                        rumqttc::AuthReasonCode::Continue
                    } else {
                        rumqttc::AuthReasonCode::Success
                    },
                    Some(rumqttc::AuthProperties {
                        method: Some("test".into()),
                        data: Some(bytes::Bytes::from_static(b"broker challenge")),
                        ..Default::default()
                    }),
                )
                .write(&mut packet)
                .unwrap();
                peer.write_all(&packet).await.unwrap();
            }
        };
        let cancelled = async {
            tokio::select! {
                biased;
                () = entered.notified() => {},
                () = async { loop { eventloop.poll().await.unwrap(); } } => unreachable!(),
            }
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(broker, cancelled);
        })
        .await
        .unwrap();
        assert!(!eventloop.diagnostics().connected, "stage={stage:?}");
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), notice.wait_async())
                .await
                .unwrap(),
            Err(rumqttc::AuthNoticeError::ConnectionClosed)
        );
        assert_eq!(authority.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(authority.failed.load(Ordering::Relaxed), 1);
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
        let mut failures = 0;
        let mut challenges = 0;
        let mut continuations = 0;
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(rumqttc::Incoming::Auth(auth))) => {
                    assert_eq!(
                        auth.properties.unwrap().data.as_deref(),
                        Some(b"broker challenge".as_slice())
                    );
                    challenges += 1;
                }
                Ok(Event::Auth(AuthEvent::Continue { .. })) => {
                    assert_eq!(challenges, 1);
                    continuations += 1;
                }
                Ok(Event::Auth(AuthEvent::Failed { reason, .. })) => {
                    assert_eq!(reason, rumqttc::AuthFailureReason::ConnectionClosed);
                    failures += 1;
                }
                Ok(Event::Auth(AuthEvent::Succeeded { .. })) => {
                    panic!("cancelled exchange succeeded")
                }
                Ok(_) => {}
                Err(ConnectionError::MqttState(rumqttc::StateError::ConnectionAborted)) => break,
                Err(error) => panic!("unexpected cancellation error: {error}"),
            }
        }
        assert_eq!(failures, 1);
        assert_eq!(challenges, usize::from(!matches!(stage, Stage::Start)));
        assert_eq!(continuations, usize::from(matches!(stage, Stage::Continue)));
        assert_eq!(authority.failed.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn rejected_connack_preserves_reason_in_authentication_failure_notification() {
    #[derive(Debug, Default)]
    struct Authority(std::sync::Mutex<Vec<AuthError>>);
    impl AsyncAuthenticator for Authority {
        fn respond(&self, _: AsyncAuthContext, challenge: AsyncAuthChallenge) -> AuthFuture {
            assert!(matches!(challenge, AsyncAuthChallenge::Start));
            Box::pin(async { Ok(AuthAction::Complete) })
        }
        fn failure(&self, context: AsyncAuthContext, error: AuthError) {
            assert_eq!(context.kind, rumqttc::AuthExchangeKind::InitialConnect);
            self.0.lock().unwrap().push(error);
        }
    }
    let (socket, mut peer) = tokio::io::duplex(1024);
    let socket = Arc::new(std::sync::Mutex::new(Some(socket)));
    let authority = Arc::new(Authority::default());
    let mut options = MqttOptions::new("auth", "localhost");
    options
        .set_authentication_method(Some("test".into()))
        .set_async_authenticator(authority.clone())
        .set_socket_connector(move |_, _| {
            let socket = socket.lock().unwrap().take().unwrap();
            async move { Ok(socket) }
        });
    let mut eventloop = EventLoop::new(options, 1);
    peer.write_all(b"\x20\x03\x00\x87\x00").await.unwrap();
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::ConnectionRefused(
            rumqttc::ConnectReturnCode::NotAuthorized
        ))
    ));
    assert_eq!(
        *authority.0.lock().unwrap(),
        [AuthError::BrokerRejected(
            rumqttc::ConnectReturnCode::NotAuthorized
        )]
    );
    assert!(eventloop.state.events.iter().any(|event| matches!(
        event,
        Event::Auth(AuthEvent::Failed {
            reason: rumqttc::AuthFailureReason::BrokerRejected(
                rumqttc::ConnectReturnCode::NotAuthorized
            ),
            ..
        })
    )));
    assert!(!eventloop.diagnostics().connected);
}
