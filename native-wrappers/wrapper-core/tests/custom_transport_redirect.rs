mod support;

use bytes::Bytes;
use rumqttc_wrapper_core::*;
use std::sync::{Arc, Mutex};
use support::*;

const CONNACK: &[u8] = &[0x20, 3, 0, 0, 0];

fn redirect_packet(reference: &str, disconnect: bool) -> Bytes {
    let mut properties = vec![0x1c];
    properties.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
    properties.extend_from_slice(reference.as_bytes());
    let mut body = if disconnect {
        vec![0x9c]
    } else {
        vec![0, 0x9c]
    };
    body.push(u8::try_from(properties.len()).unwrap());
    body.extend(properties);
    let mut packet = if disconnect {
        CONNACK.to_vec()
    } else {
        Vec::new()
    };
    packet.extend_from_slice(&[
        if disconnect { 0xe0 } else { 0x20 },
        u8::try_from(body.len()).unwrap(),
    ]);
    packet.extend(body);
    packet.into()
}

struct Io {
    packet: Mutex<Option<Bytes>>,
    failure: Mutex<Option<(TransportFailure, tokio::sync::oneshot::Receiver<()>)>>,
}

impl TransportIo for Io {
    fn read(&self, _: usize) -> TransportIoFuture<Bytes> {
        if let Some(packet) = self.packet.lock().unwrap().take() {
            return Box::pin(async move { Ok(packet) });
        }
        if let Some((failure, release)) = self.failure.lock().unwrap().take() {
            return Box::pin(async move {
                release.await.unwrap();
                Err(failure.into_io())
            });
        }
        Box::pin(std::future::pending())
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

fn stream(
    packet: Option<Bytes>,
    failure: Option<(TransportFailure, tokio::sync::oneshot::Receiver<()>)>,
) -> TransportConnection {
    TransportConnection {
        io: Arc::new(Io {
            packet: Mutex::new(packet),
            failure: Mutex::new(failure),
        }),
        mode: TransportMode::Base,
        network_handling: NetworkHandling::NotApplicable,
    }
}

struct Connector {
    requests: Arc<Mutex<Vec<String>>>,
    origin: Bytes,
    failure: TransportFailure,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    backup_succeeds: bool,
    stream_failure: bool,
}

impl TransportConnector for Connector {
    fn connect(&self, request: TransportRequest) -> TransportFuture {
        self.requests.lock().unwrap().push(request.target.clone());
        if request.target == "origin.invalid:1883" {
            let packet = self.origin.clone();
            return Box::pin(async move { Ok(stream(Some(packet), None)) });
        }
        if request.target == "backup.invalid:1883" && self.backup_succeeds {
            // Redirects use an isolated session with a server-assigned client ID.
            return Box::pin(async {
                Ok(stream(
                    Some(Bytes::from_static(&[0x20, 7, 0, 0, 4, 0x12, 0, 1, b'x'])),
                    None,
                ))
            });
        }
        let release = self.release.lock().unwrap().take();
        let failure = self.failure;
        if self.stream_failure
            && let Some(release) = release
        {
            return Box::pin(async move { Ok(stream(None, Some((failure, release)))) });
        }
        Box::pin(async move {
            if let Some(release) = release {
                release.await.unwrap();
            }
            Err(failure)
        })
    }
}

struct Resolver;
impl SrvResolver for Resolver {
    fn resolve(&self, _: String) -> SrvFuture {
        Box::pin(async {
            Ok(vec![
                SrvRecord {
                    priority: 0,
                    weight: 0,
                    port: 1883,
                    target: "preferred.invalid".into(),
                },
                SrvRecord {
                    priority: 1,
                    weight: 0,
                    port: 1883,
                    target: "backup.invalid".into(),
                },
            ])
        })
    }
}

fn check_redirect(
    failure: TransportFailure,
    srv: bool,
    disconnect: bool,
    backup_succeeds: bool,
    transport: TransportConfig,
) {
    let terminal_transport = !matches!(
        failure,
        TransportFailure::Connect
            | TransportFailure::Io
            | TransportFailure::Timeout
            | TransportFailure::Abandoned
    );
    let (release, ready) = tokio::sync::oneshot::channel();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let connector = Arc::new(Connector {
        requests: requests.clone(),
        origin: redirect_packet(
            if srv {
                "_mqtt._tcp.service.invalid"
            } else {
                "preferred.invalid:1883"
            },
            disconnect,
        ),
        failure,
        release: Mutex::new(Some(ready)),
        backup_succeeds,
        stream_failure: !matches!(transport, TransportConfig::Tcp),
    });
    let weak = Arc::downgrade(&connector);
    let mut config = ClientConfig::v5("redirect", "origin.invalid", 1883);
    config.common.connector = Some(TransportConnectorConfig {
        connector,
        mode: TransportMode::Base,
    });
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 3,
        transport,
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Resolver)));
    let mut client = NativeClient::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    until(
        &mut events,
        |event| matches!(event, WrapperEvent::Redirect(event) if event.failure.is_none()),
    );
    let pending = client
        .handle()
        .try_admit(Command::Unsubscribe(UnsubscribeCommand {
            filters: vec!["pending".into()],
            protocol: UnsubscribeProtocolOptions::VersionNeutral,
        }))
        .unwrap();
    release.send(()).unwrap();

    if srv && !terminal_transport && backup_succeeds {
        until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "origin.invalid:1883",
                "preferred.invalid:1883",
                "backup.invalid:1883"
            ]
        );
        client.closer().close_now(DEADLINE).unwrap();
    } else {
        let event = until(&mut events, |event| {
            matches!(
                event,
                WrapperEvent::DriverTerminated(_) | WrapperEvent::Connected { .. }
            ) || matches!(event, WrapperEvent::Redirect(event) if event.failure.is_some())
        });
        let WrapperEvent::Redirect(redirect) = event else {
            panic!("terminal failure was hidden by fallback: {event:?}");
        };
        let event = events.recv_timeout(DEADLINE).unwrap().unwrap();
        let WrapperEvent::DriverTerminated(error) = event else {
            panic!("expected terminal event, got {event:?}")
        };
        assert_eq!(error.transport_failure(), Some(failure));
        assert_eq!(error.redirect_failure(), Some(RedirectFailure::Transport));
        assert!(!error.retryable());
        assert_eq!(client.handle().state(), LifecycleState::Failed);
        let pending_error = terminal(&pending).unwrap_err();
        assert_eq!(pending_error.transport_failure(), Some(failure));
        assert_eq!(
            pending_error.redirect_failure(),
            Some(RedirectFailure::Transport)
        );
        assert!(!pending_error.retryable());
        if !disconnect {
            let error = client
                .handle()
                .connection()
                .try_wait()
                .unwrap()
                .unwrap_err();
            assert_eq!(error.transport_failure(), Some(failure));
            assert_eq!(error.redirect_failure(), Some(RedirectFailure::Transport));
        }
        let count = if srv && !terminal_transport { 3 } else { 2 };
        assert_eq!(requests.lock().unwrap().len(), count);
        assert_eq!(redirect.failure, Some(RedirectFailure::Transport));
        if srv {
            assert_eq!(redirect.srv_candidate_index, Some(count - 1));
            assert_eq!(redirect.srv_candidate_count, Some(2));
        }
        assert!(events.recv_timeout(DEADLINE).unwrap().is_none());
    }
    drop(client);
    assert!(weak.upgrade().is_none());
}

