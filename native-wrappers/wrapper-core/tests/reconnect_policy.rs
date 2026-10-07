mod support;
use support::*;

use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::*;

struct FailingConnector {
    calls: Arc<AtomicUsize>,
    gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl TransportConnector for FailingConnector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().take();
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            Err(TransportFailure::Connect)
        })
    }
}

fn classified(config: &mut ClientConfig, budget: RetryBudget, delay: Duration) {
    config.common.reconnect = ReconnectPolicy::Classified(ReconnectConfig {
        initial_delay: delay,
        maximum_delay: delay,
        jitter: ReconnectJitter::None,
        budget,
        ..ReconnectConfig::default()
    });
}

#[test]
fn exhaustion_resolves_pending_operations_and_first_connection_without_replacing_wait_timeout() {
    for mqtt5 in [false, true] {
        for limit in [0, 2] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (release, gate) = tokio::sync::oneshot::channel();
            let mut config = config(mqtt5, 1883);
            classified(&mut config, RetryBudget::Limited(limit), Duration::ZERO);
            config.common.connector = Some(TransportConnectorConfig {
                connector: Arc::new(FailingConnector {
                    calls: calls.clone(),
                    gate: Mutex::new(Some(gate)),
                }),
                mode: TransportMode::Base,
            });
            let mut client = start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let operation = client
                .handle()
                .try_admit(Command::Subscribe(SubscribeCommand {
                    filters: vec![Subscription {
                        filter: "pending".into(),
                        qos: QoS::AtLeastOnce,
                        protocol: SubscriptionProtocolOptions::VersionNeutral,
                    }],
                    protocol: SubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            assert_eq!(
                operation
                    .completion
                    .wait_timeout(Duration::from_millis(1))
                    .unwrap_err()
                    .kind(),
                ErrorKind::Timeout
            );
            release.send(()).unwrap();
            let mut failures = 0;
            let terminal = loop {
                match events.recv_timeout(DEADLINE).unwrap().unwrap() {
                    WrapperEvent::Disconnected { error, .. } => {
                        assert!(error.retryable());
                        failures += 1;
                    }
                    WrapperEvent::DriverTerminated(error) => break error,
                    event => panic!("unexpected {event:?}"),
                }
            };
            assert_eq!(failures, limit + 1);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                usize::try_from(limit + 1).unwrap()
            );
            assert_eq!(terminal.code(), ErrorCode::ReconnectExhausted);
            assert_eq!(
                terminal
                    .reconnect_exhaustion()
                    .unwrap()
                    .last_failure
                    .as_ref()
                    .unwrap()
                    .transport_failure(),
                Some(TransportFailure::Connect)
            );
            let failure = operation.completion.wait_timeout(DEADLINE).unwrap_err();
            assert_eq!(failure.code(), ErrorCode::ReconnectExhausted);
            assert_eq!(failure.delivery_status(), DeliveryStatus::Ambiguous);
            assert_eq!(
                operation
                    .completion
                    .wait_timeout(DEADLINE)
                    .unwrap_err()
                    .code(),
                ErrorCode::ReconnectExhausted
            );
            assert_eq!(
                client
                    .handle()
                    .connection()
                    .try_wait()
                    .unwrap()
                    .unwrap_err()
                    .code(),
                ErrorCode::ReconnectExhausted
            );
            let snapshot = client.handle().reconnect_diagnostics();
            assert_eq!(snapshot.stop_reason, ReconnectStopReason::Exhausted);
            assert_eq!(snapshot.cycles_started, limit + 1);
            assert_eq!(snapshot.retries_since_reset, limit);
        }
    }
}

#[test]
fn backoff_services_diagnostics_and_immediate_or_graceful_close() {
    for mqtt5 in [false, true] {
        for graceful in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut config = config(mqtt5, 1883);
            classified(&mut config, RetryBudget::Unlimited, Duration::from_secs(30));
            config.common.connector = Some(TransportConnectorConfig {
                connector: Arc::new(FailingConnector {
                    calls: calls.clone(),
                    gate: Mutex::new(None),
                }),
                mode: TransportMode::Base,
            });
            let mut client = start(config).unwrap();
            let mut events = client.take_events().unwrap();
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Disconnected { .. })
            });
            let observation = client.handle().try_admit(Command::Diagnostics).unwrap();
            let Completion::Diagnostics(snapshot) =
                observation.completion.wait_timeout(DEADLINE).unwrap()
            else {
                panic!("expected diagnostics");
            };
            let retry = snapshot.reconnect.unwrap();
            assert_eq!(retry.phase, ReconnectPhase::Waiting);
            assert!(retry.remaining_delay.unwrap() > Duration::from_secs(20));
            let began = Instant::now();
            if graceful {
                assert_eq!(
                    client
                        .closer()
                        .close(DEADLINE)
                        .unwrap_err()
                        .transport_failure(),
                    Some(TransportFailure::Connect)
                );
            } else {
                client.closer().close_now(DEADLINE).unwrap();
            }
            assert!(began.elapsed() < Duration::from_secs(2));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}

