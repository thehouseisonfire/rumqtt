#![cfg(feature = "use-rustls-no-provider")]
use rumqttc_wrapper_core::*;
use rustls::pki_types::{PrivatePkcs8KeyDer, pem::PemObject};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
#[path = "support/custom_transport.rs"]
mod byte_transport;
#[path = "support/tls.rs"]
mod fixture;
mod support;

struct Policy {
    calls: AtomicUsize,
    reason: Option<TlsCallbackReason>,
    slow: bool,
}
impl TlsVerifier for Policy {
    fn verify(
        &self,
        request: &TlsVerificationRequest<'_>,
    ) -> std::result::Result<(), TlsCallbackReason> {
        assert_eq!(request.server_name, "127.0.0.1");
        assert_eq!(request.layer, TlsLayer::Broker);
        assert!(!request.certificates.is_empty());
        assert!(request.deadline.is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.slow {
            std::thread::sleep(Duration::from_millis(1200));
        }
        match self.reason {
            Some(TlsCallbackReason::Panic) => panic!("secret callback panic"),
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }
}
struct Host {
    key: Arc<dyn rustls::sign::SigningKey>,
    select: Option<TlsCallbackReason>,
    invalid: bool,
    decline: bool,
    selects: AtomicUsize,
    signs: AtomicUsize,
}
impl TlsIdentityProvider for Host {
    fn select(
        &self,
        request: &TlsIdentityRequest<'_>,
    ) -> std::result::Result<Option<usize>, TlsCallbackReason> {
        assert_eq!(request.layer, TlsLayer::Broker);
        assert!(request.signature_schemes.contains(&0x0403));
        self.selects.fetch_add(1, Ordering::SeqCst);
        if let Some(reason) = self.select {
            return Err(reason);
        }
        Ok(if self.decline { None } else { Some(0) })
    }
    fn sign(
        &self,
        request: &TlsSigningRequest<'_>,
    ) -> std::result::Result<Vec<u8>, TlsCallbackReason> {
        assert_eq!(request.key_id, b"host-only-key");
        self.signs.fetch_add(1, Ordering::SeqCst);
        let mut signature = self
            .key
            .choose_scheme(&[request.signature_scheme.into()])
            .unwrap()
            .sign(request.message)
            .unwrap();
        if self.invalid {
            signature[0] ^= 1;
        }
        Ok(signature)
    }
}
fn run(
    tls: TlsConfig,
    server: Arc<rustls::ServerConfig>,
    mqtt5: bool,
    expected: Option<TlsCallbackFailure>,
    succeeds: bool,
) {
    run_with_connector(tls, server, mqtt5, expected, succeeds, false);
}
fn run_with_connector(
    tls: TlsConfig,
    server: Arc<rustls::ServerConfig>,
    mqtt5: bool,
    expected: Option<TlsCallbackFailure>,
    succeeds: bool,
    custom: bool,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
    config.common.transport = TransportConfig::Tls(tls);
    if custom {
        config.common.connector = Some(byte_transport::configured().0);
    }
    let broker = support::Broker::spawn(move || {
        let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
        if succeeds {
            assert_eq!(support::frame(&mut stream)[0], 0x10);
            stream
                .write_all(if mqtt5 {
                    &[0x20, 3, 0, 0, 0]
                } else {
                    &[0x20, 2, 0, 0]
                })
                .unwrap();
            stream.flush().unwrap();
            assert_eq!(support::frame(&mut stream)[0], 0xe0);
        } else {
            assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
        }
    });
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    if succeeds {
        support::until(&mut events, |event| {
            matches!(event, WrapperEvent::Connected { .. })
        });
        client.closer().close(support::DEADLINE).unwrap();
    } else {
        let event = loop {
            let event = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
            if matches!(
                event,
                WrapperEvent::Disconnected { .. } | WrapperEvent::DriverTerminated(_)
            ) {
                break event;
            }
        };
        let error = match event {
            WrapperEvent::Disconnected { error, .. } | WrapperEvent::DriverTerminated(error) => {
                error
            }
            other => panic!("unexpected event: {other:?}"),
        };
        assert_eq!(error.tls_callback_failure(), expected);
        if let Some(failure) = expected {
            assert_eq!(error.retryable(), failure.retryable());
        }
        assert!(!format!("{error:?}").contains("secret callback"));
        client.closer().close_now(support::DEADLINE).unwrap();
    }
    client.join(support::DEADLINE).unwrap();
    broker.join();
}
#[test]
fn supplemental_verification_preserves_trust_and_reports_terminal_failures() {
    let fixture = fixture::Fixture::new();
    for mqtt5 in [false, true] {
        for reason in [
            None,
            Some(TlsCallbackReason::Rejected),
            Some(TlsCallbackReason::Panic),
            Some(TlsCallbackReason::Transient),
        ] {
            let policy = Arc::new(Policy {
                calls: AtomicUsize::new(0),
                reason,
                slow: false,
            });
            let mut tls = fixture.client(TlsBackend::Rustls);
            tls.verifier = Some(TlsVerifierConfig(policy.clone()));
            run(
                tls,
                fixture.server.clone(),
                mqtt5,
                reason.map(|reason| TlsCallbackFailure {
                    stage: TlsCallbackStage::Verification,
                    reason,
                    layer: TlsLayer::Broker,
                }),
                reason.is_none(),
            );
            assert_eq!(policy.calls.load(Ordering::SeqCst), 1);
        }
        let policy = Arc::new(Policy {
            calls: AtomicUsize::new(0),
            reason: None,
            slow: false,
        });
        let mut tls = fixture.client(TlsBackend::Rustls);
        tls.pins.push(TlsPin {
            target: TlsPinTarget::LeafCertificate,
            sha256: [0; 32],
        });
        tls.verifier = Some(TlsVerifierConfig(policy.clone()));
        run(tls, fixture.server.clone(), mqtt5, None, false);
        assert_eq!(policy.calls.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn external_signatures_are_checked_and_failed_selection_cannot_become_anonymous_tls() {
    let fixture = fixture::Fixture::new();
    let client = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let key = provider
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(client.signing_key.serialize_der()).into())
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(client.cert.der().clone()).unwrap();
    let auth =
        rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
            .allow_unauthenticated()
            .build()
            .unwrap();
    let server = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(auth)
        .with_single_cert(
            vec![
                rustls::pki_types::CertificateDer::from_pem_slice(fixture.pem.as_bytes()).unwrap(),
            ],
            rustls::pki_types::PrivateKeyDer::from_pem_slice(fixture.key_pem.as_bytes()).unwrap(),
        )
        .unwrap(),
    );
    for mqtt5 in [false, true] {
        for scenario in ["success", "decline", "select-fail", "invalid-signature"] {
            let host = Arc::new(Host {
                key: key.clone(),
                select: (scenario == "select-fail").then_some(TlsCallbackReason::Rejected),
                invalid: scenario == "invalid-signature",
                decline: scenario == "decline",
                selects: AtomicUsize::new(0),
                signs: AtomicUsize::new(0),
            });
            let external = TlsExternalIdentityConfig::new(
                vec![TlsExternalIdentity {
                    certificate_pem: client.cert.pem().into(),
                    key_id: b"host-only-key".as_slice().into(),
                    signature_schemes: vec![0x0403],
                }],
                host.clone(),
            )
            .unwrap();
            assert_eq!(host.selects.load(Ordering::SeqCst), 0);
            let mut tls = fixture.client(TlsBackend::Rustls);
            tls.identity = Some(TlsClientIdentity::External(external));
            let failure = match scenario {
                "select-fail" => Some((
                    TlsCallbackStage::IdentitySelection,
                    TlsCallbackReason::Rejected,
                )),
                "invalid-signature" => Some((
                    TlsCallbackStage::Signing,
                    TlsCallbackReason::InvalidSignature,
                )),
                _ => None,
            };
            run(
                tls,
                server.clone(),
                mqtt5,
                failure.map(|(stage, reason)| TlsCallbackFailure {
                    stage,
                    reason,
                    layer: TlsLayer::Broker,
                }),
                failure.is_none(),
            );
            assert_eq!(host.selects.load(Ordering::SeqCst), 1);
            assert_eq!(
                host.signs.load(Ordering::SeqCst),
                usize::from(scenario == "success" || scenario == "invalid-signature")
            );
        }
    }
}
#[test]
fn cipher_and_policy_validation_is_enforceable_and_transactional() {
    fixture::install_provider_for_providerless_client();
    let suites = TlsBackend::Rustls.supported_cipher_suites().unwrap();
    assert!(!suites.is_empty());
    for suites in [vec![0xffff], vec![suites[0], suites[0]], vec![0; 65]] {
        assert!(
            TlsConfig {
                cipher_suites: suites,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    if cfg!(feature = "tls12") {
        assert!(
            TlsConfig {
                cipher_suites: vec![0xc02f],
                version_policy: TlsVersionPolicy::Tls13Only,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TlsConfig {
                cipher_suites: vec![0x1301],
                version_policy: TlsVersionPolicy::Tls12Only,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    TlsConfig {
        cipher_suites: vec![suites[0]],
        sni_policy: TlsSniPolicy::Disabled,
        resumption_policy: TlsResumptionPolicy::Disabled,
        ..Default::default()
    }
    .validate()
    .unwrap();
}

#[test]
fn callbacks_cannot_accept_results_after_the_original_attempt_deadline() {
    let fixture = fixture::Fixture::new();
    for mqtt5 in [false, true] {
        let policy = Arc::new(Policy {
            calls: AtomicUsize::new(0),
            reason: None,
            slow: true,
        });
        let mut tls = fixture.client(TlsBackend::Rustls);
        tls.verifier = Some(TlsVerifierConfig(policy.clone()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
        config.common.connection_timeout = Duration::from_secs(1);
        config.common.transport = TransportConfig::Tls(tls);
        let server = fixture.server.clone();
        let broker = support::Broker::spawn(move || {
            let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
            assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
        });
        let mut client = support::start(config).unwrap();
        let mut events = client.take_events().unwrap();
        let event = support::until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(
            error.tls_callback_failure().map(|failure| failure.reason),
            Some(TlsCallbackReason::Timeout)
        );
        client.closer().close_now(support::DEADLINE).unwrap();
        client.join(support::DEADLINE).unwrap();
        broker.join();
    }
}

#[test]
fn sni_and_cipher_selection_affect_the_wire_without_changing_certificate_authority() {
    let fixture = fixture::Fixture::new();
    for mqtt5 in [false, true] {
        for sni in [TlsSniPolicy::Enabled, TlsSniPolicy::Disabled] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
            config.common.broker = BrokerTarget::Tcp {
                host: "localhost".into(),
                port: listener.local_addr().unwrap().port(),
            };
            config.common.transport = TransportConfig::Tls(TlsConfig {
                sni_policy: sni,
                cipher_suites: vec![0x1301],
                ..fixture.client(TlsBackend::Rustls)
            });
            let server = fixture.server.clone();
            let broker = support::Broker::spawn(move || {
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server).unwrap(),
                    support::accept(&listener),
                );
                assert_eq!(support::frame(&mut stream)[0], 0x10);
                assert_eq!(
                    stream.conn.server_name(),
                    (sni == TlsSniPolicy::Enabled).then_some("localhost")
                );
                assert_eq!(
                    u16::from(stream.conn.negotiated_cipher_suite().unwrap().suite()),
                    0x1301
                );
                stream
                    .write_all(if mqtt5 {
                        &[0x20, 3, 0, 0, 0]
                    } else {
                        &[0x20, 2, 0, 0]
                    })
                    .unwrap();
                stream.flush().unwrap();
                assert_eq!(support::frame(&mut stream)[0], 0xe0);
            });
            let mut client = support::start(config).unwrap();
            let mut events = support::connected(&mut client);
            client.closer().close(support::DEADLINE).unwrap();
            client.join(support::DEADLINE).unwrap();
            broker.join();
            assert!(events.try_recv().is_ok());
        }
    }
}

#[test]
fn explicit_resumption_policy_is_enforced_on_reconnect() {
    let fixture = fixture::Fixture::new();
    for mqtt5 in [false, true] {
        for policy in [TlsResumptionPolicy::Default, TlsResumptionPolicy::Disabled] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
            config.common.transport = TransportConfig::Tls(TlsConfig {
                resumption_policy: policy,
                ..fixture.client(TlsBackend::Rustls)
            });
            let server = fixture.server.clone();
            let (release, barrier) = std::sync::mpsc::channel();
            let broker = support::Broker::spawn(move || {
                let mut first = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server.clone()).unwrap(),
                    support::accept(&listener),
                );
                assert_eq!(support::frame(&mut first)[0], 0x10);
                first
                    .write_all(if mqtt5 {
                        &[0x20, 3, 0, 0, 0]
                    } else {
                        &[0x20, 2, 0, 0]
                    })
                    .unwrap();
                first.flush().unwrap();
                barrier.recv_timeout(support::DEADLINE).unwrap();
                drop(first);
                let mut second = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server).unwrap(),
                    support::accept(&listener),
                );
                assert_eq!(support::frame(&mut second)[0], 0x10);
                assert_eq!(
                    second.conn.handshake_kind(),
                    Some(if policy == TlsResumptionPolicy::Default {
                        rustls::HandshakeKind::Resumed
                    } else {
                        rustls::HandshakeKind::Full
                    })
                );
                second
                    .write_all(if mqtt5 {
                        &[0x20, 3, 0, 0, 0]
                    } else {
                        &[0x20, 2, 0, 0]
                    })
                    .unwrap();
                second.flush().unwrap();
                assert_eq!(support::frame(&mut second)[0], 0xe0);
            });
            let mut client = support::start(config).unwrap();
            let mut events = support::connected(&mut client);
            release.send(()).unwrap();
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Disconnected { .. })
            });
            support::until(&mut events, |event| {
                matches!(event, WrapperEvent::Connected { .. })
            });
            client.closer().close(support::DEADLINE).unwrap();
            client.join(support::DEADLINE).unwrap();
            broker.join();
        }
    }
}

#[test]
fn terminal_verification_failure_stops_srv_redirect_fallback() {
    struct Resolver(u16, u16);
    impl SrvResolver for Resolver {
        fn resolve(&self, _: String) -> SrvFuture {
            let ports = [self.0, self.1];
            Box::pin(async move {
                Ok(ports
                    .into_iter()
                    .enumerate()
                    .map(|(i, port)| SrvRecord {
                        priority: u16::try_from(i).unwrap(),
                        weight: 0,
                        port,
                        target: "localhost.".into(),
                    })
                    .collect())
            })
        }
    }
    struct Reject;
    impl TlsVerifier for Reject {
        fn verify(
            &self,
            request: &TlsVerificationRequest<'_>,
        ) -> std::result::Result<(), TlsCallbackReason> {
            assert_eq!(request.layer, TlsLayer::Redirect);
            Err(TlsCallbackReason::Rejected)
        }
    }
    let fixture = fixture::Fixture::new();
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    let preferred = TcpListener::bind("127.0.0.1:0").unwrap();
    let backup = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = support::config(true, origin.local_addr().unwrap().port());
    config.common.network.local_address = Some("127.0.0.1:0".parse().unwrap());
    let ProtocolConfig::V5(v5) = &mut config.protocol else {
        unreachable!()
    };
    let mut tls = fixture.client(TlsBackend::Rustls);
    tls.verifier = Some(TlsVerifierConfig(Arc::new(Reject)));
    v5.redirect_policy = RedirectPolicy::Follow {
        max_attempts: 2,
        transport: TransportConfig::Tls(tls),
    };
    v5.srv_resolver = Some(SrvResolverConfig(Arc::new(Resolver(
        preferred.local_addr().unwrap().port(),
        backup.local_addr().unwrap().port(),
    ))));
    let server = fixture.server;
    let broker = support::Broker::spawn(move || {
        let mut socket = support::accept(&origin);
        support::frame(&mut socket);
        let reference = b"_mqtt._tcp.service.invalid";
        let mut properties = vec![0x1c];
        properties.extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
        properties.extend_from_slice(reference);
        let mut packet = vec![
            0x20,
            u8::try_from(properties.len() + 3).unwrap(),
            0,
            0x9c,
            u8::try_from(properties.len()).unwrap(),
        ];
        packet.extend(properties);
        socket.write_all(&packet).unwrap();
        let mut stream = fixture::wrap(Box::new(support::accept(&preferred)), server);
        assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
    });
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let error = loop {
        let event = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
        if let WrapperEvent::DriverTerminated(error) = event {
            break error;
        }
    };
    assert_eq!(
        error.tls_callback_failure(),
        Some(TlsCallbackFailure {
            stage: TlsCallbackStage::Verification,
            reason: TlsCallbackReason::Rejected,
            layer: TlsLayer::Redirect
        })
    );
    assert!(error.redirect_failure().is_some());
    assert!(!error.retryable());
    client.join(support::DEADLINE).unwrap();
    broker.join();
    backup.set_nonblocking(true).unwrap();
    assert_eq!(
        backup.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn every_advertised_external_algorithm_signs_with_its_selected_certificate() {
    let fixture = fixture::Fixture::new();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    for scheme in TlsBackend::Rustls.supported_signature_schemes().unwrap() {
        let legacy = matches!(scheme, 0x0401 | 0x0501 | 0x0601);
        if legacy && !cfg!(feature = "tls12") {
            continue;
        }
        let algorithm = match scheme {
            0x0403 => &rcgen::PKCS_ECDSA_P256_SHA256,
            0x0503 => &rcgen::PKCS_ECDSA_P384_SHA384,
            0x0603 => &rcgen::PKCS_ECDSA_P521_SHA512,
            0x0807 => &rcgen::PKCS_ED25519,
            _ => &rcgen::PKCS_RSA_SHA256,
        };
        let key = rcgen::KeyPair::generate_for(algorithm).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["client".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let signing_key = provider
            .key_provider
            .load_private_key(PrivatePkcs8KeyDer::from(key.serialize_der()).into())
            .unwrap();
        let host = Arc::new(Host {
            key: signing_key,
            select: None,
            invalid: false,
            decline: false,
            selects: AtomicUsize::new(0),
            signs: AtomicUsize::new(0),
        });
        let external = TlsExternalIdentityConfig::new(
            vec![TlsExternalIdentity {
                certificate_pem: cert.pem().into(),
                key_id: b"host-only-key".as_slice().into(),
                signature_schemes: vec![scheme],
            }],
            host.clone(),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .unwrap();
        let builder = rustls::ServerConfig::builder_with_provider(provider.clone());
        let builder = if legacy {
            builder.with_protocol_versions(&[&rustls::version::TLS12])
        } else {
            builder.with_protocol_versions(&[&rustls::version::TLS13])
        }
        .unwrap();
        let server = Arc::new(
            builder
                .with_client_cert_verifier(verifier)
                .with_single_cert(
                    vec![
                        rustls::pki_types::CertificateDer::from_pem_slice(fixture.pem.as_bytes())
                            .unwrap(),
                    ],
                    rustls::pki_types::PrivateKeyDer::from_pem_slice(fixture.key_pem.as_bytes())
                        .unwrap(),
                )
                .unwrap(),
        );
        let tls = TlsConfig {
            identity: Some(TlsClientIdentity::External(external)),
            version_policy: if legacy {
                TlsVersionPolicy::Tls12Only
            } else {
                TlsVersionPolicy::Tls13Only
            },
            ..fixture.client(TlsBackend::Rustls)
        };
        run(tls, server, true, None, true);
        assert_eq!(host.signs.load(Ordering::SeqCst), 1);
    }
}

#[derive(Clone, Copy)]
enum DeferredMode {
    Accept,
    Reject,
    Decline,
    BadIndex,
    BadSignature,
    Pending,
    PanicConstruct,
    PanicPoll,
    PanicDrop,
    PendingPanicDrop,
}
struct DeferredHost {
    key: Arc<dyn rustls::sign::SigningKey>,
    target: TlsCallbackStage,
    mode: DeferredMode,
    delayed: bool,
    entered: flume::Sender<TlsCallbackStage>,
    destroyed: Arc<AtomicUsize>,
}
struct ResponseDrop(Arc<AtomicUsize>);
impl Drop for ResponseDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
struct PanickingDrop<T> {
    future: TlsCallbackFuture<T>,
}
impl<T> std::future::Future for PanickingDrop<T> {
    type Output = std::result::Result<T, TlsCallbackReason>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.get_mut().future.as_mut().poll(cx)
    }
}
impl<T> Drop for PanickingDrop<T> {
    fn drop(&mut self) {
        panic!("secret callback destruction");
    }
}
impl DeferredHost {
    fn response<T: Send + 'static>(
        &self,
        stage: TlsCallbackStage,
        answer: impl FnOnce(DeferredMode) -> std::result::Result<T, TlsCallbackReason> + Send + 'static,
    ) -> TlsCallbackFuture<T> {
        self.entered.send(stage).unwrap();
        let mode = if stage == self.target {
            self.mode
        } else {
            DeferredMode::Accept
        };
        if matches!(mode, DeferredMode::PanicConstruct) {
            panic!("secret callback construction");
        }
        let dropped = ResponseDrop(self.destroyed.clone());
        let delayed = self.delayed;
        let future: TlsCallbackFuture<T> = Box::pin(async move {
            let _dropped = dropped;
            if delayed {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            match mode {
                DeferredMode::Pending | DeferredMode::PendingPanicDrop => {
                    std::future::pending().await
                }
                DeferredMode::PanicPoll => panic!("secret callback polling"),
                DeferredMode::Reject => Err(TlsCallbackReason::Rejected),
                _ => answer(mode),
            }
        });
        if matches!(
            mode,
            DeferredMode::PanicDrop | DeferredMode::PendingPanicDrop
        ) {
            Box::pin(PanickingDrop { future })
        } else {
            future
        }
    }
}
impl AsyncTlsVerifier for DeferredHost {
    fn verify(&self, r: AsyncTlsVerificationRequest) -> TlsCallbackFuture<()> {
        assert!(!r.certificates.is_empty());
        assert!(r.deadline.is_some());
        self.response(TlsCallbackStage::Verification, move |_| {
            assert_eq!(r.server_name, "127.0.0.1");
            Ok(())
        })
    }
}
impl AsyncTlsIdentityProvider for DeferredHost {
    fn select(&self, r: AsyncTlsIdentityRequest) -> TlsCallbackFuture<Option<usize>> {
        self.response(TlsCallbackStage::IdentitySelection, move |mode| {
            assert!(r.signature_schemes.contains(&0x0403));
            Ok(match mode {
                DeferredMode::Decline => None,
                DeferredMode::BadIndex => Some(99),
                _ => Some(0),
            })
        })
    }
    fn sign(&self, r: AsyncTlsSigningRequest) -> TlsCallbackFuture<Vec<u8>> {
        let key = self.key.clone();
        self.response(TlsCallbackStage::Signing, move |mode| {
            assert_eq!(&r.key_id[..], b"host-only-key");
            let mut signature = key
                .choose_scheme(&[r.signature_scheme.into()])
                .unwrap()
                .sign(&r.message)
                .unwrap();
            if matches!(mode, DeferredMode::BadSignature) {
                signature[0] ^= 1;
            }
            Ok(signature)
        })
    }
}
fn deferred_identity(
    fixture: &fixture::Fixture,
) -> (
    Arc<rustls::ServerConfig>,
    Arc<dyn rustls::sign::SigningKey>,
    Vec<TlsExternalIdentity>,
) {
    let client = rcgen::generate_simple_self_signed(vec!["client".into()]).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let key = provider
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(client.signing_key.serialize_der()).into())
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(client.cert.der().clone()).unwrap();
    let auth = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .allow_unauthenticated()
    .build()
    .unwrap();
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(auth)
        .with_single_cert(
            vec![
                rustls::pki_types::CertificateDer::from_pem_slice(fixture.pem.as_bytes()).unwrap(),
            ],
            rustls::pki_types::PrivateKeyDer::from_pem_slice(fixture.key_pem.as_bytes()).unwrap(),
        )
        .unwrap();
    (
        Arc::new(server),
        key,
        vec![TlsExternalIdentity {
            certificate_pem: client.cert.pem().into(),
            key_id: b"host-only-key".as_slice().into(),
            signature_schemes: vec![0x0403],
        }],
    )
}
fn deferred_profile(
    fixture: &fixture::Fixture,
    catalog: Vec<TlsExternalIdentity>,
    host: Arc<DeferredHost>,
) -> TlsConfig {
    let mut tls = fixture.client(TlsBackend::Rustls);
    tls.async_verifier = Some(AsyncTlsVerifierConfig(host.clone()));
    tls.identity = Some(TlsClientIdentity::ExternalAsync(
        AsyncTlsExternalIdentityConfig::new(catalog, host).unwrap(),
    ));
    tls
}
#[test]
fn deferred_hooks_accept_immediate_and_delayed_answers_and_preserve_authentication() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    for mqtt5 in [false, true] {
        for delayed in [false, true] {
            for (stage, mode, reason) in [
                (TlsCallbackStage::Verification, DeferredMode::Accept, None),
                (
                    TlsCallbackStage::IdentitySelection,
                    DeferredMode::Decline,
                    None,
                ),
                (
                    TlsCallbackStage::IdentitySelection,
                    DeferredMode::BadIndex,
                    Some(TlsCallbackReason::InvalidResponse),
                ),
                (
                    TlsCallbackStage::Signing,
                    DeferredMode::BadSignature,
                    Some(TlsCallbackReason::InvalidSignature),
                ),
                (
                    TlsCallbackStage::Verification,
                    DeferredMode::Reject,
                    Some(TlsCallbackReason::Rejected),
                ),
            ] {
                let (entered, _receiver) = flume::unbounded();
                let host = Arc::new(DeferredHost {
                    key: key.clone(),
                    target: stage,
                    mode,
                    delayed,
                    entered,
                    destroyed: Arc::new(AtomicUsize::new(0)),
                });
                let tls = deferred_profile(&fixture, catalog.clone(), host);
                run(
                    tls,
                    server.clone(),
                    mqtt5,
                    reason.map(|reason| TlsCallbackFailure {
                        stage,
                        reason,
                        layer: TlsLayer::Broker,
                    }),
                    reason.is_none(),
                );
            }
        }
    }
}
#[test]
fn deferred_pending_hooks_cancel_without_blocking_driver_join_and_drop_owned_work() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    for cancellation in [
        PendingCancellation::Immediate,
        PendingCancellation::ConnectionTimeout,
        PendingCancellation::GracefulTimeout,
    ] {
        for mqtt5 in [false, true] {
            for stage in [
                TlsCallbackStage::Verification,
                TlsCallbackStage::IdentitySelection,
                TlsCallbackStage::Signing,
            ] {
                let (entered, receiver) = flume::unbounded();
                let destroyed = Arc::new(AtomicUsize::new(0));
                let host = Arc::new(DeferredHost {
                    key: key.clone(),
                    target: stage,
                    mode: DeferredMode::Pending,
                    delayed: false,
                    entered,
                    destroyed: destroyed.clone(),
                });
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.connection_timeout = cancellation.connection_timeout();
                config.common.transport =
                    TransportConfig::Tls(deferred_profile(&fixture, catalog.clone(), host));
                let server = server.clone();
                let broker = support::Broker::spawn(move || {
                    let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
                    assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
                });
                let mut client = support::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                let mut calls = 0;
                loop {
                    calls += 1;
                    if receiver.recv_timeout(support::DEADLINE).unwrap() == stage {
                        break;
                    }
                }
                if matches!(cancellation, PendingCancellation::GracefulTimeout) {
                    assert_graceful_close_timeout(&client);
                    assert!(matches!(
                        events.recv_timeout(support::DEADLINE).unwrap().unwrap(),
                        WrapperEvent::ImmediateShutdownCompleted
                    ));
                    assert_eq!(client.handle().state(), LifecycleState::Closed);
                } else if matches!(cancellation, PendingCancellation::ConnectionTimeout) {
                    let event = events.recv_timeout(support::DEADLINE).unwrap().unwrap();
                    let error = match event {
                        WrapperEvent::Disconnected { error, .. }
                        | WrapperEvent::DriverTerminated(error) => error,
                        other => panic!("unexpected {other:?}"),
                    };
                    assert_eq!(
                        error.tls_callback_failure(),
                        Some(TlsCallbackFailure {
                            stage,
                            reason: TlsCallbackReason::Timeout,
                            layer: TlsLayer::Broker
                        })
                    );
                }
                client.closer().close_now(Duration::from_secs(1)).unwrap();
                client.join(Duration::from_secs(1)).unwrap();
                assert_eq!(destroyed.load(Ordering::SeqCst), calls);
                broker.join();
            }
        }
    }
}
#[test]
fn deferred_future_panics_are_contained_during_construction_polling_and_destruction() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    for stage in [
        TlsCallbackStage::Verification,
        TlsCallbackStage::IdentitySelection,
        TlsCallbackStage::Signing,
    ] {
        for mode in [
            DeferredMode::PanicConstruct,
            DeferredMode::PanicPoll,
            DeferredMode::PanicDrop,
        ] {
            let (entered, _receiver) = flume::unbounded();
            let host = Arc::new(DeferredHost {
                key: key.clone(),
                target: stage,
                mode,
                delayed: true,
                entered,
                destroyed: Arc::new(AtomicUsize::new(0)),
            });
            run(
                deferred_profile(&fixture, catalog.clone(), host),
                server.clone(),
                true,
                Some(TlsCallbackFailure {
                    stage,
                    reason: TlsCallbackReason::Panic,
                    layer: TlsLayer::Broker,
                }),
                false,
            );
        }
    }
}

