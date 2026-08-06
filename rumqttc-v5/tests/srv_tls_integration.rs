#![cfg(any(
    feature = "use-rustls-ring",
    feature = "use-rustls-aws-lc",
    feature = "use-native-tls"
))]

use bytes::BytesMut;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
#[cfg(feature = "use-native-tls")]
use rumqttc::TlsConfiguration;
use rumqttc::mqttbytes::v5::{ConnAck, ConnAckProperties, ConnectReturnCode, Packet};
use rumqttc::{
    ConnectionError, Event, EventLoop, MqttOptions, RedirectDecision, RedirectFailure,
    RedirectPolicy, RedirectTargetProfile, SrvRecord, SrvResolver, Transport,
};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

#[cfg(feature = "use-native-tls")]
use tokio_native_tls::{TlsAcceptor as NativeTlsAcceptor, native_tls::Identity};
#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
use tokio_rustls::{
    TlsAcceptor as RustlsTlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

const OWNER: &str = "_mqtts._tcp.redirect.test";
const ORIGIN: &str = "origin.redirect.test";
const FIRST: &str = "first.secure.test";
const SECOND: &str = "second.secure.test";
const SECRET: &str = "mqtt-secret";

struct Certificates {
    ca_pem: Vec<u8>,
    owner_cert_pem: Vec<u8>,
    owner_key_pem: Vec<u8>,
    target_cert_pem: Vec<u8>,
    target_key_pem: Vec<u8>,
}

fn certificates() -> Certificates {
    let now = SystemTime::now();
    let not_before = (now - Duration::from_secs(86_400)).into();
    let not_after = (now + Duration::from_secs(31_536_000)).into();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "SRV redirect test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();

    let issue = |name: &str| {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
        params.distinguished_name.push(DnType::CommonName, name);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = not_before;
        params.not_after = not_after;
        let cert = params.signed_by(&key, &ca).unwrap();
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
    };
    let (owner_cert_pem, owner_key_pem) = issue(OWNER);
    let (target_cert_pem, target_key_pem) = issue(SECOND);
    Certificates {
        ca_pem: ca.pem().into_bytes(),
        owner_cert_pem,
        owner_key_pem,
        target_cert_pem,
        target_key_pem,
    }
}

fn connack(code: ConnectReturnCode, reference: Option<&str>) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    ConnAck {
        session_present: false,
        code,
        properties: Some(ConnAckProperties {
            session_expiry_interval: None,
            receive_max: None,
            max_qos: None,
            retain_available: None,
            max_packet_size: None,
            assigned_client_identifier: (code == ConnectReturnCode::Success)
                .then(|| "tls-assigned-client".to_owned()),
            topic_alias_max: None,
            reason_string: None,
            user_properties: Vec::new(),
            wildcard_subscription_available: None,
            subscription_identifiers_available: None,
            shared_subscription_available: None,
            server_keep_alive: None,
            response_information: None,
            server_reference: reference.map(str::to_owned),
            authentication_method: None,
            authentication_data: None,
        }),
    }
    .write(&mut encoded)
    .unwrap();
    encoded.to_vec()
}

async fn read_connect<S: AsyncRead + Unpin>(stream: &mut S) {
    assert_eq!(stream.read_u8().await.unwrap() >> 4, 1);
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    loop {
        let byte = stream.read_u8().await.unwrap();
        remaining += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; remaining];
    stream.read_exact(&mut body).await.unwrap();
}

async fn spawn_origin() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_connect(&mut stream).await;
        stream
            .write_all(&connack(ConnectReturnCode::UseAnotherServer, Some(OWNER)))
            .await
            .unwrap();
    });
    (port, task)
}

async fn drive_redirect(eventloop: &mut EventLoop) {
    loop {
        match timeout(Duration::from_secs(3), eventloop.poll())
            .await
            .expect("event loop stalled")
            .unwrap()
        {
            Event::Redirect(_) => return,
            Event::Outgoing(_) | Event::Incoming(Packet::ConnAck(_)) => {}
            event => panic!("unexpected event before redirect: {event:?}"),
        }
    }
}

