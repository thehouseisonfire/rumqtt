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
use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
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

async fn send_packet(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    packet: Packet,
) {
    ws.send(Message::Binary(encode_packet(packet).into()))
        .await
        .unwrap();
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

            let mut websocket = accept_hdr_async(
                stream,
                |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
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
                },
            )
            .await
            .unwrap();

            let mut should_close_connection = false;

            while let Some(frame) = websocket.next().await {
                let message = match frame {
                    Ok(message) => message,
                    Err(_) => break,
                };

                match message {
                    Message::Binary(data) => {
                        let mut bytes = BytesMut::from(data.as_ref());

                        while !bytes.is_empty() {
                            let packet = Packet::read(&mut bytes, MAX_PACKET_SIZE).unwrap();
                            match packet {
                                Packet::Connect(_) => {
                                    let connack = Packet::ConnAck(ConnAck::new(
                                        ConnectReturnCode::Success,
                                        false,
                                    ));
                                    send_packet(&mut websocket, connack).await;
                                }
                                Packet::Publish(_) => {
                                    let current =
                                        server_received_count.fetch_add(1, Ordering::SeqCst) + 1;

                                    if this_connection == 1 && current == FIRST_CONNECTION_MESSAGES
                                    {
                                        websocket.close(None).await.unwrap();
                                        should_close_connection = true;
                                        break;
                                    }
                                }
                                Packet::PingReq => {
                                    send_packet(&mut websocket, Packet::PingResp).await;
                                }
                                Packet::Disconnect => break,
                                _ => {}
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Text(_) => {}
                    Message::Frame(_) => {}
                }

                if should_close_connection {
                    break;
                }
            }

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