#[test]
fn deferred_worker_cancels_network_waits_before_runtime_shutdown() {
    let fixture = fixture::Fixture::new();
    let (_, key, catalog) = deferred_identity(&fixture);
    let (entered, receiver) = flume::unbounded();
    let host = Arc::new(DeferredHost {
        key,
        target: TlsCallbackStage::Verification,
        mode: DeferredMode::Accept,
        delayed: false,
        entered,
        destroyed: Arc::new(AtomicUsize::new(0)),
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = support::config(true, listener.local_addr().unwrap().port());
    config.common.transport = TransportConfig::Tls(deferred_profile(&fixture, catalog, host));
    let client = support::start(config).unwrap();
    let mut socket = support::accept(&listener);
    let mut hello = [0; 4096];
    assert!(socket.read(&mut hello).unwrap() > 0);
    client.closer().close_now(Duration::from_secs(1)).unwrap();
    client.join(Duration::from_secs(1)).unwrap();
    assert!(receiver.try_recv().is_err());
    assert_eq!(socket.read(&mut hello).unwrap(), 0);
}
#[test]
fn deferred_verification_cannot_override_pins_and_conflicting_policies_fail_eagerly() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    let (entered, receiver) = flume::unbounded();
    let host = Arc::new(DeferredHost {
        key,
        target: TlsCallbackStage::Verification,
        mode: DeferredMode::Accept,
        delayed: false,
        entered,
        destroyed: Arc::new(AtomicUsize::new(0)),
    });
    let mut tls = deferred_profile(&fixture, catalog, host);
    tls.pins.push(TlsPin {
        target: TlsPinTarget::LeafCertificate,
        sha256: [0; 32],
    });
    run(tls.clone(), server, true, None, false);
    assert!(
        !receiver
            .try_iter()
            .any(|stage| stage == TlsCallbackStage::Verification)
    );
    tls.verifier = Some(TlsVerifierConfig(Arc::new(Policy {
        calls: AtomicUsize::new(0),
        reason: None,
        slow: false,
    })));
    assert!(tls.validate().is_err());
    tls.verifier = None;
    tls.backend = TlsBackend::Native;
    assert!(tls.validate().is_err());
}

#[test]
fn deferred_handshakes_preserve_custom_transport_short_reads_and_writes() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    for mqtt5 in [false, true] {
        let (entered, _receiver) = flume::unbounded();
        let host = Arc::new(DeferredHost {
            key: key.clone(),
            target: TlsCallbackStage::Verification,
            mode: DeferredMode::Accept,
            delayed: true,
            entered,
            destroyed: Arc::new(AtomicUsize::new(0)),
        });
        run_with_connector(
            deferred_profile(&fixture, catalog.clone(), host),
            server.clone(),
            mqtt5,
            None,
            true,
            true,
        );
    }
}