fn options(
    transport: Transport,
    origin_port: u16,
    target_ports: &[(String, u16)],
    attempts: Arc<Mutex<Vec<String>>>,
) -> MqttOptions {
    let records = target_ports
        .iter()
        .enumerate()
        .map(|(index, (target, port))| SrvRecord {
            priority: (index as u16 + 1) * 10,
            weight: 0,
            port: *port,
            target: target.clone(),
        })
        .collect::<Vec<_>>();
    let policy = RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), move |context| {
        RedirectDecision::follow(
            RedirectTargetProfile::isolated(context.references[0].clone(), transport.clone())
                .unwrap(),
        )
    });
    let ports = target_ports.iter().cloned().collect::<HashMap<_, _>>();
    let mut options = MqttOptions::new("tls-srv", ORIGIN);
    options
        .set_credentials("mqtt-user", SECRET)
        .set_connect_timeout(Duration::from_secs(2))
        .set_redirect_policy(policy)
        .set_srv_resolver(SrvResolver::new(move |_| {
            let records = records.clone();
            async move { Ok(records) }
        }))
        .set_socket_connector(move |endpoint, _| {
            attempts.lock().unwrap().push(endpoint.clone());
            let port = if endpoint == format!("{ORIGIN}:1883") {
                origin_port
            } else {
                let host = endpoint.rsplit_once(':').unwrap().0;
                *ports.get(host).unwrap()
            };
            async move { TcpStream::connect(("127.0.0.1", port)).await }
        });
    options
}

#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
fn install_rustls_provider() {
    #[cfg(feature = "use-rustls-ring")]
    drop(tokio_rustls::rustls::crypto::ring::default_provider().install_default());

    #[cfg(all(not(feature = "use-rustls-ring"), feature = "use-rustls-aws-lc"))]
    drop(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default());
}

#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
fn rustls_acceptor(cert: &[u8], key: &[u8]) -> RustlsTlsAcceptor {
    let chain = CertificateDer::pem_slice_iter(cert)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(key).unwrap();
    RustlsTlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
    ))
}

#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
async fn spawn_rustls_server(
    acceptor: RustlsTlsAcceptor,
    code: ConnectReturnCode,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(mut stream) = acceptor.accept(stream).await {
            read_connect(&mut stream).await;
            stream.write_all(&connack(code, None)).await.unwrap();
        }
    });
    (port, task)
}

#[cfg(feature = "use-native-tls")]
fn native_acceptor(cert: &[u8], key: &[u8]) -> NativeTlsAcceptor {
    let identity = Identity::from_pkcs8(cert, key).unwrap();
    NativeTlsAcceptor::from(tokio_native_tls::native_tls::TlsAcceptor::new(identity).unwrap())
}

#[cfg(feature = "use-native-tls")]
async fn spawn_native_server(
    acceptor: NativeTlsAcceptor,
    code: ConnectReturnCode,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(mut stream) = acceptor.accept(stream).await {
            read_connect(&mut stream).await;
            stream.write_all(&connack(code, None)).await.unwrap();
        }
    });
    (port, task)
}

async fn assert_success(mut eventloop: EventLoop, tasks: Vec<tokio::task::JoinHandle<()>>) {
    drive_redirect(&mut eventloop).await;
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    for task in tasks {
        task.await.unwrap();
    }
}

async fn assert_mqtt_failure(mut eventloop: EventLoop, tasks: Vec<tokio::task::JoinHandle<()>>) {
    drive_redirect(&mut eventloop).await;
    let error = eventloop.poll().await.unwrap_err();
    assert!(matches!(error, ConnectionError::Redirect(ref redirect)
        if matches!(redirect.failure, RedirectFailure::FollowFailed(_))));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(SECRET));
    assert!(!diagnostic.contains("mqtt-user"));
    for task in tasks {
        task.await.unwrap();
    }
}