const fn persistent(config: &mut ClientConfig) {
    match &mut config.protocol {
        ProtocolConfig::V4(v4) => v4.clean_session = false,
        ProtocolConfig::V5(v5) => {
            v5.clean_start = false;
            v5.connect_properties.session_expiry_interval = Some(60);
        }
    }
}

#[test]
fn broker_recovery_replays_native_pending_publish_and_retains_completed_outcomes() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut first = accept(&listener);
            connect(&mut first, mqtt5);
            let original = publish_id(&mut first, mqtt5);
            drop(first);
            let mut second = accept(&listener);
            assert_eq!(frame(&mut second)[0], 0x10);
            second
                .write_all(if mqtt5 {
                    &[0x20, 3, 1, 0, 0]
                } else {
                    &[0x20, 2, 1, 0]
                })
                .unwrap();
            let replayed = publish_id(&mut second, mqtt5);
            assert_eq!(original, replayed);
            second
                .write_all(&[
                    0x40,
                    2,
                    replayed.to_be_bytes()[0],
                    replayed.to_be_bytes()[1],
                ])
                .unwrap();
            assert_eq!(frame(&mut second)[0] >> 4, 14);
        });
        let mut config = config(mqtt5, port);
        persistent(&mut config);
        classified(
            &mut config,
            RetryBudget::Limited(2),
            Duration::from_millis(30),
        );
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let operation = publish(&client, b"replay");
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert!(matches!(
            operation.completion.wait_timeout(DEADLINE).unwrap(),
            Completion::Publish(_)
        ));
        assert_eq!(client.handle().reconnect_diagnostics().cycles_started, 2);
        client.closer().close(DEADLINE).unwrap();
        assert!(matches!(
            operation.completion.wait_timeout(DEADLINE).unwrap(),
            Completion::Publish(_)
        ));
        broker.join().unwrap();
    }
}

#[test]
fn authentication_refusal_is_terminal_under_classified_policy() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            assert_eq!(frame(&mut socket)[0], 0x10);
            socket
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0x87, 0]
                } else {
                    &[0x20, 2, 0, 5]
                })
                .unwrap();
        });
        let mut config = config(mqtt5, port);
        classified(&mut config, RetryBudget::Unlimited, Duration::ZERO);
        let mut client = start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let error = loop {
            if let WrapperEvent::DriverTerminated(error) =
                events.recv_timeout(DEADLINE).unwrap().unwrap()
            {
                break error;
            }
        };
        assert!(!error.retryable());
        assert_eq!(error.kind(), ErrorKind::Authentication);
        assert_eq!(client.handle().reconnect_diagnostics().cycles_started, 1);
        assert_eq!(
            client.handle().reconnect_diagnostics().stop_reason,
            ReconnectStopReason::TerminalFailure
        );
        broker.join().unwrap();
    }
}

