#![cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
#[cfg(feature = "use-rustls-no-provider")]
use std::time::{Duration, SystemTime};

use rcgen::KeyPair;
#[cfg(feature = "use-rustls-no-provider")]
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa};
use rumqttc_wrapper_core::*;
use rustls::pki_types::PrivatePkcs8KeyDer;

#[path = "support/tls.rs"]
#[allow(dead_code)]
mod fixture;
mod support;

fn server(
    certificate: rustls::pki_types::CertificateDer<'static>,
    key: &KeyPair,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Arc<rustls::ServerConfig> {
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .unwrap(),
    )
}

fn exercise(tls: TlsConfig, server: Arc<rustls::ServerConfig>, mqtt5: bool, succeeds: bool) {
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
            // Rejected TLS must not deliver MQTT CONNECT or application credentials.
            let result = stream.read(&mut [0]);
            assert!(matches!(result, Err(_) | Ok(0)));
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
        let event = support::until(&mut events, |event| {
            matches!(event, WrapperEvent::Disconnected { .. })
        });
        let WrapperEvent::Disconnected { error, .. } = event else {
            unreachable!()
        };
        assert_eq!(error.kind(), ErrorKind::Tls);
        client.closer().close_now(support::DEADLINE).unwrap();
    }
    client.join(support::DEADLINE).unwrap();
    broker.join();
}

#[test]
fn advertised_version_policies_enforce_the_allowed_set() {
    fixture::install_provider_for_providerless_client();
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        let capabilities = backend.capabilities();
        if capabilities.version_policies == 0 {
            continue;
        }
        for policy in [
            TlsVersionPolicy::Tls12Only,
            TlsVersionPolicy::Tls13Only,
            TlsVersionPolicy::Tls12OrTls13,
        ] {
            let tls = TlsConfig {
                backend,
                roots: TlsRootPolicy::Pem(cert.cert.pem().into()),
                version_policy: policy,
                ..TlsConfig::default()
            };
            if capabilities.version_policies & (1 << policy as u32) == 0 {
                assert_eq!(tls.validate().unwrap_err().kind(), ErrorKind::Configuration);
                continue;
            }
            for mqtt5 in [false, true] {
                for (version, allowed) in [
                    (
                        &rustls::version::TLS12,
                        policy != TlsVersionPolicy::Tls13Only
                            && (backend == TlsBackend::Native || cfg!(feature = "tls12")),
                    ),
                    (
                        &rustls::version::TLS13,
                        policy != TlsVersionPolicy::Tls12Only
                            && !(backend == TlsBackend::Native && cfg!(target_vendor = "apple")),
                    ),
                ] {
                    exercise(
                        tls.clone(),
                        server(cert.cert.der().clone(), &cert.signing_key, &[version]),
                        mqtt5,
                        allowed,
                    );
                }
            }
        }
    }
}

