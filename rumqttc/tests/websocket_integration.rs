#![cfg(feature = "websocket")]

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use rumqttc::mqttbytes::v4::{ConnAck, ConnectReturnCode, Packet};
use rumqttc::{AsyncClient, MqttOptions, QoS, Transport};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
    },
};

const MAX_PACKET_SIZE: usize = 1024 * 1024;
const TOTAL_MESSAGES: usize = 100;
const FIRST_CONNECTION_MESSAGES: usize = 20;

fn encode_packet(packet: Packet) -> Vec<u8> {
    let mut buffer = BytesMut::with_capacity(128);
    packet.write(&mut buffer, MAX_PACKET_SIZE).unwrap();
    buffer.to_vec()
}

fn websocket_handshake_callback(
    request: &Request,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    let protocol = request
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(protocol.contains("mqtt"));

    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
    Ok(response)
}

async fn send_packet(
    ws: &mut tokio_tungstenite::WebSocketStream<impl AsyncRead + AsyncWrite + Unpin>,
    packet: Packet,
) {
    ws.send(Message::Binary(encode_packet(packet).into()))
        .await
        .unwrap();
}

async fn process_websocket_connection(
    websocket: &mut tokio_tungstenite::WebSocketStream<impl AsyncRead + AsyncWrite + Unpin>,
    publish_counter: &AtomicUsize,
    close_after_publish_count: Option<usize>,
) {
    while let Some(frame) = websocket.next().await {
        let message = match frame {
            Ok(message) => message,
            Err(_) => break,
        };

        match message {
            Message::Binary(data) => {
                let mut bytes = BytesMut::from(data.as_ref());
                while !bytes.is_empty() {
                    match Packet::read(&mut bytes, MAX_PACKET_SIZE).unwrap() {
                        Packet::Connect(_) => {
                            send_packet(
                                websocket,
                                Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)),
                            )
                            .await;
                        }
                        Packet::Publish(_) => {
                            let current = publish_counter.fetch_add(1, Ordering::SeqCst) + 1;
                            if close_after_publish_count == Some(current) {
                                websocket.close(None).await.unwrap();
                                return;
                            }
                        }
                        Packet::PingReq => {
                            send_packet(websocket, Packet::PingResp).await;
                        }
                        Packet::Disconnect => return,
                        _ => {}
                    }
                }
            }
            Message::Close(_) => return,
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Text(_) => {}
            Message::Frame(_) => {}
        }
    }
}

async fn publish_with_retry(client: &AsyncClient, payload: [u8; 2]) {
    loop {
        let publish_result = client
            .publish("ws/migration", QoS::AtMostOnce, false, payload.to_vec())
            .await;

        if publish_result.is_ok() {
            break;
        }

        sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_at_least(counter: &AtomicUsize, value: usize, timeout_duration: Duration) {
    timeout(timeout_duration, async {
        while counter.load(Ordering::SeqCst) < value {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn websocket_client_reconnects_and_delivers_all_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let connection_count = Arc::new(AtomicUsize::new(0));
    let received_count = Arc::new(AtomicUsize::new(0));

    let server_connection_count = Arc::clone(&connection_count);
    let server_received_count = Arc::clone(&received_count);

    let server_task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let this_connection = server_connection_count.fetch_add(1, Ordering::SeqCst) + 1;

            let mut websocket = accept_hdr_async(stream, websocket_handshake_callback)
                .await
                .unwrap();
            let close_after_publish_count =
                (this_connection == 1).then_some(FIRST_CONNECTION_MESSAGES);
            process_websocket_connection(
                &mut websocket,
                &server_received_count,
                close_after_publish_count,
            )
            .await;

            if server_received_count.load(Ordering::SeqCst) >= TOTAL_MESSAGES {
                break;
            }
        }
    });

    let mut mqtt_options = MqttOptions::new(
        "ws-adapter-test",
        format!("ws://127.0.0.1:{port}/mqtt"),
        port,
    );
    mqtt_options.set_transport(Transport::Ws);
    mqtt_options.set_keep_alive(2);

    let (client, mut eventloop) = AsyncClient::new(mqtt_options, 100);
    let eventloop_task = tokio::spawn(async move {
        loop {
            let _ = eventloop.poll().await;
        }
    });

    for index in 0..FIRST_CONNECTION_MESSAGES {
        publish_with_retry(&client, [0xAB, index as u8]).await;
    }

    wait_for_at_least(
        &received_count,
        FIRST_CONNECTION_MESSAGES,
        Duration::from_secs(10),
    )
    .await;

    wait_for_at_least(&connection_count, 2, Duration::from_secs(10)).await;

    for index in FIRST_CONNECTION_MESSAGES..TOTAL_MESSAGES {
        publish_with_retry(&client, [0xAB, index as u8]).await;
    }

    wait_for_at_least(&received_count, TOTAL_MESSAGES, Duration::from_secs(10)).await;

    eventloop_task.abort();
    let _ = eventloop_task.await;

    server_task.await.unwrap();

    assert_eq!(received_count.load(Ordering::SeqCst), TOTAL_MESSAGES);
    assert!(connection_count.load(Ordering::SeqCst) >= 2);
}

#[cfg(feature = "use-rustls-no-provider")]
fn make_wss_acceptor_and_ca() -> (TlsAcceptor, Vec<u8>) {
    let certified_key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.signing_key.serialize_pem();

    let cert_chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let private_key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .unwrap();

    (
        TlsAcceptor::from(Arc::new(server_config)),
        cert_pem.into_bytes(),
    )
}

#[cfg(feature = "use-rustls-no-provider")]
#[tokio::test]
async fn wss_client_publishes_over_tls_websocket() {
    let (tls_acceptor, cert_pem) = make_wss_acceptor_and_ca();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let publish_count = Arc::new(AtomicUsize::new(0));
    let server_publish_count = Arc::clone(&publish_count);

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let tls_stream = tls_acceptor.accept(stream).await.unwrap();

        let mut websocket = accept_hdr_async(tls_stream, websocket_handshake_callback)
            .await
            .unwrap();
        process_websocket_connection(&mut websocket, &server_publish_count, None).await;
    });

    let mut mqtt_options = MqttOptions::new(
        "wss-adapter-test",
        format!("wss://localhost:{port}/mqtt"),
        port,
    );
    mqtt_options.set_transport(Transport::wss(cert_pem, None, None));
    mqtt_options.set_keep_alive(2);

    let (client, mut eventloop) = AsyncClient::new(mqtt_options, 20);
    let eventloop_task = tokio::spawn(async move {
        loop {
            let _ = eventloop.poll().await;
        }
    });

    for index in 0..10 {
        publish_with_retry(&client, [0xCD, index]).await;
    }

    wait_for_at_least(&publish_count, 10, Duration::from_secs(10)).await;

    eventloop_task.abort();
    let _ = eventloop_task.await;
    server_task.await.unwrap();

    assert_eq!(publish_count.load(Ordering::SeqCst), 10);
}