#[test]
fn native_srv_fallback_and_buffered_redirect_events_share_one_connection_cycle() {
    struct Candidates(u16);
    impl SrvResolver for Candidates {
        fn resolve(&self, _: String) -> SrvFuture {
            let port = self.0;
            Box::pin(async move {
                Ok(vec![
                    // Distinct priorities make fallback deterministic. The custom
                    // connector rejects the first port without relying on a host port.
                    SrvRecord {
                        priority: 0,
                        weight: 0,
                        port: 1,
                        target: "localhost.".into(),
                    },
                    SrvRecord {
                        priority: 1,
                        weight: 0,
                        port,
                        target: "localhost.".into(),
                    },
                ])
            })
        }
    }
    #[path = "support/custom_transport.rs"]
    mod custom;
    struct Connector {
        tcp: custom::TcpConnector,
        calls: Arc<AtomicUsize>,
    }
    impl TransportConnector for Connector {
        fn connect(&self, request: TransportRequest) -> TransportFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if request.target.ends_with(":1") {
                return Box::pin(async { Err(TransportFailure::Connect) });
            }
            self.tcp.connect(request)
        }
    }
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = target.local_addr().unwrap().port();
    let mut config = config(true, origin.local_addr().unwrap().port());
    classified(
        &mut config,
        RetryBudget::Limited(0),
        Duration::from_secs(30),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    config.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(Connector {
            tcp: custom::TcpConnector {
                write_chunk: 3,
                requests: Arc::new(Mutex::new(Vec::new())),
                target_override: None,
            },
            calls: calls.clone(),
        }),
        mode: TransportMode::Base,
    });
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 2,
        transport: TransportConfig::Tcp,
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Candidates(port))));
    let broker = std::thread::spawn(move || {
        let mut socket = accept(&origin);
        frame(&mut socket);
        let reference = b"_mqtt._tcp.service.invalid";
        let mut properties = vec![0x1c, 0, u8::try_from(reference.len()).unwrap()];
        properties.extend_from_slice(reference);
        let mut body = vec![0, 0x9c, u8::try_from(properties.len()).unwrap()];
        body.extend(properties);
        socket
            .write_all(&[0x20, u8::try_from(body.len()).unwrap()])
            .unwrap();
        socket.write_all(&body).unwrap();
        let mut target = accept(&target);
        // Redirect isolation requests a fresh client identity.
        assert_eq!(frame(&mut target)[0], 0x10);
        target
            .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
            .unwrap();
        assert_eq!(frame(&mut target)[0], 0xe0);
    });
    let mut client = start(config).unwrap();
    let mut events = client.take_events().unwrap();
    until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    let snapshot = client.handle().reconnect_diagnostics();
    assert_eq!(snapshot.cycles_started, 1);
    assert_eq!(snapshot.retries_since_reset, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    client.closer().close(DEADLINE).unwrap();
    broker.join().unwrap();
}

#[test]
fn established_redirect_obeys_the_next_cycle_budget_and_backoff() {
    for limit in [0, 1] {
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let reference = format!("127.0.0.1:{}", target.local_addr().unwrap().port());
        let mut config = config(true, origin.local_addr().unwrap().port());
        classified(
            &mut config,
            RetryBudget::Limited(limit),
            Duration::from_millis(100),
        );
        let ProtocolConfig::V5(v5) = &mut config.protocol else {
            unreachable!()
        };
        v5.redirect_policy = RedirectPolicy::Follow {
            max_attempts: 2,
            transport: TransportConfig::Tcp,
        };
        let (sent, receive) = std::sync::mpsc::channel();
        let broker = std::thread::spawn(move || {
            let mut socket = accept(&origin);
            connect(&mut socket, true);
            let mut properties = vec![0x1c, 0, u8::try_from(reference.len()).unwrap()];
            properties.extend_from_slice(reference.as_bytes());
            let mut body = vec![0x9c, u8::try_from(properties.len()).unwrap()];
            body.extend(properties);
            let began = Instant::now();
            socket
                .write_all(&[0xe0, u8::try_from(body.len()).unwrap()])
                .unwrap();
            socket.write_all(&body).unwrap();
            if limit > 0 {
                sent.send(began).unwrap();
            }
        });
        let target_broker = (limit > 0).then(|| {
            let target = target.try_clone().unwrap();
            std::thread::spawn(move || {
                let mut socket = accept(&target);
                assert!(
                    receive.recv_timeout(DEADLINE).unwrap().elapsed() >= Duration::from_millis(80)
                );
                assert_eq!(frame(&mut socket)[0], 0x10);
                socket
                    .write_all(b"\x20\x0c\x00\x00\x09\x12\x00\x06target")
                    .unwrap();
                assert_eq!(frame(&mut socket)[0], 0xe0);
            })
        });
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Redirect(_))
        });
        if limit == 0 {
            let event = until(&mut events, |event| {
                matches!(event, WrapperEvent::DriverTerminated(_))
            });
            let WrapperEvent::DriverTerminated(error) = event else {
                unreachable!()
            };
            assert_eq!(error.code(), ErrorCode::ReconnectExhausted);
            assert_eq!(client.handle().reconnect_diagnostics().cycles_started, 1);
            target.set_nonblocking(true).unwrap();
            assert_eq!(
                target.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        } else {
            until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            let snapshot = client.handle().reconnect_diagnostics();
            assert_eq!(snapshot.cycles_started, 2);
            assert_eq!(snapshot.retries_since_reset, 1);
            client.closer().close(DEADLINE).unwrap();
            target_broker.unwrap().join().unwrap();
        }
        broker.join().unwrap();
    }
}

