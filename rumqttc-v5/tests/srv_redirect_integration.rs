#![cfg(any(feature = "http-proxy", feature = "socks-proxy"))]

use bytes::BytesMut;
use rumqttc::mqttbytes::v5::{ConnAck, ConnAckProperties, ConnectReturnCode, Packet};
use rumqttc::{
    ConnectionError, Event, EventLoop, MqttOptions, Proxy, RedirectDecision, RedirectFailure,
    RedirectPolicy, RedirectTargetProfile, SrvRecord, SrvResolver, Transport,
};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const OWNER: &str = "_mqtt._tcp.redirect.test";
const ORIGIN: &str = "origin.redirect.test";
const FIRST: &str = "first.target.test";
const SECOND: &str = "second.target.test";
const SECRET: &str = "proxy-secret";

fn connack(code: ConnectReturnCode, reference: Option<&str>) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    ConnAck {
        session_present: false,
        code,
        properties: (reference.is_some() || code == ConnectReturnCode::Success).then(|| {
            ConnAckProperties {
                session_expiry_interval: None,
                receive_max: None,
                max_qos: None,
                retain_available: None,
                max_packet_size: None,
                assigned_client_identifier: (code == ConnectReturnCode::Success)
                    .then(|| "redirect-assigned-client".to_owned()),
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
            }
        }),
    }
    .write(&mut encoded)
    .unwrap();
    encoded.to_vec()
}

async fn read_mqtt_packet<S>(stream: &mut S)
where
    S: AsyncRead + Unpin,
{
    let first = stream.read_u8().await.unwrap();
    assert_eq!(first >> 4, 1, "proxy tunnel must receive MQTT CONNECT");
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

fn options(proxy: Proxy, records: Vec<SrvRecord>) -> MqttOptions {
    let policy = RedirectPolicy::new(NonZeroUsize::new(1).unwrap(), |context| {
        RedirectDecision::follow(
            RedirectTargetProfile::isolated(context.references[0].clone(), Transport::tcp())
                .unwrap()
                .reuse_network_credentials(),
        )
    });
    let resolver = SrvResolver::new(move |owner| {
        assert_eq!(owner, format!("{OWNER}."));
        let records = records.clone();
        async move { Ok(records) }
    });
    let mut options = MqttOptions::new("srv-proxy", ORIGIN);
    options
        .set_connect_timeout(Duration::from_secs(2))
        .set_redirect_policy(policy)
        .set_srv_resolver(resolver)
        .set_proxy(proxy);
    options
}

async fn drive_redirect(eventloop: &mut EventLoop) {
    loop {
        match timeout(Duration::from_secs(2), eventloop.poll())
            .await
            .expect("event loop stalled")
            .unwrap()
        {
            Event::Redirect(_) => return,
            Event::Incoming(Packet::ConnAck(_)) | Event::Outgoing(_) => {}
            other => panic!("unexpected event before redirect: {other:?}"),
        }
    }
}

fn records() -> Vec<SrvRecord> {
    vec![
        SrvRecord {
            priority: 10,
            weight: 0,
            port: 4111,
            target: format!("{FIRST}."),
        },
        SrvRecord {
            priority: 20,
            weight: 0,
            port: 5222,
            target: SECOND.to_owned(),
        },
    ]
}

#[cfg(feature = "http-proxy")]
async fn read_http_target(stream: &mut TcpStream) -> (String, bool) {
    let mut request = Vec::new();
    loop {
        let byte = stream.read_u8().await.unwrap();
        request.push(byte);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8(request).unwrap();
    let target = request
        .lines()
        .next()
        .unwrap()
        .strip_prefix("CONNECT ")
        .unwrap()
        .strip_suffix(" HTTP/1.1")
        .unwrap()
        .to_owned();
    let has_auth = request.contains("Proxy-Authorization: Basic ");
    (target, has_auth)
}

#[cfg(feature = "http-proxy")]
async fn spawn_http_proxy(
    reject_candidates: bool,
) -> (
    u16,
    Arc<Mutex<Vec<(String, bool)>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (target, has_auth) = read_http_target(&mut stream).await;
            captured.lock().unwrap().push((target.clone(), has_auth));
            if target == format!("{ORIGIN}:1883") {
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                read_mqtt_packet(&mut stream).await;
                stream
                    .write_all(&connack(ConnectReturnCode::UseAnotherServer, Some(OWNER)))
                    .await
                    .unwrap();
            } else if target == format!("{FIRST}:4111") || reject_candidates {
                stream
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await
                    .unwrap();
            } else {
                assert_eq!(target, format!("{SECOND}:5222"));
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                read_mqtt_packet(&mut stream).await;
                stream
                    .write_all(&connack(ConnectReturnCode::Success, None))
                    .await
                    .unwrap();
            }
        }
    });
    (port, requests, task)
}

#[cfg(feature = "http-proxy")]
#[tokio::test]
async fn http_proxy_uses_srv_candidates_and_retries_setup_failures() {
    let (port, requests, proxy) = spawn_http_proxy(false).await;
    let mut eventloop = EventLoop::new(
        options(
            Proxy::http("127.0.0.1", port).with_credentials("proxy-user", SECRET),
            records(),
        ),
        10,
    );
    drive_redirect(&mut eventloop).await;
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    proxy.await.unwrap();
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            (format!("{ORIGIN}:1883"), true),
            (format!("{FIRST}:4111"), true),
            (format!("{SECOND}:5222"), true),
        ]
    );
}

