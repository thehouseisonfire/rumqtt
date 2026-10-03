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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
    config.common.transport = TransportConfig::Tls(tls);
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
    let mut client = NativeClient::start(config).unwrap();
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
        let mut client = NativeClient::start(config).unwrap();
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
            let mut client = NativeClient::start(config).unwrap();
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
            let mut client = NativeClient::start(config).unwrap();
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
    let mut client = NativeClient::start(config).unwrap();
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
