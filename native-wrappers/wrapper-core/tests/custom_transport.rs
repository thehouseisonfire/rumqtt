#[path = "support/custom_transport.rs"]
mod custom;
mod support;
use bytes::Bytes;
use rumqttc_wrapper_core::*;
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::*;

#[test]
fn custom_streams_reconnect_with_fresh_attempts_and_keep_native_tracking() {
    for mqtt5 in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = std::thread::spawn(move || {
            for attempt in 0..2 {
                let mut socket = accept(&listener);
                connect(&mut socket, mqtt5);
                let id = publish_id(&mut socket, mqtt5);
                socket
                    .write_all(&[0x40, 2, (id >> 8) as u8, id as u8])
                    .unwrap();
                // Close the first connection; wait for DISCONNECT on the next.
                if attempt == 1 {
                    assert_eq!(frame(&mut socket)[0] >> 4, 14);
                }
            }
        });
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut config = config(mqtt5, 1883);
        config.common.broker = BrokerTarget::Tcp {
            host: "supplied.invalid".into(),
            port: 1883,
        };
        config.common.connector = Some(TransportConnectorConfig {
            mode: TransportMode::Base,
            connector: Arc::new(custom::TcpConnector {
                requests: requests.clone(),
                target_override: Some(format!("127.0.0.1:{port}")),
            }),
        });
        let started = Instant::now();
        let mut client = NativeClient::start(config).unwrap();
        let mut events = connected(&mut client);
        let first = publish(&client, b"first");
        first.completion.wait_timeout(DEADLINE).unwrap();
        until(&mut events, |e| {
            matches!(e, WrapperEvent::Disconnected { .. })
        });
        until(&mut events, |e| matches!(e, WrapperEvent::Connected { .. }));
        publish(&client, b"next")
            .completion
            .wait_timeout(DEADLINE)
            .unwrap();
        client.closer().close(DEADLINE).unwrap();
        broker.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.generation, index as u64 + 1);
            assert_eq!(request.target, "supplied.invalid:1883");
            assert_eq!(
                request.protocol,
                if mqtt5 {
                    ProtocolVersion::V5
                } else {
                    ProtocolVersion::V4
                }
            );
            assert!(request.deadline > started);
            assert!(request.deadline <= Instant::now() + Duration::from_secs(5));
        }
    }
}

struct Failure(TransportFailure);
impl TransportConnector for Failure {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let failure = self.0;
        Box::pin(async move { Err(failure) })
    }
}
#[test]
fn terminal_connector_failures_stop_the_driver_and_fail_pending_operations() {
    for mqtt5 in [false, true] {
        for failure in [
            TransportFailure::NetworkOptions,
            TransportFailure::Composition,
            TransportFailure::InvalidResult,
            TransportFailure::Panic,
            TransportFailure::ResourceLimit,
        ] {
            let (release, ready) = tokio::sync::oneshot::channel();
            let calls = Arc::new(AtomicUsize::new(0));
            let connector = Arc::new(GatedFailure {
                failure,
                ready: Mutex::new(Some(ready)),
                calls: calls.clone(),
            });
            let weak = Arc::downgrade(&connector);
            let mut config = config(mqtt5, 1883);
            config.common.connector = Some(TransportConnectorConfig {
                connector,
                mode: TransportMode::Base,
            });
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let subscribe = client
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
            let unsubscribe = client
                .handle()
                .try_admit(Command::Unsubscribe(UnsubscribeCommand {
                    filters: vec!["pending".into()],
                    protocol: UnsubscribeProtocolOptions::VersionNeutral,
                }))
                .unwrap();
            assert!(client.handle().connection().try_wait().is_none());
            release.send(()).unwrap();

            let event = events.recv_timeout(DEADLINE).unwrap().unwrap();
            let WrapperEvent::DriverTerminated(error) = event else {
                panic!("expected automatic driver termination, got {event:?}");
            };
            assert_eq!(error.transport_failure(), Some(failure));
            assert!(!error.retryable());
            assert_eq!(client.handle().state(), LifecycleState::Failed);
            for operation in [&subscribe, &unsubscribe] {
                let error = terminal(operation).unwrap_err();
                assert_eq!(error.transport_failure(), Some(failure));
                assert!(!error.retryable());
                assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
            }
            let error = client
                .handle()
                .connection()
                .try_wait()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.transport_failure(), Some(failure));
            assert!(client.handle().try_admit(Command::Diagnostics).is_err());
            assert!(events.recv_timeout(DEADLINE).unwrap().is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            drop(client);
            assert!(weak.upgrade().is_none());
        }
    }
}