#[cfg(feature = "http-proxy")]
#[tokio::test]
async fn http_proxy_exhaustion_is_structured_and_redacted() {
    let (port, requests, proxy) = spawn_http_proxy(true).await;
    let mut eventloop = EventLoop::new(
        options(
            Proxy::http("127.0.0.1", port).with_credentials("proxy-user", SECRET),
            records(),
        ),
        10,
    );
    drive_redirect(&mut eventloop).await;
    let error = eventloop.poll().await.unwrap_err();
    assert!(matches!(
        error,
        ConnectionError::Redirect(ref redirect)
            if matches!(redirect.failure, RedirectFailure::SrvTargetsExhausted { attempted: 2, .. })
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(SECRET));
    assert!(!diagnostic.contains("proxy-user"));
    proxy.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 3);
}

#[cfg(feature = "socks-proxy")]
async fn read_socks_target(stream: &mut TcpStream) -> (String, bool) {
    assert_eq!(stream.read_u8().await.unwrap(), 5);
    let count = stream.read_u8().await.unwrap() as usize;
    let mut methods = vec![0; count];
    stream.read_exact(&mut methods).await.unwrap();
    let authenticated = methods.contains(&2);
    if authenticated {
        stream.write_all(&[5, 2]).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        let username_len = stream.read_u8().await.unwrap() as usize;
        let mut username = vec![0; username_len];
        stream.read_exact(&mut username).await.unwrap();
        let password_len = stream.read_u8().await.unwrap() as usize;
        let mut password = vec![0; password_len];
        stream.read_exact(&mut password).await.unwrap();
        assert_eq!(username, b"proxy-user");
        assert_eq!(password, SECRET.as_bytes());
        stream.write_all(&[1, 0]).await.unwrap();
    } else {
        stream.write_all(&[5, 0]).await.unwrap();
    }
    assert_eq!(stream.read_u8().await.unwrap(), 5);
    assert_eq!(stream.read_u8().await.unwrap(), 1);
    assert_eq!(stream.read_u8().await.unwrap(), 0);
    assert_eq!(stream.read_u8().await.unwrap(), 3);
    let len = stream.read_u8().await.unwrap() as usize;
    let mut host = vec![0; len];
    stream.read_exact(&mut host).await.unwrap();
    let port = stream.read_u16().await.unwrap();
    (
        format!("{}:{port}", String::from_utf8(host).unwrap()),
        authenticated,
    )
}

#[cfg(feature = "socks-proxy")]
async fn spawn_socks_proxy(
    reject_candidates: bool,
) -> (
    u16,
    Arc<Mutex<Vec<(String, bool)>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (target, authenticated) = read_socks_target(&mut stream).await;
            captured
                .lock()
                .unwrap()
                .push((target.clone(), authenticated));
            let reject = target == format!("{FIRST}:4111")
                || (reject_candidates && target != format!("{ORIGIN}:1883"));
            let reply = if reject { 5 } else { 0 };
            stream
                .write_all(&[5, reply, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            if reject {
                continue;
            }
            read_mqtt_packet(&mut stream).await;
            let response = if target == format!("{ORIGIN}:1883") {
                connack(ConnectReturnCode::UseAnotherServer, Some(OWNER))
            } else {
                assert_eq!(target, format!("{SECOND}:5222"));
                connack(ConnectReturnCode::Success, None)
            };
            stream.write_all(&response).await.unwrap();
        }
    });
    (port, requests, task)
}

#[cfg(feature = "socks-proxy")]
#[tokio::test]
async fn socks_proxy_uses_srv_candidates_and_retries_setup_failures() {
    let (port, requests, proxy) = spawn_socks_proxy(false).await;
    let mut eventloop = EventLoop::new(
        options(
            Proxy::socks5("127.0.0.1", port).with_credentials("proxy-user", SECRET),
            records(),
        ),
        10,
    );
    drive_redirect(&mut eventloop).await;
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    proxy.await.unwrap();
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            (format!("{ORIGIN}:1883"), true),
            (format!("{FIRST}:4111"), true),
            (format!("{SECOND}:5222"), true),
        ]
    );
}

#[cfg(feature = "socks-proxy")]
#[tokio::test]
async fn socks_proxy_exhaustion_is_structured_and_redacted() {
    let (port, requests, proxy) = spawn_socks_proxy(true).await;
    let mut eventloop = EventLoop::new(
        options(
            Proxy::socks5("127.0.0.1", port).with_credentials("proxy-user", SECRET),
            records(),
        ),
        10,
    );
    drive_redirect(&mut eventloop).await;
    let error = eventloop.poll().await.unwrap_err();
    assert!(matches!(
        error,
        ConnectionError::Redirect(ref redirect)
            if matches!(redirect.failure, RedirectFailure::SrvTargetsExhausted { attempted: 2, .. })
    ));
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(SECRET));
    assert!(!diagnostic.contains("proxy-user"));
    proxy.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 3);
}