struct MixedHost {
    deferred: Arc<DeferredHost>,
    synchronous: Host,
    driver: std::sync::Mutex<Option<std::thread::ThreadId>>,
}
impl MixedHost {
    fn check_driver(&self) {
        let thread = std::thread::current().id();
        let mut driver = self.driver.lock().unwrap();
        if std::env::var("RUMQTTC_TEST_EXECUTION").as_deref() == Ok("shared") {
            driver.get_or_insert(thread);
            assert_eq!(std::thread::current().name(), Some("rumqtt-context-worker"));
        } else {
            assert_eq!(*driver.get_or_insert(thread), thread);
        }
        drop(driver);
    }
}
impl TlsVerifier for MixedHost {
    fn verify(&self, _: &TlsVerificationRequest<'_>) -> std::result::Result<(), TlsCallbackReason> {
        self.check_driver();
        Ok(())
    }
}
impl AsyncTlsVerifier for MixedHost {
    fn verify(&self, r: AsyncTlsVerificationRequest) -> TlsCallbackFuture<()> {
        self.check_driver();
        self.deferred.verify(r)
    }
}
impl TlsIdentityProvider for MixedHost {
    fn select(
        &self,
        r: &TlsIdentityRequest<'_>,
    ) -> std::result::Result<Option<usize>, TlsCallbackReason> {
        self.check_driver();
        self.synchronous.select(r)
    }
    fn sign(&self, r: &TlsSigningRequest<'_>) -> std::result::Result<Vec<u8>, TlsCallbackReason> {
        self.check_driver();
        self.synchronous.sign(r)
    }
}
impl AsyncTlsIdentityProvider for MixedHost {
    fn select(&self, r: AsyncTlsIdentityRequest) -> TlsCallbackFuture<Option<usize>> {
        self.check_driver();
        self.deferred.select(r)
    }
    fn sign(&self, r: AsyncTlsSigningRequest) -> TlsCallbackFuture<Vec<u8>> {
        self.check_driver();
        self.deferred.sign(r)
    }
}
#[test]
fn mixed_profiles_invoke_synchronous_and_deferred_hooks_on_the_same_driver() {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    for mqtt5 in [false, true] {
        for async_identity in [false, true] {
            let (entered, receiver) = flume::unbounded();
            let host = Arc::new(MixedHost {
                deferred: Arc::new(DeferredHost {
                    key: key.clone(),
                    target: TlsCallbackStage::Verification,
                    mode: DeferredMode::Accept,
                    delayed: true,
                    entered,
                    destroyed: Arc::new(AtomicUsize::new(0)),
                }),
                synchronous: Host {
                    key: key.clone(),
                    select: None,
                    invalid: false,
                    decline: false,
                    selects: AtomicUsize::new(0),
                    signs: AtomicUsize::new(0),
                },
                driver: std::sync::Mutex::new(None),
            });
            let mut tls = fixture.client(TlsBackend::Rustls);
            if async_identity {
                tls.verifier = Some(TlsVerifierConfig(host.clone()));
                tls.identity = Some(TlsClientIdentity::ExternalAsync(
                    AsyncTlsExternalIdentityConfig::new(catalog.clone(), host.clone()).unwrap(),
                ));
            } else {
                tls.async_verifier = Some(AsyncTlsVerifierConfig(host.clone()));
                tls.identity = Some(TlsClientIdentity::External(
                    TlsExternalIdentityConfig::new(catalog.clone(), host.clone()).unwrap(),
                ));
            }
            run(tls, server.clone(), mqtt5, None, true);
            assert!(host.driver.lock().unwrap().is_some());
            assert_eq!(
                receiver.try_iter().count(),
                if async_identity { 2 } else { 1 }
            );
            assert_eq!(
                host.synchronous.signs.load(Ordering::SeqCst),
                usize::from(!async_identity)
            );
        }
    }
}

