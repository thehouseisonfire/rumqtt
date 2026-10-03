use bytes::Bytes;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use rumqttc_wrapper_core::{
    AckMode, AcknowledgementProtocolOptions, ClientConfig, Command, Completion, DeliveryStatus,
    ErrorKind, NativeClient, V5AcknowledgementOptions, WrapperEvent,
};

fn read_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    let header = byte[0];
    let mut length = 0;
    let mut multiplier = 1;
    loop {
        stream.read_exact(&mut byte).unwrap();
        length += usize::from(byte[0] & 127) * multiplier;
        if byte[0] & 128 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (header, body)
}

struct FlushControl {
    pending_ack: AtomicBool,
    entered: mpsc::Sender<()>,
    release: tokio::sync::Semaphore,
    fail: bool,
}

struct ControlledConnector(Arc<FlushControl>);

impl rumqttc_wrapper_core::TransportConnector for ControlledConnector {
    fn connect(
        &self,
        request: rumqttc_wrapper_core::TransportRequest,
    ) -> rumqttc_wrapper_core::TransportFuture {
        let control = Arc::clone(&self.0);
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(request.target)
                .await
                .map_err(|_| rumqttc_wrapper_core::TransportFailure::Connect)?;
            let (read, write) = stream.into_split();
            Ok(rumqttc_wrapper_core::TransportConnection {
                io: Arc::new(ControlledIo {
                    read: Arc::new(tokio::sync::Mutex::new(read)),
                    write: Arc::new(tokio::sync::Mutex::new(write)),
                    control,
                }),
                mode: rumqttc_wrapper_core::TransportMode::Established,
                network_handling: rumqttc_wrapper_core::NetworkHandling::NotApplicable,
            })
        })
    }
}

struct ControlledIo {
    read: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    control: Arc<FlushControl>,
}

impl rumqttc_wrapper_core::TransportIo for ControlledIo {
    fn read(&self, max: usize) -> rumqttc_wrapper_core::TransportIoFuture<Bytes> {
        let read = Arc::clone(&self.read);
        Box::pin(async move {
            let mut bytes = vec![0; max];
            let count = read.lock().await.read(&mut bytes).await?;
            bytes.truncate(count);
            Ok(bytes.into())
        })
    }

    fn write(&self, bytes: Bytes) -> rumqttc_wrapper_core::TransportIoFuture<usize> {
        let write = Arc::clone(&self.write);
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            if matches!(bytes.first(), Some(0x40 | 0x50)) {
                control.pending_ack.store(true, Ordering::Release);
            }
            write.lock().await.write_all(&bytes).await?;
            Ok(bytes.len())
        })
    }

    fn flush(&self) -> rumqttc_wrapper_core::TransportIoFuture<()> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            if control.pending_ack.swap(false, Ordering::AcqRel) {
                control.entered.send(()).unwrap();
                control.release.acquire().await.unwrap().forget();
                if control.fail {
                    return Err(std::io::Error::other("controlled ACK flush failure"));
                }
            }
            Ok(())
        })
    }

    fn shutdown(&self) -> rumqttc_wrapper_core::TransportIoFuture<()> {
        let write = Arc::clone(&self.write);
        Box::pin(async move { write.lock().await.shutdown().await })
    }
}

#[test]
fn acknowledgement_completion_waits_for_flush_and_retains_ambiguous_failures() {
    for fail in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let broker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            read_frame(&mut stream);
            stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
            publish(&mut stream, true, 7);
            assert_eq!(read_frame(&mut stream), (0x50, vec![0, 7, 0x99, 0]));
            // Keep the connection alive until the driver closes or the controlled flush fails.
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
        });
        let (entered_tx, entered_rx) = mpsc::channel();
        let control = Arc::new(FlushControl {
            pending_ack: AtomicBool::new(false),
            entered: entered_tx,
            release: tokio::sync::Semaphore::new(0),
            fail,
        });
        let mut config = ClientConfig::v5("ack-flush", "127.0.0.1", port);
        config.common.ack_mode = AckMode::Manual;
        config.common.connector = Some(rumqttc_wrapper_core::TransportConnectorConfig {
            connector: Arc::new(ControlledConnector(Arc::clone(&control))),
            mode: rumqttc_wrapper_core::TransportMode::Established,
        });
        let mut client = NativeClient::start(config).unwrap();
        let handle = client.handle();
        let mut events = client.take_events().unwrap();
        let token = recv_publish(&mut events).ack_token.unwrap();
        let admission = handle
            .try_admit(Command::AcknowledgeWithOptions {
                token,
                protocol: AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
                    reason_code: 0x99,
                    ..Default::default()
                }),
            })
            .unwrap();
        let observer = admission.completion.clone();
        drop(admission.completion);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(observer.try_wait().unwrap().is_none());
        assert_eq!(
            observer
                .wait_timeout(Duration::from_millis(10))
                .unwrap_err()
                .kind(),
            ErrorKind::Timeout
        );
        control.release.add_permits(1);
        let outcome = observer.wait_timeout(Duration::from_secs(3));
        if fail {
            assert_eq!(
                outcome.unwrap_err().delivery_status(),
                DeliveryStatus::Ambiguous
            );
            assert_eq!(
                observer.try_wait().unwrap_err().delivery_status(),
                DeliveryStatus::Ambiguous
            );
        } else {
            assert_eq!(outcome.unwrap(), Completion::Acknowledged);
        }
        handle.close_now_idempotent();
        client.join(Duration::from_secs(3)).unwrap();
        broker.join().unwrap();
    }
}

fn publish(stream: &mut TcpStream, qos2: bool, id: u8) {
    stream
        .write_all(&[
            if qos2 { 0x34 } else { 0x32 },
            7,
            0,
            1,
            b'a',
            0,
            id,
            0,
            b'x',
        ])
        .unwrap();
}

