#![cfg(any(feature = "use-rustls", feature = "use-native-tls"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rumqttc_wrapper_core::*;

#[path = "support/tls.rs"]
mod fixture;
mod support;

#[test]
fn tls_input_ownership_is_released_on_every_driver_exit() {
    struct OwnedPem {
        bytes: Vec<u8>,
        _owner: Arc<()>,
    }
    impl AsRef<[u8]> for OwnedPem {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    support::capture::start();
    let fixture = fixture::Fixture::new();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls"))
            || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mqtt5 in [false, true] {
            for mode in [
                "failed-start",
                "graceful",
                "immediate",
                "abandon",
                "driver-failure",
            ] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let owner = Arc::new(());
                let weak = Arc::downgrade(&owner);
                let roots = Bytes::from_owner(OwnedPem {
                    bytes: fixture.pem.as_bytes().to_vec(),
                    _owner: owner,
                });
                let retained = roots.clone();
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    backend,
                    roots: TlsRootPolicy::Pem(roots),
                    alpn_protocols: if mode == "failed-start" {
                        vec![vec![]]
                    } else {
                        vec![]
                    },
                    identity: if backend == TlsBackend::Rustls {
                        Some(TlsClientIdentity::RustlsPem {
                            certificate: fixture.pem.clone().into(),
                            private_key: SecretBytes::new(fixture.key_pem.as_bytes().to_vec()),
                        })
                    } else {
                        #[cfg(all(target_os = "linux", feature = "use-native-tls"))]
                        {
                            Some(native_identity(&fixture.pem, &fixture.key_pem))
                        }
                        #[cfg(not(all(target_os = "linux", feature = "use-native-tls")))]
                        {
                            None
                        }
                    },
                });
                if mode == "failed-start" {
                    assert_eq!(
                        NativeClient::start(config).unwrap_err().kind(),
                        ErrorKind::Configuration
                    );
                } else {
                    let server = fixture.server.clone();
                    let broker = support::Broker::spawn(move || {
                        let mut stream =
                            fixture::wrap(Box::new(support::accept(&listener)), server);
                        assert_eq!(support::frame(&mut stream)[0], 0x10);
                        stream
                            .write_all(if mqtt5 {
                                &[0x20, 3, 0, 0, 0]
                            } else {
                                &[0x20, 2, 0, 0]
                            })
                            .unwrap();
                        stream.flush().unwrap();
                        if mode == "driver-failure" {
                            // Abrupt TLS closure can be EOF or UnexpectedEof,
                            // depending on whether close_notify was transmitted.
                            let result = stream.read(&mut [0]);
                            assert!(matches!(result, Ok(0) | Err(_)));
                        } else {
                            assert_eq!(support::frame(&mut stream)[0], 0xe0);
                        }
                    });
                    let mut client = NativeClient::start(config).unwrap();
                    let mut events = support::connected(&mut client);
                    let closer = client.closer();
                    match mode {
                        "graceful" => {
                            closer.close(support::DEADLINE).unwrap();
                        }
                        "immediate" => closer.close_now(support::DEADLINE).unwrap(),
                        "abandon" => {
                            drop(client);
                            closer.close_now(support::DEADLINE).unwrap();
                        }
                        "driver-failure" => {
                            client.handle().terminate_for_internal_panic();
                            let event = support::until(&mut events, |event| {
                                matches!(event, WrapperEvent::DriverTerminated(_))
                            });
                            let WrapperEvent::DriverTerminated(error) = event else {
                                unreachable!()
                            };
                            assert_eq!(error.code(), ErrorCode::InternalPanic);
                            support::capture::assert_redacted(
                                &format!("{error} {error:?}"),
                                &[&fixture.key_pem],
                            );
                            client.join(support::DEADLINE).unwrap();
                        }
                        _ => unreachable!(),
                    }
                    broker.join();
                }
                // Host-retained copies keep their own ownership; destroying
                // them after driver teardown releases the final input buffer.
                assert!(weak.upgrade().is_some());
                drop(retained);
                assert!(weak.upgrade().is_none());
            }
        }
    }
}