struct GatedFailure {
    failure: TransportFailure,
    ready: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    calls: Arc<AtomicUsize>,
}

impl TransportConnector for GatedFailure {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let ready = self.ready.lock().unwrap().take();
        let failure = self.failure;
        Box::pin(async move {
            if let Some(ready) = ready {
                ready.await.unwrap();
            }
            Err(failure)
        })
    }
}

struct RetryThenFail {
    failure: TransportFailure,
    calls: Arc<AtomicUsize>,
}

impl TransportConnector for RetryThenFail {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let failure = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.failure
        } else {
            TransportFailure::ResourceLimit
        };
        Box::pin(async move { Err(failure) })
    }
}

#[test]
fn retryable_connector_failures_reconnect_before_a_terminal_failure_stops_the_driver() {
    for mqtt5 in [false, true] {
        for failure in [
            TransportFailure::Connect,
            TransportFailure::Io,
            TransportFailure::Timeout,
            TransportFailure::Abandoned,
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut config = config(mqtt5, 1883);
            config.common.connector = Some(TransportConnectorConfig {
                connector: Arc::new(RetryThenFail {
                    failure,
                    calls: calls.clone(),
                }),
                mode: TransportMode::Base,
            });
            let mut client = NativeClient::start(config).unwrap();
            let mut events = client.take_events().unwrap();
            let event = events.recv_timeout(DEADLINE).unwrap().unwrap();
            let WrapperEvent::Disconnected { error, .. } = event else {
                panic!("expected retryable disconnection, got {event:?}");
            };
            assert_eq!(error.transport_failure(), Some(failure));
            assert!(error.retryable());
            let event = events.recv_timeout(DEADLINE).unwrap().unwrap();
            let WrapperEvent::DriverTerminated(error) = event else {
                panic!("expected automatic driver termination, got {event:?}");
            };
            assert_eq!(
                error.transport_failure(),
                Some(TransportFailure::ResourceLimit)
            );
            assert_eq!(client.handle().state(), LifecycleState::Failed);
            assert!(events.recv_timeout(DEADLINE).unwrap().is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }
}

struct FailingStream {
    connack: Mutex<Option<Bytes>>,
    ready: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    failure: TransportFailure,
}

impl TransportIo for FailingStream {
    fn read(&self, max: usize) -> TransportIoFuture<Bytes> {
        if let Some(connack) = self.connack.lock().unwrap().take() {
            assert!(connack.len() <= max);
            return Box::pin(async move { Ok(connack) });
        }
        let ready = self.ready.lock().unwrap().take().unwrap();
        let failure = self.failure;
        Box::pin(async move {
            ready.await.unwrap();
            Err(failure.into_io())
        })
    }

    fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
        Box::pin(async move { Ok(bytes.len()) })
    }

    fn flush(&self) -> TransportIoFuture<()> {
        Box::pin(async { Ok(()) })
    }

    fn shutdown(&self) -> TransportIoFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

struct StreamConnector {
    io: Arc<FailingStream>,
    calls: Arc<AtomicUsize>,
}

impl TransportConnector for StreamConnector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let io = self.io.clone();
        Box::pin(async move {
            Ok(TransportConnection {
                io,
                mode: TransportMode::Base,
                network_handling: NetworkHandling::NotApplicable,
            })
        })
    }
}