fn recv_publish(
    events: &mut rumqttc_wrapper_core::EventConsumer,
) -> rumqttc_wrapper_core::IncomingPublish {
    loop {
        if let Some(WrapperEvent::IncomingPublish(publish)) =
            events.recv_timeout(Duration::from_secs(3)).unwrap()
        {
            return *publish;
        }
    }
}

#[test]
fn client_ack_contents_round_trip_and_negative_pubrec_releases_the_exchange() {
    // Alternate IDs to prove receive quota is released; reuse them to prove rejected QoS2 state is cleared.
    let reasons = [0x00, 0x80, 0x83, 0x87, 0x90, 0x91, 0x97, 0x99];
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let broker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        read_frame(&mut stream);
        stream.write_all(&[0x20, 3, 0, 0, 0]).unwrap();
        for qos2 in [false, true] {
            for (index, reason) in reasons.into_iter().enumerate() {
                let id = 7 + u8::try_from(index % 2).unwrap();
                publish(&mut stream, qos2, id);
                let (header, body) = read_frame(&mut stream);
                assert_eq!(header, if qos2 { 0x50 } else { 0x40 });
                // Present-empty Reason String and duplicate User Properties with an empty value.
                assert_eq!(
                    body,
                    [
                        0, id, reason, 16, 0x1f, 0, 0, 0x26, 0, 1, b'k', 0, 1, b'v', 0x26, 0, 1,
                        b'k', 0, 0
                    ]
                );
                if qos2 {
                    stream.write_all(&[0x62, 2, 0, id]).unwrap();
                    let (header, body) = read_frame(&mut stream);
                    assert_eq!(header, 0x70);
                    if reason == 0 {
                        assert_eq!(body, [0, id]);
                    } else {
                        assert_eq!(body, [0, id, 0x92, 0]);
                    }
                }
            }
        }
        stream.write_all(&[0x30, 5, 0, 1, b'a', 0, b'd']).unwrap();
        assert_eq!(read_frame(&mut stream).0, 0xe0);
    });
    let mut config = ClientConfig::v5("ack-contents", "127.0.0.1", port);
    config.common.ack_mode = AckMode::Manual;
    if let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol {
        v5.connect_properties.receive_maximum = Some(1);
    }
    let mut client = NativeClient::start(config).unwrap();
    let handle = client.handle();
    let mut events = client.take_events().unwrap();
    for _qos2 in [false, true] {
        for reason_code in reasons {
            let publication = recv_publish(&mut events);
            let token = publication.ack_token.unwrap();
            let admission = handle
                .try_admit(Command::AcknowledgeWithOptions {
                    token,
                    protocol: AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
                        reason_code,
                        reason_string: Some(String::new()),
                        user_properties: vec![
                            ("k".into(), "v".into()),
                            ("k".into(), String::new()),
                        ],
                    }),
                })
                .unwrap();
            assert_eq!(
                admission
                    .completion
                    .wait_timeout(Duration::from_secs(3))
                    .unwrap(),
                Completion::Acknowledged
            );
            assert_eq!(
                admission.completion.try_wait().unwrap().unwrap(),
                Completion::Acknowledged
            );
            assert_eq!(&publication.payload[..], b"x");
            assert!(handle.try_admit(Command::Acknowledge(token)).is_err());
        }
    }
    assert!(recv_publish(&mut events).ack_token.is_none());
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    client.join(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
}

#[test]
fn reconnect_binds_ack_validation_to_the_new_limit_and_generation() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (close_tx, close_rx) = mpsc::channel();
    let broker = thread::spawn(move || {
        for maximum in [100u8, 9] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            read_frame(&mut stream);
            stream
                .write_all(&[0x20, 8, 0, 0, 5, 0x27, 0, 0, 0, maximum])
                .unwrap();
            publish(&mut stream, false, 7);
            if maximum == 100 {
                close_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            } else {
                assert_eq!(
                    read_frame(&mut stream),
                    (0x40, vec![0, 7, 0, 3, 0x1f, 0, 0])
                );
                assert_eq!(read_frame(&mut stream).0, 0xe0);
            }
        }
    });
    let mut config = ClientConfig::v5("ack-reconnect", "127.0.0.1", port);
    config.common.ack_mode = AckMode::Manual;
    let mut client = NativeClient::start(config).unwrap();
    let handle = client.handle();
    let mut events = client.take_events().unwrap();
    let old_token = recv_publish(&mut events).ack_token.unwrap();
    close_tx.send(()).unwrap();
    let new_token = recv_publish(&mut events).ack_token.unwrap();
    assert_ne!(old_token, new_token);
    for (token, reason_string) in [(old_token, String::new()), (new_token, "x".into())] {
        let error = handle
            .try_admit(Command::AcknowledgeWithOptions {
                token,
                protocol: AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
                    reason_string: Some(reason_string),
                    ..Default::default()
                }),
            })
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Admission);
        assert_eq!(error.delivery_status(), DeliveryStatus::NotAdmitted);
    }
    let admission = handle
        .try_admit(Command::AcknowledgeWithOptions {
            token: new_token,
            protocol: AcknowledgementProtocolOptions::V5(V5AcknowledgementOptions {
                reason_string: Some(String::new()),
                ..Default::default()
            }),
        })
        .unwrap();
    assert_eq!(
        admission
            .completion
            .wait_timeout(Duration::from_secs(3))
            .unwrap(),
        Completion::Acknowledged
    );
    handle.try_admit(Command::ImmediateDisconnect).unwrap();
    client.join(Duration::from_secs(3)).unwrap();
    broker.join().unwrap();
}