const CALLBACK_STAGES: [TlsCallbackStage; 3] = [
    TlsCallbackStage::Verification,
    TlsCallbackStage::IdentitySelection,
    TlsCallbackStage::Signing,
];

#[derive(Clone, Copy)]
enum PendingCancellation {
    Immediate,
    ConnectionTimeout,
    GracefulTimeout,
}

impl PendingCancellation {
    const fn connection_timeout(self) -> Duration {
        match self {
            Self::ConnectionTimeout => Duration::from_secs(1),
            Self::Immediate | Self::GracefulTimeout => support::DEADLINE.saturating_mul(2),
        }
    }
}

fn assert_graceful_close_timeout(client: &NativeClient) {
    let close = client
        .handle()
        .try_admit(Command::GracefulDisconnect {
            timeout: Some(Duration::from_millis(100)),
        })
        .unwrap();
    let error = close
        .completion
        .wait_timeout(support::DEADLINE)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
    assert_eq!(error.tls_callback_failure(), None);
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep cancellation setup and terminal assertions together"
)]
fn pending_destructor_failure(
    mqtt5: bool,
    stage: TlsCallbackStage,
    layer: TlsLayer,
    cancellation: PendingCancellation,
) {
    let fixture = fixture::Fixture::new();
    let (server, key, catalog) = deferred_identity(&fixture);
    let (entered, receiver) = flume::unbounded();
    let destroyed = Arc::new(AtomicUsize::new(0));
    let host = Arc::new(DeferredHost {
        key,
        target: stage,
        mode: DeferredMode::PendingPanicDrop,
        delayed: false,
        entered,
        destroyed: destroyed.clone(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let tls = deferred_profile(&fixture, catalog, host);
    let mut config = support::config(mqtt5, port);
    config.common.connection_timeout = cancellation.connection_timeout();
    let origin = match layer {
        TlsLayer::Broker => {
            config.common.transport = TransportConfig::Tls(tls);
            None
        }
        #[cfg(feature = "http-proxy")]
        TlsLayer::Proxy => {
            config.common.proxy = Some(ProxyConfig::Http {
                host: "127.0.0.1".into(),
                port,
                credentials: None,
                tls: Some(tls),
            });
            None
        }
        TlsLayer::Redirect => {
            let origin = TcpListener::bind("127.0.0.1:0").unwrap();
            config.common.broker = BrokerTarget::Tcp {
                host: "127.0.0.1".into(),
                port: origin.local_addr().unwrap().port(),
            };
            let ProtocolConfig::V5(v5) = &mut config.protocol else {
                panic!("redirect requires v5")
            };
            v5.redirect_policy = RedirectPolicy::Follow {
                max_attempts: 1,
                transport: TransportConfig::Tls(tls),
            };
            Some(support::Broker::spawn(move || {
                let mut socket = support::accept(&origin);
                assert_eq!(support::frame(&mut socket)[0], 0x10);
                let reference = format!("127.0.0.1:{port}");
                let mut properties = vec![0x1c];
                properties
                    .extend_from_slice(&u16::try_from(reference.len()).unwrap().to_be_bytes());
                properties.extend_from_slice(reference.as_bytes());
                let mut response = vec![
                    0x20,
                    u8::try_from(properties.len() + 3).unwrap(),
                    0,
                    0x9c,
                    u8::try_from(properties.len()).unwrap(),
                ];
                response.extend(properties);
                socket.write_all(&response).unwrap();
            }))
        }
        #[cfg(not(feature = "http-proxy"))]
        TlsLayer::Proxy => panic!("proxy feature disabled"),
    };
    let broker = support::Broker::spawn(move || {
        let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
        assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
    });
    let mut client = support::start(config).unwrap();
    let mut events = client.take_events().unwrap();
    let mut calls = 0;
    loop {
        calls += 1;
        if receiver.recv_timeout(support::DEADLINE).unwrap() == stage {
            break;
        }
    }
    let pending = client
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
    let expected = Some(TlsCallbackFailure {
        stage,
        reason: TlsCallbackReason::Panic,
        layer,
    });
    if matches!(cancellation, PendingCancellation::GracefulTimeout) {
        assert_graceful_close_timeout(&client);
    } else if matches!(cancellation, PendingCancellation::Immediate) {
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let callers: Vec<_> = (0..2)
                .map(|_| {
                    let closer = client.closer();
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        closer.close_now(support::DEADLINE)
                    })
                })
                .collect();
            barrier.wait();
            for caller in callers {
                assert_eq!(
                    caller.join().unwrap().unwrap_err().tls_callback_failure(),
                    expected
                );
            }
        });
        assert_eq!(
            client
                .closer()
                .close_now(support::DEADLINE)
                .unwrap_err()
                .tls_callback_failure(),
            expected
        );
    }
    loop {
        match events.recv_timeout(support::DEADLINE).unwrap().unwrap() {
            WrapperEvent::DriverTerminated(error) => {
                assert_eq!(error.tls_callback_failure(), expected);
                assert_eq!(error.kind(), ErrorKind::Tls);
                assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
                assert!(!error.retryable());
                assert_ne!(error.code(), ErrorCode::InternalPanic);
                assert!(!format!("{error:?}").contains("secret callback"));
                break;
            }
            WrapperEvent::Redirect(_) => {}
            event => panic!("destructor failure was suppressed: {event:?}"),
        }
    }
    let pending_error = pending
        .completion
        .wait_timeout(support::DEADLINE)
        .unwrap_err();
    assert_eq!(pending_error.tls_callback_failure(), expected);
    assert_eq!(pending_error.kind(), ErrorKind::Tls);
    assert_eq!(pending_error.delivery_status(), DeliveryStatus::Ambiguous);
    client.join(support::DEADLINE).unwrap();
    assert_eq!(client.handle().state(), LifecycleState::Failed);
    assert_eq!(destroyed.load(Ordering::SeqCst), calls);
    assert!(events.try_recv().unwrap().is_none());
    broker.join();
    if let Some(origin) = origin {
        origin.join();
    }
}

