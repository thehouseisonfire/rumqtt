use std::time::Duration;

use rumqttc_wrapper_core::*;

#[cfg(feature = "ordered-shutdown")]
mod support;

#[cfg(feature = "ordered-shutdown")]
mod enabled {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};

    use super::support;
    use support::{Broker, DEADLINE, accept, config, connect, connected, frame, puback, terminal};

    fn publish(qos: QoS, payload: &[u8]) -> Command {
        Command::Publish(PublishCommand {
            topic: "a".into(),
            payload: payload.to_vec().into(),
            qos,
            retain: false,
            protocol: PublishProtocolOptions::VersionNeutral,
        })
    }

    const fn persistent(config: &mut ClientConfig) {
        match &mut config.protocol {
            ProtocolConfig::V4(config) => config.clean_session = false,
            ProtocolConfig::V5(config) => {
                config.clean_start = false;
                config.connect_properties.session_expiry_interval = Some(60);
            }
        }
    }

    const fn inflight_one(config: &mut ClientConfig) {
        config.common.max_request_batch = 1;
        match &mut config.protocol {
            ProtocolConfig::V4(config) => config.inflight_limit = 1,
            ProtocolConfig::V5(config) => config.outgoing_inflight_upper_limit = Some(1),
        }
    }

    fn acknowledge(socket: &mut std::net::TcpStream, packet: &[u8], qos: QoS) {
        if qos == QoS::AtMostOnce {
            return;
        }
        let id = u16::from_be_bytes([packet[5], packet[6]]);
        if qos == QoS::AtLeastOnce {
            puback(socket, id);
        } else {
            let [high, low] = id.to_be_bytes();
            socket.write_all(&[0x50, 2, high, low]).unwrap();
            let rel = frame(socket);
            assert_eq!(rel[0] >> 4, 6);
            assert_eq!(&rel[2..4], &[high, low]);
            socket.write_all(&[0x70, 2, high, low]).unwrap();
        }
    }

    #[test]
    fn ordered_fence_completes_preceding_qos_work_and_preserves_ordinary_close() {
        for mqtt5 in [false, true] {
            for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
                for ordered in [false, true] {
                    if !ordered && qos == QoS::AtMostOnce {
                        continue;
                    }
                    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let (first_tx, first_rx) = mpsc::channel();
                    let (release_tx, release_rx) = mpsc::channel();
                    let broker = Broker::spawn(move || {
                        let mut socket = accept(&listener);
                        connect(&mut socket, mqtt5);
                        let first = frame(&mut socket);
                        assert_eq!(first[0] >> 4, 3);
                        first_tx.send(()).unwrap();
                        release_rx.recv_timeout(DEADLINE).unwrap();
                        acknowledge(&mut socket, &first, qos);
                        if ordered {
                            let second = frame(&mut socket);
                            assert_eq!(second[0] >> 4, 3, "DISCONNECT overtook queued B");
                            acknowledge(&mut socket, &second, qos);
                        }
                        assert_eq!(frame(&mut socket)[0] >> 4, 14);
                    });
                    let mut cfg = config(mqtt5, port);
                    inflight_one(&mut cfg);
                    let mut client = NativeClient::start(cfg).unwrap();
                    let _events = connected(&mut client);
                    let a = client.handle().try_admit(publish(qos, b"a")).unwrap();
                    first_rx.recv_timeout(DEADLINE).unwrap();
                    let b = client.handle().try_admit(publish(qos, b"b")).unwrap();
                    let close = client
                        .handle()
                        .try_admit(if ordered {
                            Command::OrderedDisconnect {
                                timeout: Some(DEADLINE),
                            }
                        } else {
                            Command::GracefulDisconnect {
                                timeout: Some(DEADLINE),
                            }
                        })
                        .unwrap();
                    release_tx.send(()).unwrap();
                    assert_eq!(
                        terminal(&close).unwrap(),
                        if ordered {
                            Completion::OrderedShutdown
                        } else {
                            Completion::GracefulShutdown
                        }
                    );
                    assert!(terminal(&a).is_ok());
                    assert_eq!(terminal(&b).is_ok(), ordered);
                    client.join(DEADLINE).unwrap();
                    broker.join();
                }
            }
        }
    }

    #[test]
    fn collective_success_does_not_cover_pending_subscriptions_or_inbound_acknowledgements() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (seen_tx, seen_rx) = mpsc::channel();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                socket
                    .write_all(if mqtt5 {
                        &[0x32, 7, 0, 1, b'a', 0, 7, 0, b'x']
                    } else {
                        &[0x32, 6, 0, 1, b'a', 0, 7, b'x']
                    })
                    .unwrap();
                assert_eq!(frame(&mut socket)[0] >> 4, 8);
                // Leave SUBACK and the independent inbound PUBACK outstanding.
                seen_tx.send(()).unwrap();
                assert_eq!(frame(&mut socket)[0] >> 4, 3);
                assert_eq!(frame(&mut socket)[0] >> 4, 14);
            });
            let mut cfg = config(mqtt5, port);
            cfg.common.ack_mode = AckMode::Manual;
            let mut client = NativeClient::start(cfg).unwrap();
            let mut events = connected(&mut client);
            let inbound = support::until(&mut events, |event| {
                matches!(event, WrapperEvent::IncomingPublish(_))
            });
            let WrapperEvent::IncomingPublish(inbound) = inbound else {
                unreachable!()
            };
            let token = inbound.ack_token.unwrap();
            let subscription = client
                .handle()
                .try_admit(Command::Subscribe(SubscribeCommand {
                    filters: vec![Subscription {
                        filter: "a".into(),
                        qos: QoS::AtLeastOnce,
                        protocol: SubscriptionProtocolOptions::VersionNeutral,
                    }],
                    protocol: SubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            seen_rx.recv_timeout(DEADLINE).unwrap();
            let publication = client
                .handle()
                .try_admit(publish(QoS::AtMostOnce, b"covered"))
                .unwrap();
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect {
                    timeout: Some(DEADLINE),
                })
                .unwrap();
            assert_eq!(terminal(&close).unwrap(), Completion::OrderedShutdown);
            assert!(matches!(
                terminal(&publication).unwrap(),
                Completion::Publish(_)
            ));
            client.join(DEADLINE).unwrap();
            assert!(terminal(&subscription).is_err());
            assert!(
                client
                    .handle()
                    .try_admit(Command::Acknowledge(token))
                    .is_err()
            );
            broker.join();
        }
    }

    #[test]
    fn concurrent_producers_use_successful_admission_order_and_later_calls_close() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (first_tx, first_rx) = mpsc::channel();
            let (expected_tx, expected_rx) = mpsc::channel::<Vec<u8>>();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                let first = frame(&mut socket);
                first_tx.send(()).unwrap();
                let expected = expected_rx.recv_timeout(DEADLINE).unwrap();
                acknowledge(&mut socket, &first, QoS::AtLeastOnce);
                for expected in expected {
                    let packet = frame(&mut socket);
                    assert_eq!(packet[0] >> 4, 3);
                    assert_eq!(packet.last(), Some(&expected));
                    acknowledge(&mut socket, &packet, QoS::AtLeastOnce);
                }
                assert_eq!(frame(&mut socket)[0] >> 4, 14);
            });
            let mut cfg = config(mqtt5, port);
            inflight_one(&mut cfg);
            cfg.common.request_channel_capacity = 64;
            let mut client = NativeClient::start(cfg).unwrap();
            let _events = connected(&mut client);
            client
                .handle()
                .try_admit(publish(QoS::AtLeastOnce, b"first"))
                .unwrap();
            first_rx.recv_timeout(DEADLINE).unwrap();
            let mut admissions = std::thread::scope(|scope| {
                (0..16u8)
                    .map(|value| {
                        let handle = client.handle();
                        scope.spawn(move || {
                            (
                                handle
                                    .try_admit(publish(QoS::AtLeastOnce, &[value]))
                                    .unwrap(),
                                value,
                            )
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|worker| worker.join().unwrap())
                    .collect::<Vec<_>>()
            });
            admissions.sort_by_key(|(admission, _)| admission.operation_id.get());
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect {
                    timeout: Some(DEADLINE),
                })
                .unwrap();
            let error = client
                .handle()
                .try_admit(publish(QoS::AtLeastOnce, b"late"))
                .unwrap_err();
            assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
            assert_eq!(error.kind(), ErrorKind::Shutdown);
            assert!(
                client
                    .handle()
                    .try_admit(Command::OrderedDisconnect { timeout: None })
                    .is_err()
            );
            assert!(client.closer().close(DEADLINE).is_err());
            expected_tx
                .send(admissions.iter().map(|(_, value)| *value).collect())
                .unwrap();
            assert_eq!(terminal(&close).unwrap(), Completion::OrderedShutdown);
            assert_eq!(
                client.closer().close_after_queued(DEADLINE).unwrap(),
                Completion::OrderedShutdown
            );
            assert_eq!(
                client.closer().close_after_queued(DEADLINE).unwrap(),
                Completion::OrderedShutdown
            );
            for (admission, _) in admissions {
                assert!(terminal(&admission).is_ok());
            }
            broker.join();
        }
    }

    #[test]
    fn reconnect_keeps_replay_before_fence_and_session_loss_fails_honestly() {
        for mqtt5 in [false, true] {
            for resume in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let (first_tx, first_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                let broker = Broker::spawn(move || {
                    let mut socket = accept(&listener);
                    connect(&mut socket, mqtt5);
                    let first = frame(&mut socket);
                    first_tx.send(()).unwrap();
                    release_rx.recv_timeout(DEADLINE).unwrap();
                    drop(socket);
                    let mut socket = accept(&listener);
                    assert_eq!(frame(&mut socket)[0] >> 4, 1);
                    if mqtt5 {
                        socket
                            .write_all(&[0x20, 3, u8::from(resume), 0, 0])
                            .unwrap();
                    } else {
                        socket.write_all(&[0x20, 2, u8::from(resume), 0]).unwrap();
                    }
                    if resume {
                        let replay = frame(&mut socket);
                        assert_eq!(&replay[5..7], &first[5..7]);
                        assert_ne!(replay[0] & 8, 0);
                        acknowledge(&mut socket, &replay, QoS::AtLeastOnce);
                        let second = frame(&mut socket);
                        assert_eq!(second[0] >> 4, 3);
                        acknowledge(&mut socket, &second, QoS::AtLeastOnce);
                        assert_eq!(frame(&mut socket)[0] >> 4, 14);
                    } else {
                        let mut byte = [0];
                        assert_eq!(socket.read(&mut byte).unwrap(), 0);
                    }
                });
                let mut cfg = config(mqtt5, port);
                persistent(&mut cfg);
                inflight_one(&mut cfg);
                let mut client = NativeClient::start(cfg).unwrap();
                let _events = connected(&mut client);
                client
                    .handle()
                    .try_admit(publish(QoS::AtLeastOnce, b"a"))
                    .unwrap();
                first_rx.recv_timeout(DEADLINE).unwrap();
                client
                    .handle()
                    .try_admit(publish(QoS::AtLeastOnce, b"b"))
                    .unwrap();
                let close = client
                    .handle()
                    .try_admit(Command::OrderedDisconnect {
                        timeout: Some(DEADLINE),
                    })
                    .unwrap();
                release_tx.send(()).unwrap();
                let result = terminal(&close);
                if resume {
                    assert_eq!(result.unwrap(), Completion::OrderedShutdown);
                } else {
                    assert_eq!(
                        result.unwrap_err().ordered_disconnect_failure(),
                        Some(OrderedDisconnectFailure::SessionReset)
                    );
                }
                client.join(DEADLINE).unwrap();
                broker.join();
            }
        }
    }

    struct PendingConnector;
    impl TransportConnector for PendingConnector {
        fn connect(&self, _: TransportRequest) -> TransportFuture {
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn full_admission_and_cancelled_capacity_wait_install_no_fence() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                let mut byte = [0];
                while socket.read(&mut byte).unwrap() != 0 {}
            });
            let (entered, entries) = mpsc::channel();
            let store = Arc::new(GatedStore {
                armed: AtomicBool::new(false),
                released: Arc::new(AtomicBool::new(false)),
                calls: AtomicUsize::new(0),
                entered,
                ready: Arc::new(tokio::sync::Notify::new()),
                fail: false,
            });
            let mut cfg = config(mqtt5, port);
            persistent(&mut cfg);
            cfg.common.request_channel_capacity = 1;
            match &mut cfg.protocol {
                ProtocolConfig::V4(cfg) => {
                    cfg.session_store = Some(SessionStoreConfig::new(
                        store.clone(),
                        "ordered-backpressure",
                    ));
                }
                ProtocolConfig::V5(cfg) => {
                    cfg.session_store = Some(SessionStoreConfig::new(
                        store.clone(),
                        "ordered-backpressure",
                    ));
                }
            }
            let mut client = NativeClient::start(cfg).unwrap();
            let _events = connected(&mut client);
            let handle = client.handle();
            store.armed.store(true, Ordering::Release);
            handle
                .try_admit(publish(QoS::AtLeastOnce, b"first"))
                .unwrap();
            entries.recv_timeout(DEADLINE).unwrap();
            handle
                .try_admit(publish(QoS::AtLeastOnce, b"queued"))
                .unwrap();
            assert_eq!(
                handle
                    .try_admit(Command::OrderedDisconnect { timeout: None })
                    .unwrap_err()
                    .kind(),
                ErrorKind::Backpressure
            );
            assert_eq!(handle.state(), LifecycleState::Running);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(20),
                        handle.admit_async(Command::OrderedDisconnect {
                            timeout: Some(DEADLINE)
                        })
                    )
                    .await
                    .is_err()
                );
            });
            assert_eq!(handle.state(), LifecycleState::Running);
            assert_eq!(
                handle
                    .try_admit(Command::OrderedDisconnect {
                        timeout: Some(Duration::MAX)
                    })
                    .unwrap_err()
                    .delivery_status(),
                DeliveryStatus::NotAdmitted
            );
            store.released.store(true, Ordering::Release);
            store.ready.notify_waiters();
            client.closer().close_now(DEADLINE).unwrap();
            broker.join();
        }
    }

    #[test]
    fn established_connection_timeout_retains_admission_context_for_all_observers() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                let mut byte = [0];
                while socket.read(&mut byte).unwrap() != 0 {}
            });
            let mut client = NativeClient::start(config(mqtt5, port)).unwrap();
            let _events = connected(&mut client);
            let closer = client.closer();
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect {
                    timeout: Some(Duration::ZERO),
                })
                .unwrap();
            let expected = ErrorContext {
                protocol: Some(if mqtt5 {
                    ProtocolVersion::V5
                } else {
                    ProtocolVersion::V4
                }),
                phase: Some(ConnectionPhase::Established),
                generation: Some(1),
                operation_id: Some(close.operation_id),
            };
            let error = terminal(&close).unwrap_err();
            assert_eq!(
                error.ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::Timeout)
            );
            assert_eq!(error.context(), expected);
            assert_eq!(
                closer.close_after_queued(DEADLINE).unwrap_err().context(),
                expected
            );
            client.join(DEADLINE).unwrap();
            drop(client);
            assert_eq!(terminal(&close).unwrap_err().context(), expected);
            broker.join();
        }
    }

    #[test]
    fn zero_deadline_and_pending_connection_resolve_without_observer_cancellation() {
        for mqtt5 in [false, true] {
            for timeout in [Duration::ZERO, Duration::from_millis(30)] {
                let mut cfg = config(mqtt5, 1883);
                cfg.common.connector = Some(TransportConnectorConfig {
                    mode: TransportMode::Base,
                    connector: Arc::new(PendingConnector),
                });
                let client = NativeClient::start(cfg).unwrap();
                let close = client
                    .handle()
                    .try_admit(Command::OrderedDisconnect {
                        timeout: Some(timeout),
                    })
                    .unwrap();
                let error = terminal(&close).unwrap_err();
                assert_eq!(
                    error.ordered_disconnect_failure(),
                    Some(OrderedDisconnectFailure::Timeout)
                );
                assert_eq!(
                    close
                        .completion
                        .wait_timeout(Duration::ZERO)
                        .unwrap_err()
                        .ordered_disconnect_failure(),
                    Some(OrderedDisconnectFailure::Timeout)
                );
                client.join(DEADLINE).unwrap();
            }
        }
    }

    #[test]
    fn immediate_escalation_and_owner_destruction_preserve_superseded_result() {
        for mqtt5 in [false, true] {
            for destroy in [false, true] {
                let mut cfg = config(mqtt5, 1883);
                cfg.common.connector = Some(TransportConnectorConfig {
                    mode: TransportMode::Base,
                    connector: Arc::new(PendingConnector),
                });
                let client = NativeClient::start(cfg).unwrap();
                let closer = client.closer();
                let close = client
                    .handle()
                    .try_admit(Command::OrderedDisconnect { timeout: None })
                    .unwrap();
                assert!(matches!(
                    close.completion.wait_timeout_outcome(Duration::ZERO),
                    CompletionWaitOutcome::DeadlineElapsed
                ));
                if !destroy {
                    closer.close_now(DEADLINE).unwrap();
                }
                drop(client);
                assert_eq!(
                    terminal(&close).unwrap_err().ordered_disconnect_failure(),
                    Some(OrderedDisconnectFailure::SupersededByImmediate)
                );
                closer.close_now(DEADLINE).unwrap();
            }
        }
    }

    #[test]
    fn boundary_panic_resolves_retained_fence_from_native_receiver_termination() {
        for mqtt5 in [false, true] {
            let mut cfg = config(mqtt5, 1883);
            cfg.common.connector = Some(TransportConnectorConfig {
                mode: TransportMode::Base,
                connector: Arc::new(PendingConnector),
            });
            let mut client = NativeClient::start(cfg).unwrap();
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect { timeout: None })
                .unwrap();
            client.handle().terminate_for_internal_panic();
            client.join(DEADLINE).unwrap();
            let mut events = client.take_events().unwrap();
            let failure = loop {
                if let WrapperEvent::DriverTerminated(error) = events
                    .recv_timeout(DEADLINE)
                    .unwrap()
                    .expect("driver terminal event")
                {
                    break error;
                }
            };
            assert_eq!(failure.code(), ErrorCode::InternalPanic);
            drop(client);
            let error = terminal(&close).unwrap_err();
            assert_eq!(
                error.ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::ReceiverTerminated)
            );
            assert!(!error.retryable());
            assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        }
    }

    #[test]
    fn dropped_raw_observer_and_repeated_closers_preserve_original_deadline_and_payload() {
        for mqtt5 in [false, true] {
            let mut cfg = config(mqtt5, 1883);
            cfg.common.connector = Some(TransportConnectorConfig {
                mode: TransportMode::Base,
                connector: Arc::new(PendingConnector),
            });
            let client = NativeClient::start(cfg).unwrap();
            let protocol = if mqtt5 {
                DisconnectProtocolOptions::V5(V5DisconnectOptions {
                    reason_string: Some("original".into()),
                    ..Default::default()
                })
            } else {
                DisconnectProtocolOptions::VersionNeutral
            };
            let admission = client
                .handle()
                .try_admit(Command::OrderedDisconnectWithOptions {
                    timeout: Some(Duration::from_millis(40)),
                    protocol: protocol.clone(),
                })
                .unwrap();
            drop(admission);
            let closer = client.closer();
            let observer = closer
                .close_after_queued_with_options(Duration::ZERO, protocol.clone())
                .unwrap_err();
            assert_eq!(observer.kind(), ErrorKind::Timeout);
            assert_eq!(observer.ordered_disconnect_failure(), None);
            if mqtt5 {
                assert_eq!(
                    closer
                        .close_after_queued(Duration::ZERO)
                        .unwrap_err()
                        .kind(),
                    ErrorKind::Shutdown
                );
            }
            let started = std::time::Instant::now();
            let result = closer
                .close_after_queued_with_options(DEADLINE, protocol.clone())
                .unwrap_err();
            assert_eq!(
                result.ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::Timeout)
            );
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "later caller reset the deadline"
            );
            assert_eq!(
                closer
                    .close_after_queued_with_options(Duration::ZERO, protocol)
                    .unwrap_err()
                    .ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::Timeout)
            );
            client.join(DEADLINE).unwrap();
        }
    }

    #[test]
    fn earlier_negative_acknowledgement_fails_collective_notice() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = Broker::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, true);
            let packet = frame(&mut socket);
            socket
                .write_all(&[0x40, 4, packet[5], packet[6], 0x97, 0])
                .unwrap();
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).unwrap(), 0);
        });
        let mut client = NativeClient::start(config(true, port)).unwrap();
        let _events = connected(&mut client);
        let publication = client
            .handle()
            .try_admit(publish(QoS::AtLeastOnce, b"rejected"))
            .unwrap();
        assert_eq!(
            terminal(&publication).unwrap_err().broker_reason(),
            Some(0x97)
        );
        let close = client
            .handle()
            .try_admit(Command::OrderedDisconnect {
                timeout: Some(DEADLINE),
            })
            .unwrap();
        let error = terminal(&close).unwrap_err();
        assert_eq!(
            error.ordered_disconnect_failure(),
            Some(OrderedDisconnectFailure::Publish)
        );
        assert_eq!(error.broker_reason(), Some(0x97));
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert!(!error.retryable());
        client.join(DEADLINE).unwrap();
        broker.join();
    }

    struct GatedStore {
        armed: AtomicBool,
        released: Arc<AtomicBool>,
        calls: AtomicUsize,
        entered: mpsc::Sender<()>,
        ready: Arc<tokio::sync::Notify>,
        fail: bool,
    }
    impl GatedStore {
        fn write(&self) -> StoreFuture<()> {
            if !self.armed.load(Ordering::Acquire) {
                return Box::pin(async { Ok(()) });
            }
            self.calls.fetch_add(1, Ordering::AcqRel);
            self.entered.send(()).unwrap();
            let ready = Arc::clone(&self.ready);
            let released = Arc::clone(&self.released);
            let fail = self.fail;
            Box::pin(async move {
                loop {
                    let changed = ready.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if released.load(Ordering::Acquire) {
                        break;
                    }
                    changed.await;
                }
                if fail {
                    Err(StoreFailure::Save)
                } else {
                    Ok(())
                }
            })
        }
    }
    impl SessionStore for GatedStore {
        fn load(&self, _: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
            Box::pin(async { Ok(None) })
        }
        fn save(&self, _: SessionStoreKey, _: SessionCheckpoint) -> StoreFuture<()> {
            self.write()
        }
        fn clear(&self, _: SessionStoreKey) -> StoreFuture<()> {
            self.write()
        }
    }

    #[test]
    fn explicit_abort_interrupts_connected_pending_store_without_waiting_for_cleanup() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 0);
            });
            let (entered, entries) = mpsc::channel();
            let store = Arc::new(GatedStore {
                armed: AtomicBool::new(false),
                released: Arc::new(AtomicBool::new(false)),
                calls: AtomicUsize::new(0),
                entered,
                ready: Arc::new(tokio::sync::Notify::new()),
                fail: false,
            });
            let mut cfg = config(mqtt5, port);
            persistent(&mut cfg);
            let persistence = Some(SessionStoreConfig::new(store.clone(), "ordered-abort"));
            match &mut cfg.protocol {
                ProtocolConfig::V4(cfg) => cfg.session_store = persistence,
                ProtocolConfig::V5(cfg) => cfg.session_store = persistence,
            }
            let mut client = NativeClient::start(cfg).unwrap();
            let _events = connected(&mut client);
            store.armed.store(true, Ordering::Release);
            client
                .handle()
                .try_admit(publish(QoS::AtLeastOnce, b"pending"))
                .unwrap();
            entries.recv_timeout(DEADLINE).unwrap();
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect { timeout: None })
                .unwrap();
            client.closer().close_now(Duration::from_secs(1)).unwrap();
            assert_eq!(
                terminal(&close).unwrap_err().ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::SupersededByImmediate)
            );
            assert!(!store.released.load(Ordering::Acquire));
            broker.join();
        }
    }

    #[test]
    fn timeout_resolves_before_pending_terminal_store_cleanup_and_retains_its_result() {
        for mqtt5 in [false, true] {
            for (fail, abort) in [(false, false), (true, false), (false, true), (true, true)] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let broker = Broker::spawn(move || {
                    let mut socket = accept(&listener);
                    connect(&mut socket, mqtt5);
                    let mut byte = [0];
                    while socket.read(&mut byte).unwrap() != 0 {}
                });
                let (entered, entries) = mpsc::channel();
                let store = Arc::new(GatedStore {
                    armed: AtomicBool::new(false),
                    released: Arc::new(AtomicBool::new(false)),
                    calls: AtomicUsize::new(0),
                    entered,
                    ready: Arc::new(tokio::sync::Notify::new()),
                    fail,
                });
                let mut cfg = config(mqtt5, port);
                persistent(&mut cfg);
                let persistence = Some(SessionStoreConfig::new(store.clone(), "ordered-cleanup"));
                match &mut cfg.protocol {
                    ProtocolConfig::V4(cfg) => cfg.session_store = persistence,
                    ProtocolConfig::V5(cfg) => cfg.session_store = persistence,
                }
                let mut client = NativeClient::start(cfg).unwrap();
                let mut events = connected(&mut client);
                store.armed.store(true, Ordering::Release);
                client
                    .handle()
                    .try_admit(publish(QoS::AtLeastOnce, b"pending"))
                    .unwrap();
                entries.recv_timeout(DEADLINE).unwrap();
                let close = client
                    .handle()
                    .try_admit(Command::OrderedDisconnect {
                        timeout: Some(Duration::from_millis(30)),
                    })
                    .unwrap();
                assert_eq!(
                    terminal(&close).unwrap_err().ordered_disconnect_failure(),
                    Some(OrderedDisconnectFailure::Timeout)
                );
                entries.recv_timeout(DEADLINE).unwrap();
                assert!(store.calls.load(Ordering::Acquire) >= 2);
                assert!(client.join(Duration::ZERO).is_err());
                if abort {
                    // The native admission gate is already terminated at expiry; abort must
                    // still cancel retained cleanup rather than depend on another native send.
                    client.closer().close_now(Duration::from_secs(1)).unwrap();
                    assert_eq!(
                        terminal(&close).unwrap_err().ordered_disconnect_failure(),
                        Some(OrderedDisconnectFailure::Timeout)
                    );
                    assert!(!store.released.load(Ordering::Acquire));
                    broker.join();
                    continue;
                }
                store.released.store(true, Ordering::Release);
                store.ready.notify_waiters();
                client.join(DEADLINE).unwrap();
                assert_eq!(
                    terminal(&close).unwrap_err().ordered_disconnect_failure(),
                    Some(OrderedDisconnectFailure::Timeout)
                );
                let terminal_event = support::until(&mut events, |event| {
                    matches!(event, WrapperEvent::DriverTerminated(_))
                });
                if let WrapperEvent::DriverTerminated(error) = terminal_event {
                    assert_eq!(
                        error.kind(),
                        if fail {
                            ErrorKind::Persistence
                        } else {
                            ErrorKind::Timeout
                        }
                    );
                }
                broker.join();
            }
        }
    }

    #[test]
    fn a_full_application_event_queue_does_not_hide_the_native_deadline() {
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (ready_tx, ready_rx) = mpsc::channel();
            let broker = Broker::spawn(move || {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                socket
                    .write_all(if mqtt5 {
                        &[0x30, 5, 0, 1, b'a', 0, b'x']
                    } else {
                        &[0x30, 4, 0, 1, b'a', b'x']
                    })
                    .unwrap();
                ready_tx.send(()).unwrap();
                let mut byte = [0];
                while socket.read(&mut byte).unwrap() != 0 {}
            });
            let mut cfg = config(mqtt5, port);
            cfg.common.event_buffer_capacity = 1;
            cfg.common.event_delivery_timeout = DEADLINE;
            let client = NativeClient::start(cfg).unwrap();
            ready_rx.recv_timeout(DEADLINE).unwrap();
            // Leave incoming delivery blocked before admitting the ordered fence.
            std::thread::sleep(Duration::from_millis(20));
            client
                .handle()
                .admit(publish(QoS::AtLeastOnce, b"blocked"))
                .unwrap();
            let close = client
                .handle()
                .try_admit(Command::OrderedDisconnect {
                    timeout: Some(Duration::from_millis(30)),
                })
                .unwrap();
            assert_eq!(
                terminal(&close).unwrap_err().ordered_disconnect_failure(),
                Some(OrderedDisconnectFailure::Timeout)
            );
            client.join(Duration::from_secs(1)).unwrap();
            broker.join();
        }
    }
}

#[cfg(not(feature = "ordered-shutdown"))]
#[test]
fn disabled_ordered_commands_leave_the_client_running() {
    let client = NativeClient::start(ClientConfig::v4("disabled-ordered", "127.0.0.1", 1)).unwrap();
    let handle = client.handle();
    let error = handle
        .try_admit(Command::OrderedDisconnect {
            timeout: Some(Duration::ZERO),
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Configuration);
    assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    assert_eq!(handle.state(), LifecycleState::Running);
    client.closer().close_now(Duration::from_secs(5)).unwrap();
}