#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
#[tokio::test]
async fn rustls_authenticates_the_srv_target_and_retries_identity_failure() {
    install_rustls_provider();
    let certs = certificates();
    let (origin_port, origin) = spawn_origin().await;
    let (first_port, first) = spawn_rustls_server(
        rustls_acceptor(&certs.owner_cert_pem, &certs.owner_key_pem),
        ConnectReturnCode::Success,
    )
    .await;
    let (second_port, second) = spawn_rustls_server(
        rustls_acceptor(&certs.target_cert_pem, &certs.target_key_pem),
        ConnectReturnCode::Success,
    )
    .await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let options = options(
        Transport::tls(certs.ca_pem, None, None),
        origin_port,
        &[
            (FIRST.to_owned(), first_port),
            (SECOND.to_owned(), second_port),
        ],
        attempts.clone(),
    );
    assert_success(EventLoop::new(options, 10), vec![origin, first, second]).await;
    assert_eq!(
        *attempts.lock().unwrap(),
        vec![
            format!("{ORIGIN}:1883"),
            format!("{FIRST}:{first_port}"),
            format!("{SECOND}:{second_port}")
        ]
    );
}

#[cfg(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))]
#[tokio::test]
async fn rustls_mqtt_failure_does_not_advance_srv_candidates() {
    install_rustls_provider();
    let certs = certificates();
    let (origin_port, origin) = spawn_origin().await;
    let (first_port, first) = spawn_rustls_server(
        rustls_acceptor(&certs.target_cert_pem, &certs.target_key_pem),
        ConnectReturnCode::NotAuthorized,
    )
    .await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let options = options(
        Transport::tls(certs.ca_pem, None, None),
        origin_port,
        &[
            (SECOND.to_owned(), first_port),
            ("unused.secure.test".to_owned(), 9),
        ],
        attempts.clone(),
    );
    assert_mqtt_failure(EventLoop::new(options, 10), vec![origin, first]).await;
    assert_eq!(attempts.lock().unwrap().len(), 2);
}

#[cfg(feature = "use-native-tls")]
#[tokio::test]
async fn native_tls_authenticates_the_srv_target_and_retries_identity_failure() {
    let certs = certificates();
    let (origin_port, origin) = spawn_origin().await;
    let (first_port, first) = spawn_native_server(
        native_acceptor(&certs.owner_cert_pem, &certs.owner_key_pem),
        ConnectReturnCode::Success,
    )
    .await;
    let (second_port, second) = spawn_native_server(
        native_acceptor(&certs.target_cert_pem, &certs.target_key_pem),
        ConnectReturnCode::Success,
    )
    .await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let options = options(
        Transport::tls_with_config(TlsConfiguration::simple_native(certs.ca_pem, None)),
        origin_port,
        &[
            (FIRST.to_owned(), first_port),
            (SECOND.to_owned(), second_port),
        ],
        attempts.clone(),
    );
    assert_success(EventLoop::new(options, 10), vec![origin, first, second]).await;
    assert_eq!(attempts.lock().unwrap().len(), 3);
}

#[cfg(feature = "use-native-tls")]
#[tokio::test]
async fn native_tls_mqtt_failure_does_not_advance_srv_candidates() {
    let certs = certificates();
    let (origin_port, origin) = spawn_origin().await;
    let (first_port, first) = spawn_native_server(
        native_acceptor(&certs.target_cert_pem, &certs.target_key_pem),
        ConnectReturnCode::NotAuthorized,
    )
    .await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let options = options(
        Transport::tls_with_config(TlsConfiguration::simple_native(certs.ca_pem, None)),
        origin_port,
        &[
            (SECOND.to_owned(), first_port),
            ("unused.secure.test".to_owned(), 9),
        ],
        attempts.clone(),
    );
    assert_mqtt_failure(EventLoop::new(options, 10), vec![origin, first]).await;
    assert_eq!(attempts.lock().unwrap().len(), 2);
}