#[test]
fn deferred_pending_destructor_failures_are_terminal_on_close() {
    for mqtt5 in [false, true] {
        for stage in CALLBACK_STAGES {
            pending_destructor_failure(
                mqtt5,
                stage,
                TlsLayer::Broker,
                PendingCancellation::Immediate,
            );
        }
    }
}
#[test]
fn deferred_pending_destructor_failures_override_retryable_timeouts() {
    for mqtt5 in [false, true] {
        for stage in CALLBACK_STAGES {
            pending_destructor_failure(
                mqtt5,
                stage,
                TlsLayer::Broker,
                PendingCancellation::ConnectionTimeout,
            );
        }
    }
}
#[test]
fn deferred_pending_destructor_failures_reach_driver_from_redirects() {
    for stage in CALLBACK_STAGES {
        pending_destructor_failure(
            true,
            stage,
            TlsLayer::Redirect,
            PendingCancellation::Immediate,
        );
    }
}
#[cfg(feature = "http-proxy")]
#[test]
fn deferred_pending_destructor_failures_reach_driver_from_https_proxies() {
    for mqtt5 in [false, true] {
        for stage in CALLBACK_STAGES {
            pending_destructor_failure(
                mqtt5,
                stage,
                TlsLayer::Proxy,
                PendingCancellation::Immediate,
            );
        }
    }
}
#[test]
fn deferred_cancelled_destructor_panics_do_not_print_private_payloads() {
    for test in [
        "deferred_pending_destructor_failures_are_terminal_on_close",
        "deferred_pending_destructor_failures_are_terminal_on_graceful_close_timeout",
    ] {
        let output = support::process_output(
            std::process::Command::new(std::env::current_exe().unwrap()).args([
                "--exact",
                test,
                "--nocapture",
            ]),
        );
        assert!(output.status.success());
        for bytes in [&output.stdout, &output.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("secret callback destruction"));
        }
    }
}