#[test]
fn terminal_stream_failures_stop_the_driver_and_fail_pending_publishes() {
    for mqtt5 in [false, true] {
        for failure in [
            TransportFailure::InvalidResult,
            TransportFailure::ResourceLimit,
        ] {
            let (release, ready) = tokio::sync::oneshot::channel();
            let io = Arc::new(FailingStream {
                connack: Mutex::new(Some(Bytes::from_static(if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                }))),
                ready: Mutex::new(Some(ready)),
                failure,
            });
            let weak = Arc::downgrade(&io);
            let calls = Arc::new(AtomicUsize::new(0));
            let mut config = config(mqtt5, 1883);
            config.common.connector = Some(TransportConnectorConfig {
                connector: Arc::new(StreamConnector {
                    io,
                    calls: calls.clone(),
                }),
                mode: TransportMode::Base,
            });
            let mut client = NativeClient::start(config).unwrap();
            let mut events = connected(&mut client);
            let publish = publish(&client, b"pending");
            release.send(()).unwrap();
            let event = until(&mut events, |event| {
                matches!(
                    event,
                    WrapperEvent::DriverTerminated(_) | WrapperEvent::Disconnected { .. }
                )
            });
            let WrapperEvent::DriverTerminated(error) = event else {
                panic!("expected automatic driver termination, got {event:?}");
            };
            assert_eq!(error.transport_failure(), Some(failure));
            assert_eq!(error.context().phase, Some(ConnectionPhase::Established));
            assert!(!error.retryable());
            assert_eq!(client.handle().state(), LifecycleState::Failed);
            let error = terminal(&publish).unwrap_err();
            assert_eq!(error.transport_failure(), Some(failure));
            assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
            assert!(events.recv_timeout(DEADLINE).unwrap().is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            drop(client);
            assert!(weak.upgrade().is_none());
        }
    }
}

#[test]
fn established_streams_reject_native_layers_before_calling_the_connector() {
    let mut config = config(false, 1883);
    config.common.connector = Some(TransportConnectorConfig {
        connector: Arc::new(Failure(TransportFailure::Connect)),
        mode: TransportMode::Established,
    });
    config.common.transport = TransportConfig::Tls(TlsConfig::default());
    assert!(config.validate().is_err());
}

struct PendingConnector {
    cancelled: Arc<std::sync::atomic::AtomicUsize>,
}
struct Cancelled(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for Cancelled {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl TransportConnector for PendingConnector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let guard = Cancelled(self.cancelled.clone());
        Box::pin(async move {
            let _guard = guard;
            std::future::pending().await
        })
    }
}
#[test]
fn native_attempt_timeout_cancels_host_work_and_failed_construction_releases_owners() {
    for mqtt5 in [false, true] {
        let cancelled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector = Arc::new(PendingConnector {
            cancelled: cancelled.clone(),
        });
        let weak = Arc::downgrade(&connector);
        let mut config = config(mqtt5, 1883);
        config.common.connection_timeout = Duration::from_secs(1);
        config.common.connector = Some(TransportConnectorConfig {
            connector,
            mode: TransportMode::Base,
        });
        let mut invalid = config.clone();
        invalid.common.connection_timeout = Duration::ZERO;
        assert!(NativeClient::start(invalid).is_err());
        let mut client = NativeClient::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let event = until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Timeout);
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        client.closer().close_now(DEADLINE).unwrap();
        drop(client);
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn failed_native_client_construction_releases_its_only_connector_owner() {
    let connector = Arc::new(Failure(TransportFailure::Connect));
    let weak = Arc::downgrade(&connector);
    let mut config = config(false, 1883);
    config.common.connector = Some(TransportConnectorConfig {
        connector,
        mode: TransportMode::Base,
    });
    config.common.connection_timeout = Duration::ZERO;
    assert!(NativeClient::start(config).is_err());
    assert!(weak.upgrade().is_none());
}
