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