#[cfg(feature = "use-rustls-no-provider")]
#[tokio::test]
async fn wss_client_reconnects_and_delivers_all_messages() {
    let (tls_acceptor, cert_pem) = make_wss_acceptor_and_ca();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let connection_count = Arc::new(AtomicUsize::new(0));
    let received_count = Arc::new(AtomicUsize::new(0));

    let server_connection_count = Arc::clone(&connection_count);
    let server_received_count = Arc::clone(&received_count);

    let server_task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let this_connection = server_connection_count.fetch_add(1, Ordering::SeqCst) + 1;
            let tls_stream = tls_acceptor.accept(stream).await.unwrap();
            let mut websocket = accept_hdr_async(tls_stream, websocket_handshake_callback)
                .await
                .unwrap();

            let close_after_publish_count =
                (this_connection == 1).then_some(FIRST_CONNECTION_MESSAGES);
            process_websocket_connection(
                &mut websocket,
                &server_received_count,
                close_after_publish_count,
            )
            .await;

            if server_received_count.load(Ordering::SeqCst) >= TOTAL_MESSAGES {
                break;
            }
        }
    });

    let mut mqtt_options = MqttOptions::new(
        "wss-adapter-reconnect-test",
        format!("wss://localhost:{port}/mqtt"),
        port,
    );
    mqtt_options.set_transport(Transport::wss(cert_pem, None, None));
    mqtt_options.set_keep_alive(2);

    let (client, mut eventloop) = AsyncClient::new(mqtt_options, 100);
    let eventloop_task = tokio::spawn(async move {
        loop {
            let _ = eventloop.poll().await;
        }
    });

    for index in 0..FIRST_CONNECTION_MESSAGES {
        publish_with_retry(&client, [0xEF, index as u8]).await;
    }

    wait_for_at_least(
        &received_count,
        FIRST_CONNECTION_MESSAGES,
        Duration::from_secs(10),
    )
    .await;

    wait_for_at_least(&connection_count, 2, Duration::from_secs(10)).await;

    for index in FIRST_CONNECTION_MESSAGES..TOTAL_MESSAGES {
        publish_with_retry(&client, [0xEF, index as u8]).await;
    }

    wait_for_at_least(&received_count, TOTAL_MESSAGES, Duration::from_secs(10)).await;

    eventloop_task.abort();
    let _ = eventloop_task.await;

    server_task.await.unwrap();

    assert_eq!(received_count.load(Ordering::SeqCst), TOTAL_MESSAGES);
    assert!(connection_count.load(Ordering::SeqCst) >= 2);
}