#[test]
fn tls_failure_panic_output_is_redacted() {
    let output = support::process_output(
        std::process::Command::new(std::env::current_exe().unwrap()).args([
            "--exact",
            "tls_input_ownership_is_released_on_every_driver_exit",
            "--nocapture",
        ]),
    );
    assert!(
        output.status.success(),
        "TLS lifecycle subprocess failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.contains("BEGIN PRIVATE KEY"));
    assert!(!output.contains("panicked at"));
}

#[cfg(all(target_os = "linux", feature = "use-native-tls"))]
#[test]
fn valid_pkcs12_with_wrong_password_fails_without_disclosing_identity() {
    support::capture::start();
    let fixture = fixture::Fixture::new();
    let TlsClientIdentity::NativePkcs12 { identity, .. } =
        native_identity(&fixture.pem, &fixture.key_pem)
    else {
        unreachable!()
    };
    let archive_debug = format!("{:?}", identity.expose());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for mqtt5 in [false, true] {
        let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
        config.common.transport = TransportConfig::Tls(TlsConfig {
            identity: Some(TlsClientIdentity::NativePkcs12 {
                identity: identity.clone(),
                password: SecretBytes::new(b"private-wrong-password".to_vec()),
            }),
            ..fixture.client(TlsBackend::Native)
        });
        let formatted = format!("{config:?}");
        let error = NativeClient::start(config).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Tls);
        support::capture::assert_redacted(
            &format!("{formatted} {error:?} {error}"),
            &[
                &archive_debug,
                &fixture.key_pem,
                "private-wrong-password",
                "wrapper-test-password",
            ],
        );
    }
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn malformed_tls_credentials_and_alpn_fail_without_network_or_secret_disclosure() {
    support::capture::start();
    let fixture = fixture::Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls"))
            || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mqtt5 in [false, true] {
            let malformed = if backend == TlsBackend::Rustls {
                vec![
                    TlsClientIdentity::RustlsPem {
                        certificate: b"private-invalid-certificate".as_slice().into(),
                        private_key: SecretBytes::new(fixture.key_pem.as_bytes().to_vec()),
                    },
                    TlsClientIdentity::RustlsPem {
                        certificate: fixture.pem.clone().into(),
                        private_key: SecretBytes::new(b"private-invalid-key".to_vec()),
                    },
                ]
            } else {
                vec![
                    TlsClientIdentity::NativePkcs12 {
                        identity: SecretBytes::new(b"private-invalid-archive".to_vec()),
                        password: SecretBytes::new(b"private-archive-password".to_vec()),
                    },
                    TlsClientIdentity::NativePkcs12 {
                        identity: SecretBytes::new(b"private-invalid-archive".to_vec()),
                        password: SecretBytes::new(vec![255]),
                    },
                ]
            };
            for identity in malformed {
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    identity: Some(identity),
                    ..fixture.client(backend)
                });
                let debug = format!("{config:?}");
                let error = NativeClient::start(config).unwrap_err();
                assert_eq!(error.kind(), ErrorKind::Tls);
                support::capture::assert_redacted(
                    &format!("{debug} {error:?} {error}"),
                    &[
                        &fixture.key_pem,
                        "private-invalid-certificate",
                        "private-invalid-key",
                        "private-invalid-archive",
                        "private-archive-password",
                    ],
                );
            }
            for alpn in [vec![], vec![b'a'; 256]] {
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    alpn_protocols: vec![alpn],
                    ..fixture.client(backend)
                });
                assert_eq!(
                    NativeClient::start(config).unwrap_err().kind(),
                    ErrorKind::Configuration
                );
            }
            if backend == TlsBackend::Native {
                let mut config = support::config(mqtt5, listener.local_addr().unwrap().port());
                config.common.transport = TransportConfig::Tls(TlsConfig {
                    alpn_protocols: vec![vec![255]],
                    ..fixture.client(backend)
                });
                assert_eq!(
                    NativeClient::start(config).unwrap_err().kind(),
                    ErrorKind::Configuration
                );
            }
        }
    }
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(target_os = "linux")]
#[test]
fn platform_roots_validate_an_isolated_process_trust_store() {
    let fixture = fixture::Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let roots = directory.path().join("roots.pem");
    std::fs::write(&roots, &fixture.pem).unwrap();
    for backend in ["rustls", "native"] {
        if (backend == "rustls" && !cfg!(feature = "use-rustls"))
            || (backend == "native" && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mqtt5 in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = fixture.server.clone();
            let broker = support::Broker::spawn(move || {
                let mut stream = fixture::wrap(Box::new(support::accept(&listener)), server);
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
            });
            let output = support::process_output(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "platform_roots_child", "--nocapture"])
                    .env("SSL_CERT_FILE", &roots)
                    .env("SSL_CERT_DIR", directory.path())
                    .env("RUMQTTC_ROOTS_PORT", port.to_string())
                    .env("RUMQTTC_ROOTS_BACKEND", backend)
                    .env("RUMQTTC_ROOTS_MQTT5", mqtt5.to_string()),
            );
            assert!(
                output.status.success(),
                "platform-root subprocess: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            broker.join();
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn platform_roots_child() {
    let Ok(port) = std::env::var("RUMQTTC_ROOTS_PORT") else {
        return;
    };
    let mqtt5 = std::env::var("RUMQTTC_ROOTS_MQTT5").unwrap() == "true";
    let mut config = support::config(mqtt5, port.parse().unwrap());
    config.common.transport = TransportConfig::Tls(TlsConfig {
        backend: if std::env::var("RUMQTTC_ROOTS_BACKEND").unwrap() == "rustls" {
            TlsBackend::Rustls
        } else {
            TlsBackend::Native
        },
        roots: TlsRootPolicy::Platform,
        ..Default::default()
    });
    let mut client = NativeClient::start(config).unwrap();
    let _events = support::connected(&mut client);
    client.closer().close(support::DEADLINE).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_roots_return_startup_errors_inside_async_callers() {
    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls"))
            || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mut config in [
            ClientConfig::v4("invalid-roots", "localhost", 8883),
            ClientConfig::v5("invalid-roots", "localhost", 8883),
        ] {
            config.common.transport = TransportConfig::Tls(TlsConfig {
                backend,
                roots: TlsRootPolicy::Pem(Bytes::from_static(b"not a PEM certificate")),
                ..Default::default()
            });
            config.validate().unwrap();
            NativeClient::start(config).expect_err("malformed roots must fail during startup");
        }
    }
}

fn read_mqtt(stream: &mut impl Read) -> Vec<u8> {
    let mut header = [0; 2];
    stream.read_exact(&mut header).unwrap();
    assert!(header[1] < 128);
    let mut packet = vec![0; usize::from(header[1]) + 2];
    packet[..2].copy_from_slice(&header);
    stream.read_exact(&mut packet[2..]).unwrap();
    packet
}

#[cfg(all(target_os = "linux", feature = "use-native-tls"))]
fn native_identity(certificate: &str, key: &str) -> TlsClientIdentity {
    // OpenSSL is supplied by the Linux native-TLS CI job. Generate fresh test
    // credentials instead of checking in a time-limited PKCS#12 fixture.
    let directory = tempfile::tempdir().unwrap();
    let certificate_path = directory.path().join("certificate.pem");
    let key_path = directory.path().join("key.pem");
    let identity_path = directory.path().join("identity.p12");
    std::fs::write(&certificate_path, certificate).unwrap();
    std::fs::write(&key_path, key).unwrap();
    let output = std::process::Command::new("openssl")
        .args([
            "pkcs12",
            "-export",
            "-passout",
            "pass:wrapper-test-password",
            "-in",
        ])
        .arg(certificate_path)
        .arg("-inkey")
        .arg(key_path)
        .arg("-out")
        .arg(&identity_path)
        .output()
        .expect("Linux native-TLS tests require openssl");
    assert!(
        output.status.success(),
        "PKCS#12 test identity generation failed"
    );
    TlsClientIdentity::NativePkcs12 {
        identity: SecretBytes::new(std::fs::read(identity_path).unwrap()),
        password: SecretBytes::new(b"wrapper-test-password".to_vec()),
    }
}

#[test]
#[expect(
    clippy::result_large_err,
    reason = "tungstenite fixes the handshake callback's error type"
)]
fn tls_and_wss_enforce_roots_hostname_identity_and_alpn() {
    let now = SystemTime::now();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "wrapper test CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = (now - Duration::from_secs(86400)).into();
    params.not_after = (now + Duration::from_secs(86400 * 30)).into();
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.not_before = (now - Duration::from_secs(86400)).into();
    params.not_after = (now + Duration::from_secs(86400 * 30)).into();
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key = KeyPair::generate().unwrap();
    let certificate = params.signed_by(&key, &ca).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    let roots = Arc::new(roots);

    for backend in [TlsBackend::Rustls, TlsBackend::Native] {
        if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls"))
            || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
        {
            continue;
        }
        for mqtt5 in [false, true] {
            for websocket in [false, true] {
                if websocket && !cfg!(feature = "websocket") {
                    continue;
                }
                // Success, an untrusted platform-root connection, and a hostname mismatch.
                for failure in [None, Some("roots"), Some("hostname")] {
                    let mutual = failure.is_none()
                        && (backend == TlsBackend::Rustls
                            || cfg!(all(target_os = "linux", feature = "use-native-tls")));
                    let verifier = rustls::server::WebPkiClientVerifier::builder(roots.clone())
                        .build()
                        .unwrap();
                    let builder = rustls::ServerConfig::builder();
                    let builder = if mutual {
                        builder.with_client_cert_verifier(verifier)
                    } else {
                        builder.with_no_client_auth()
                    };
                    let mut server = builder
                        .with_single_cert(
                            vec![certificate.der().clone()],
                            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
                        )
                        .unwrap();
                    server.alpn_protocols = vec![b"mqtt".to_vec()];
                    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let broker = std::thread::spawn(move || {
                        let (socket, _) = listener.accept().unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut stream = rustls::StreamOwned::new(
                            rustls::ServerConnection::new(Arc::new(server)).unwrap(),
                            socket,
                        );
                        if failure.is_some() {
                            let mut byte = [0];
                            assert!(stream.read(&mut byte).is_err());
                            return;
                        }
                        if websocket {
                            let mut stream = tungstenite::accept_hdr(stream, |_: &tungstenite::handshake::server::Request, mut response: tungstenite::handshake::server::Response| {
                                response.headers_mut().insert("sec-websocket-protocol", "mqtt".parse().unwrap());
                                Ok(response)
                            }).unwrap();
                            assert_eq!(stream.read().unwrap().into_data()[0], 0x10);
                            assert_eq!(
                                stream.get_ref().conn.alpn_protocol(),
                                Some(b"mqtt".as_slice())
                            );
                            if mutual {
                                assert!(stream.get_ref().conn.peer_certificates().is_some());
                            }
                            stream
                                .send(tungstenite::Message::Binary(Bytes::from_static(if mqtt5 {
                                    &[0x20, 3, 0, 0, 0]
                                } else {
                                    &[0x20, 2, 0, 0]
                                })))
                                .unwrap();
                            assert_eq!(stream.read().unwrap().into_data()[0], 0xe0);
                        } else {
                            assert_eq!(read_mqtt(&mut stream)[0], 0x10);
                            assert_eq!(stream.conn.alpn_protocol(), Some(b"mqtt".as_slice()));
                            if mutual {
                                assert!(stream.conn.peer_certificates().is_some());
                            }
                            stream
                                .write_all(if mqtt5 {
                                    &[0x20, 3, 0, 0, 0]
                                } else {
                                    &[0x20, 2, 0, 0]
                                })
                                .unwrap();
                            stream.flush().unwrap();
                            assert_eq!(read_mqtt(&mut stream)[0], 0xe0);
                        }
                    });
                    let host = if failure == Some("hostname") {
                        "localhost"
                    } else {
                        "127.0.0.1"
                    };
                    let mut config = if mqtt5 {
                        ClientConfig::v5("tls", host, port)
                    } else {
                        ClientConfig::v4("tls", host, port)
                    };
                    let tls = TlsConfig {
                        backend,
                        roots: if failure == Some("roots") {
                            TlsRootPolicy::Platform
                        } else {
                            TlsRootPolicy::Pem(Bytes::from(ca.pem()))
                        },
                        identity: mutual.then(|| {
                            #[cfg(all(target_os = "linux", feature = "use-native-tls"))]
                            if backend == TlsBackend::Native {
                                return native_identity(&certificate.pem(), &key.serialize_pem());
                            }
                            TlsClientIdentity::RustlsPem {
                                certificate: Bytes::from(certificate.pem()),
                                private_key: SecretBytes::new(key.serialize_pem().into_bytes()),
                            }
                        }),
                        alpn_protocols: vec![b"mqtt".to_vec()],
                    };
                    config.common.transport = if websocket {
                        config.common.broker = BrokerTarget::WebSocket {
                            url: format!("wss://{host}:{port}/mqtt"),
                        };
                        TransportConfig::Wss(tls)
                    } else {
                        TransportConfig::Tls(tls)
                    };
                    let mut native = NativeClient::start(config).unwrap();
                    let mut events = native.take_events().unwrap();
                    let event = events
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap()
                        .unwrap();
                    if failure.is_some() {
                        let WrapperEvent::Disconnected { error, .. } = event else {
                            panic!("expected TLS failure")
                        };
                        assert_eq!(error.kind(), ErrorKind::Tls);
                    } else {
                        assert!(
                            matches!(event, WrapperEvent::Connected { .. }),
                            "{backend:?} mqtt5={mqtt5} websocket={websocket}: {event:?}"
                        );
                    }
                    native.closer().close_now(Duration::from_secs(3)).unwrap();
                    broker.join().unwrap();
                }
            }
        }
    }
}
