#![cfg(feature = "use-native-tls")]

mod support;

use bytes::Bytes;
use rumqttc_wrapper_core::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use support::*;

#[derive(Clone, Copy, Debug)]
enum FailureAt {
    Read,
    Write,
    Flush,
}

struct FailingIo {
    operation: FailureAt,
    failure: TransportFailure,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FailingIo {
    fn fail<T: Send + 'static>(&self) -> TransportIoFuture<T> {
        let release = self.release.lock().unwrap().take().unwrap();
        let failure = self.failure;
        Box::pin(async move {
            release.await.unwrap();
            Err(failure.into_io())
        })
    }
}

impl TransportIo for FailingIo {
    fn read(&self, _: usize) -> TransportIoFuture<Bytes> {
        if matches!(self.operation, FailureAt::Read) {
            self.fail()
        } else {
            Box::pin(std::future::pending())
        }
    }

    fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
        if matches!(self.operation, FailureAt::Write) {
            self.fail()
        } else {
            Box::pin(async move { Ok(bytes.len()) })
        }
    }

    fn flush(&self) -> TransportIoFuture<()> {
        if matches!(self.operation, FailureAt::Flush) {
            self.fail()
        } else {
            Box::pin(async { Ok(()) })
        }
    }

    fn shutdown(&self) -> TransportIoFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

struct Connector {
    io: Arc<FailingIo>,
    calls: Arc<AtomicUsize>,
}

impl TransportConnector for Connector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            return Box::pin(async { Err(TransportFailure::ResourceLimit) });
        }
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
fn native_tls_handshake_preserves_transport_failures_and_driver_policy() {
    for mqtt5 in [false, true] {
        for operation in [FailureAt::Read, FailureAt::Write, FailureAt::Flush] {
            for failure in [
                TransportFailure::InvalidResult,
                TransportFailure::Panic,
                TransportFailure::ResourceLimit,
                TransportFailure::Io,
            ] {
                let (release, ready) = tokio::sync::oneshot::channel();
                let io = Arc::new(FailingIo {
                    operation,
                    failure,
                    release: Mutex::new(Some(ready)),
                });
                let weak = Arc::downgrade(&io);
                let calls = Arc::new(AtomicUsize::new(0));
                let mut config = config(mqtt5, 1883);
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    backend: TlsBackend::Native,
                    ..TlsConfig::default()
                });
                config.common.connector = Some(TransportConnectorConfig {
                    connector: Arc::new(Connector {
                        io,
                        calls: calls.clone(),
                    }),
                    mode: TransportMode::Base,
                });
                let mut client = support::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                let pending = client
                    .handle()
                    .try_admit(Command::Unsubscribe(UnsubscribeCommand {
                        filters: vec!["pending".into()],
                        protocol: UnsubscribeProtocolOptions::VersionNeutral,
                    }))
                    .unwrap();
                release.send(()).unwrap();

                let mut event = events.recv_timeout(DEADLINE).unwrap().unwrap();
                let terminal_failure = if failure == TransportFailure::Io {
                    let WrapperEvent::Disconnected { error, .. } = event else {
                        panic!("expected retryable disconnection for {operation:?}, got {event:?}");
                    };
                    assert_eq!(error.transport_failure(), Some(failure));
                    assert!(error.retryable());
                    event = events.recv_timeout(DEADLINE).unwrap().unwrap();
                    TransportFailure::ResourceLimit
                } else {
                    failure
                };
                let WrapperEvent::DriverTerminated(error) = event else {
                    panic!(
                        "expected automatic termination for {operation:?}/{failure:?}, got {event:?}"
                    );
                };
                assert_eq!(error.transport_failure(), Some(terminal_failure));
                assert!(!error.retryable());
                assert_eq!(client.handle().state(), LifecycleState::Failed);
                assert_eq!(
                    terminal(&pending).unwrap_err().transport_failure(),
                    Some(terminal_failure)
                );
                let error = client
                    .handle()
                    .connection()
                    .try_wait()
                    .unwrap()
                    .unwrap_err();
                assert_eq!(error.transport_failure(), Some(terminal_failure));
                assert!(events.recv_timeout(DEADLINE).unwrap().is_none());
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    if failure == TransportFailure::Io {
                        2
                    } else {
                        1
                    }
                );
                drop(client);
                assert!(weak.upgrade().is_none());
            }
        }
    }
}