#[test]
fn one_shared_worker_keeps_other_clients_progressing_during_backoff_and_idle_reset() {
    let context = ExecutionContext::new(ExecutionOptions {
        worker_threads: 1,
        ..ExecutionOptions::default()
    })
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut outage = config(false, 1883);
    classified(&mut outage, RetryBudget::Unlimited, Duration::from_secs(30));
    outage.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(FailingConnector {
            calls,
            gate: Mutex::new(None),
        }),
        mode: TransportMode::Base,
    });
    let mut delayed = NativeClient::start_in(outage, &context).unwrap();
    let mut delayed_events = delayed.take_events().unwrap();
    until(&mut delayed_events, |event| {
        matches!(event, WrapperEvent::Disconnected { .. })
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = std::thread::spawn(move || {
        let mut socket = accept(&listener);
        connect(&mut socket, true);
        let id = publish_id(&mut socket, true);
        socket
            .write_all(&[0x40, 2, id.to_be_bytes()[0], id.to_be_bytes()[1]])
            .unwrap();
        assert_eq!(frame(&mut socket)[0] >> 4, 14);
    });
    let mut healthy = config(true, port);
    classified(&mut healthy, RetryBudget::Unlimited, Duration::ZERO);
    let ReconnectPolicy::Classified(policy) = &mut healthy.common.reconnect else {
        unreachable!()
    };
    policy.stability_interval = Duration::from_millis(20);
    let mut healthy = NativeClient::start_in(healthy, &context).unwrap();
    let _events = connected(&mut healthy);
    let deadline = Instant::now() + DEADLINE;
    while healthy.handle().reconnect_diagnostics().reset_count == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    publish(&healthy, b"progress")
        .completion
        .wait_timeout(DEADLINE)
        .unwrap();
    healthy.closer().close(DEADLINE).unwrap();
    delayed.closer().close_now(DEADLINE).unwrap();
    broker.join().unwrap();
    context.request_shutdown();
    context.join(DEADLINE).unwrap();
}

#[cfg(feature = "ordered-shutdown")]
#[test]
fn ordered_deadline_interrupts_backoff_and_drives_native_terminal_cleanup() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            connect(&mut socket, mqtt5);
            publish_id(&mut socket, mqtt5);
        });
        let mut config = config(mqtt5, port);
        persistent(&mut config);
        classified(&mut config, RetryBudget::Unlimited, Duration::from_secs(30));
        let mut client = start(config).unwrap();
        let mut events = connected(&mut client);
        let operation = publish(&client, b"pending fence");
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let fence = client
            .handle()
            .try_admit(Command::OrderedDisconnect {
                timeout: Some(Duration::from_millis(30)),
            })
            .unwrap();
        let error = fence.completion.wait_timeout(DEADLINE).unwrap_err();
        assert_eq!(
            error.ordered_disconnect_failure(),
            Some(OrderedDisconnectFailure::Timeout)
        );
        let deadline = Instant::now() + DEADLINE;
        while client.handle().state() == LifecycleState::Closing {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(operation.completion.wait_timeout(DEADLINE).is_err());
        assert_eq!(client.handle().reconnect_diagnostics().cycles_started, 1);
        broker.join().unwrap();
    }
}