#[test]
fn deferred_pending_destructor_failures_are_terminal_on_graceful_close_timeout() {
    for mqtt5 in [false, true] {
        for stage in CALLBACK_STAGES {
            pending_destructor_failure(
                mqtt5,
                stage,
                TlsLayer::Broker,
                PendingCancellation::GracefulTimeout,
            );
        }
    }
}

#[test]
fn deferred_graceful_timeout_destructor_failures_reach_driver_from_redirects() {
    for stage in CALLBACK_STAGES {
        pending_destructor_failure(
            true,
            stage,
            TlsLayer::Redirect,
            PendingCancellation::GracefulTimeout,
        );
    }
}

#[cfg(feature = "http-proxy")]
#[test]
fn deferred_graceful_timeout_destructor_failures_reach_driver_from_https_proxies() {
    for mqtt5 in [false, true] {
        for stage in CALLBACK_STAGES {
            pending_destructor_failure(
                mqtt5,
                stage,
                TlsLayer::Proxy,
                PendingCancellation::GracefulTimeout,
            );
        }
    }
}

#[test]
fn tls_destructor_failures_are_isolated_between_clients_sharing_a_profile() {
    struct SharedVerifier {
        calls: AtomicUsize,
        entered: flume::Sender<usize>,
        release: flume::Receiver<()>,
    }
    impl AsyncTlsVerifier for SharedVerifier {
        fn verify(&self, _: AsyncTlsVerificationRequest) -> TlsCallbackFuture<()> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send(call).unwrap();
            if call == 0 {
                Box::pin(PanickingDrop {
                    future: Box::pin(std::future::pending()),
                })
            } else {
                let release = self.release.clone();
                Box::pin(async move {
                    release
                        .recv_async()
                        .await
                        .map_err(|_| TlsCallbackReason::Failed)
                })
            }
        }
    }
    let fixture = fixture::Fixture::new();
    let (entered, receiver) = flume::unbounded();
    let (release, released) = flume::bounded(1);
    let host = Arc::new(SharedVerifier {
        calls: AtomicUsize::new(0),
        entered,
        release: released,
    });
    let mut tls = fixture.client(TlsBackend::Rustls);
    tls.async_verifier = Some(AsyncTlsVerifierConfig(host.clone()));
    let mut clients = Vec::new();
    let mut brokers = Vec::new();
    for call in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = support::config(true, listener.local_addr().unwrap().port());
        config.common.transport = TransportConfig::Tls(tls.clone());
        let server = fixture.server.clone();
        brokers.push(support::Broker::spawn(move || {
            let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
            if call == 0 {
                assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
            } else {
                assert_eq!(support::frame(&mut stream)[0], 0x10);
                stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
                stream.flush().unwrap();
                assert_eq!(support::frame(&mut stream)[0], 0xe0);
            }
        }));
        clients.push(support::start(config).unwrap());
        assert_eq!(receiver.recv_timeout(support::DEADLINE).unwrap(), call);
    }
    let error = clients[0]
        .closer()
        .close_now(support::DEADLINE)
        .unwrap_err();
    assert_eq!(
        error.tls_callback_failure().unwrap().reason,
        TlsCallbackReason::Panic
    );
    release.send(()).unwrap();
    let mut events = clients[1].take_events().unwrap();
    support::until(&mut events, |event| {
        matches!(event, WrapperEvent::Connected { .. })
    });
    assert_eq!(
        clients[1].closer().close(support::DEADLINE).unwrap(),
        Completion::GracefulShutdown
    );
    assert_eq!(clients[1].handle().state(), LifecycleState::Closed);
    assert_eq!(host.calls.load(Ordering::SeqCst), 2);
    for broker in brokers {
        broker.join();
    }
}