#[test]
fn redirects_preserve_transport_failures_and_srv_fallback_policy() {
    for disconnect in [false, true] {
        for failure in [
            TransportFailure::NetworkOptions,
            TransportFailure::Composition,
            TransportFailure::InvalidResult,
            TransportFailure::Panic,
            TransportFailure::ResourceLimit,
            TransportFailure::Connect,
            TransportFailure::Io,
            TransportFailure::Timeout,
            TransportFailure::Abandoned,
        ] {
            check_redirect(failure, false, disconnect, true, TransportConfig::Tcp);
            check_redirect(failure, true, disconnect, true, TransportConfig::Tcp);
        }
        for failure in [
            TransportFailure::Connect,
            TransportFailure::Io,
            TransportFailure::Timeout,
            TransportFailure::Abandoned,
        ] {
            check_redirect(failure, true, disconnect, false, TransportConfig::Tcp);
        }
    }
}

#[cfg(feature = "use-native-tls")]
#[test]
fn srv_fallback_stops_on_terminal_native_tls_stream_failures() {
    check_tls_redirects(TlsBackend::Native);
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn srv_fallback_stops_on_terminal_rustls_stream_failures() {
    // The caller may select no-provider or both providers; choose one explicitly.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    check_tls_redirects(TlsBackend::Rustls);
}

#[cfg(any(feature = "use-native-tls", feature = "use-rustls-no-provider"))]
fn check_tls_redirects(backend: TlsBackend) {
    for disconnect in [false, true] {
        for failure in [
            TransportFailure::InvalidResult,
            TransportFailure::Panic,
            TransportFailure::ResourceLimit,
        ] {
            check_redirect(
                failure,
                true,
                disconnect,
                false,
                TransportConfig::Tls(TlsConfig {
                    backend,
                    ..TlsConfig::default()
                }),
            );
        }
    }
}