#[test]
fn unsupported_pins_and_malformed_combined_roots_fail_before_networking() {
    fixture::install_provider_for_providerless_client();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if backend.capabilities().version_policies == 0 {
            continue;
        }
        for mqtt5 in [false, true] {
            let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
            config.common.transport = TransportConfig::Tls(TlsConfig {
                backend,
                roots: TlsRootPolicy::PlatformAndPem(b"private-invalid-root".as_slice().into()),
                ..TlsConfig::default()
            });
            assert_eq!(support::start(config).unwrap_err().kind(), ErrorKind::Tls);
        }
        if backend == TlsBackend::Native {
            let tls = TlsConfig {
                backend,
                pins: vec![TlsPin {
                    target: TlsPinTarget::LeafCertificate,
                    sha256: [0; 32],
                }],
                ..TlsConfig::default()
            };
            assert_eq!(tls.validate().unwrap_err().kind(), ErrorKind::Configuration);
        }
    }
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(feature = "use-rustls-no-provider")]
fn pin(cert: &rustls::pki_types::CertificateDer<'_>, target: TlsPinTarget) -> TlsPin {
    use sha2::{Digest, Sha256};
    let sha256 = match target {
        TlsPinTarget::LeafCertificate => Sha256::digest(cert.as_ref()),
        TlsPinTarget::LeafSpki => Sha256::digest(
            rustls::server::ParsedCertificate::try_from(cert)
                .unwrap()
                .subject_public_key_info()
                .as_ref(),
        ),
    }
    .into();
    TlsPin { target, sha256 }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn pinning_revalidates_rotated_certificates_on_ticket_enabled_reconnects() {
    fixture::install_provider_for_providerless_client();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let first_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.serial_number = Some(1.into());
    let first = params.signed_by(&first_key, &ca).unwrap();
    for mqtt5 in [false, true] {
        for target in [TlsPinTarget::LeafCertificate, TlsPinTarget::LeafSpki] {
            for same_key in [false, true] {
                let second_key = if same_key {
                    KeyPair::from_pem(&first_key.serialize_pem()).unwrap()
                } else {
                    KeyPair::generate().unwrap()
                };
                params.serial_number = Some(2.into());
                let second = params.signed_by(&second_key, &ca).unwrap();
                let first_server =
                    server(first.der().clone(), &first_key, &[&rustls::version::TLS13]);
                let mut second_server = (*server(
                    second.der().clone(),
                    &second_key,
                    &[&rustls::version::TLS13],
                ))
                .clone();
                // Preserve the stateful ticket cache across the certificate rotation.
                second_server.session_storage = Arc::clone(&first_server.session_storage);
                assert!(first_server.send_tls13_tickets > 0);
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    roots: TlsRootPolicy::Pem(ca.pem().into()),
                    pins: vec![pin(first.der(), target)],
                    ..TlsConfig::default()
                });
                let (release, barrier) = std::sync::mpsc::channel();
                let succeeds = same_key && target == TlsPinTarget::LeafSpki;
                let broker = support::Broker::spawn(move || {
                    let mut stream = rustls::StreamOwned::new(
                        rustls::ServerConnection::new(first_server).unwrap(),
                        support::accept(&listener),
                    );
                    assert_eq!(support::frame(&mut stream)[0], 0x10);
                    stream
                        .write_all(if mqtt5 {
                            &[0x20, 3, 0, 0, 0]
                        } else {
                            &[0x20, 2, 0, 0]
                        })
                        .unwrap();
                    stream.flush().unwrap();
                    barrier.recv_timeout(support::DEADLINE).unwrap();
                    drop(stream);
                    let mut stream = rustls::StreamOwned::new(
                        rustls::ServerConnection::new(Arc::new(second_server)).unwrap(),
                        support::accept(&listener),
                    );
                    if succeeds {
                        assert_eq!(support::frame(&mut stream)[0], 0x10);
                        assert_eq!(
                            stream.conn.handshake_kind(),
                            Some(rustls::HandshakeKind::Full)
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
                    } else {
                        assert!(matches!(stream.read(&mut [0]), Err(_) | Ok(0)));
                    }
                });
                let mut client = support::start(config).unwrap();
                let mut events = support::connected(&mut client);
                release.send(()).unwrap();
                support::until(&mut events, |event| {
                    matches!(event, WrapperEvent::Disconnected { .. })
                });
                if succeeds {
                    support::until(&mut events, |event| {
                        matches!(event, WrapperEvent::Connected { .. })
                    });
                    client.closer().close(support::DEADLINE).unwrap();
                } else {
                    let event = support::until(
                        &mut events,
                        |event| matches!(event, WrapperEvent::Disconnected { error, .. } if error.kind() == ErrorKind::Tls),
                    );
                    assert!(matches!(event, WrapperEvent::Disconnected { .. }));
                    client.closer().close_now(support::DEADLINE).unwrap();
                }
                client.join(support::DEADLINE).unwrap();
                broker.join();
            }
        }
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn matching_pins_do_not_bypass_names_chains_or_expiry() {
    fixture::install_provider_for_providerless_client();
    let now = SystemTime::now();
    for failure in ["name", "chain", "expired", "pin", "backup"] {
        let mut params = CertificateParams::new(vec![
            if failure == "name" {
                "wrong.invalid"
            } else {
                "127.0.0.1"
            }
            .into(),
        ])
        .unwrap();
        if failure == "expired" {
            params.not_before = (now - Duration::from_secs(86400 * 2)).into();
            params.not_after = (now - Duration::from_secs(86400)).into();
        }
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let wrong = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let mut pins = vec![if failure == "pin" || failure == "backup" {
            pin(wrong.cert.der(), TlsPinTarget::LeafSpki)
        } else {
            pin(cert.der(), TlsPinTarget::LeafCertificate)
        }];
        if failure == "backup" {
            pins.push(pin(cert.der(), TlsPinTarget::LeafSpki));
        }
        let tls = TlsConfig {
            roots: TlsRootPolicy::Pem(
                if failure == "chain" {
                    wrong.cert.pem()
                } else {
                    cert.pem()
                }
                .into(),
            ),
            pins,
            ..TlsConfig::default()
        };
        let formatted = format!("{tls:?}");
        assert!(!formatted.contains(&cert.pem()));
        for mqtt5 in [false, true] {
            exercise(
                tls.clone(),
                server(cert.der().clone(), &key, &[&rustls::version::TLS13]),
                mqtt5,
                failure == "backup",
            );
        }
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn matching_pins_preserve_handshake_signature_verification() {
    use rustls::sign::{CertifiedKey, Signer, SigningKey, SingleCertAndKey};

    #[derive(Debug)]
    struct InvalidSignatureKey(Arc<dyn SigningKey>);
    #[derive(Debug)]
    struct InvalidSigner(Box<dyn Signer>);
    impl SigningKey for InvalidSignatureKey {
        fn choose_scheme(&self, offered: &[rustls::SignatureScheme]) -> Option<Box<dyn Signer>> {
            self.0
                .choose_scheme(offered)
                .map(|signer| Box::new(InvalidSigner(signer)) as Box<dyn Signer>)
        }
        fn algorithm(&self) -> rustls::SignatureAlgorithm {
            self.0.algorithm()
        }
    }
    impl Signer for InvalidSigner {
        fn sign(&self, message: &[u8]) -> std::result::Result<Vec<u8>, rustls::Error> {
            let mut signature = self.0.sign(message)?;
            signature[0] ^= 1;
            Ok(signature)
        }
        fn scheme(&self) -> rustls::SignatureScheme {
            self.0.scheme()
        }
    }
    fixture::install_provider_for_providerless_client();
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let key = provider
        .key_provider
        .load_private_key(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into())
        .unwrap();
    let bad_key = CertifiedKey::new(
        vec![cert.cert.der().clone()],
        Arc::new(InvalidSignatureKey(key)),
    );
    let server = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(SingleCertAndKey::from(bad_key))),
    );
    let tls = TlsConfig {
        roots: TlsRootPolicy::Pem(cert.cert.pem().into()),
        pins: vec![pin(cert.cert.der(), TlsPinTarget::LeafSpki)],
        ..TlsConfig::default()
    };
    for mqtt5 in [false, true] {
        exercise(tls.clone(), Arc::clone(&server), mqtt5, false);
    }
}

#[test]
fn disabling_sni_preserves_hostname_verification_for_each_backend() {
    fixture::install_provider_for_providerless_client();
    let cert = rcgen::generate_simple_self_signed(vec!["wrong.invalid".into()]).unwrap();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if backend.capabilities().version_policies == 0 {
            continue;
        }
        let tls = TlsConfig {
            backend,
            roots: TlsRootPolicy::Pem(cert.cert.pem().into()),
            sni_policy: rumqttc_wrapper_core::TlsSniPolicy::Disabled,
            ..Default::default()
        };
        for mqtt5 in [false, true] {
            exercise(
                tls.clone(),
                server(
                    cert.cert.der().clone(),
                    &cert.signing_key,
                    &[&rustls::version::TLS13],
                ),
                mqtt5,
                false,
            );
        }
    }
}
