#![cfg(feature = "ordered-shutdown")]

use rumqttc::mqttbytes::v4::Packet;
use rumqttc::*;
use std::time::Duration;
#[test]
fn cloned_producers_and_fence_have_one_admission_order() {
    for _ in 0..16 {
        let (client, eventloop) = AsyncClient::builder(options(1)).capacity(2).build();
        let start = std::sync::Barrier::new(2);
        let (publish, fence) = std::thread::scope(|scope| {
            let first = client.clone();
            let second = client.clone();
            let start_ref = &start;
            let publisher = scope.spawn(move || {
                start_ref.wait();
                first.try_publish("race", "payload", PublishOptions::new(QoS::AtLeastOnce))
            });
            let closer = scope.spawn(move || {
                start_ref.wait();
                second.try_disconnect_after_queued()
            });
            (publisher.join().unwrap(), closer.join().unwrap())
        });
        let notice = fence.unwrap();
        let sequence = eventloop.diagnostics().disconnect_fence_sequence.unwrap();
        match publish {
            Ok(()) => assert_eq!(sequence, 2),
            Err(ClientError::Closing(_)) => assert_eq!(sequence, 1),
            other => panic!("unexpected admission result: {other:?}"),
        }
        drop(eventloop);
        assert!(matches!(
            notice.wait(),
            Err(DisconnectNoticeError::ReceiverTerminated)
        ));
    }
}
#[test]
fn blocking_and_try_apis_return_independent_notices() {
    for trying in [false, true] {
        let (client, connection) = Client::builder(options(1)).capacity(1).build();
        let notice = if trying {
            client
                .try_disconnect_after_queued_with_timeout(LIMIT)
                .unwrap()
        } else {
            client.disconnect_after_queued_with_timeout(LIMIT).unwrap()
        };
        assert!(matches!(
            client.try_disconnect_after_queued(),
            Err(ClientError::Closing(_))
        ));
        drop(connection);
        assert!(matches!(
            notice.wait(),
            Err(DisconnectNoticeError::ReceiverTerminated)
        ));
    }
}

#[tokio::test]
async fn overflowing_timeout_does_not_close_admission() {
    let (client, eventloop) = AsyncClient::builder(options(1)).capacity(1).build();
    assert!(matches!(
        client.try_disconnect_after_queued_with_timeout(Duration::MAX),
        Err(ClientError::InvalidDisconnectTimeout)
    ));
    client
        .try_publish(
            "still-open",
            "payload",
            PublishOptions::new(QoS::AtMostOnce),
        )
        .unwrap();
    assert_eq!(eventloop.diagnostics().disconnect_fence_sequence, None);
}
async fn reconnect_case(resume: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = options(port);
    config.set_clean_session(false);
    let (client, mut eventloop) = AsyncClient::builder(config).capacity(3).build();
    for index in 0..2u8 {
        client
            .try_publish("replay", vec![index], PublishOptions::new(QoS::AtLeastOnce))
            .unwrap();
    }
    let notice = client
        .try_disconnect_after_queued_with_timeout(LIMIT)
        .unwrap();
    let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let driver = tokio::spawn(async move {
        let mut lost_tx = Some(lost_tx);
        let mut resume_rx = Some(resume_rx);
        loop {
            match eventloop.poll().await {
                Ok(_) => {}
                Err(ConnectionError::RequestsDone) => break,
                Err(error) => {
                    if let Some(tx) = lost_tx.take() {
                        tx.send(()).unwrap();
                        resume_rx.take().unwrap().await.unwrap();
                    } else {
                        panic!("unexpected reconnect error: {error:?}");
                    }
                }
            }
        }
    });
    let mut first = broker(listener).await;
    let publish = first.read_publish_with_timeout(LIMIT).await.unwrap();
    assert_eq!(publish.payload.as_ref(), &[0]);
    drop(first);
    tokio::time::timeout(LIMIT, lost_rx).await.unwrap().unwrap();
    assert!(matches!(
        client.try_publish("later", "", PublishOptions::new(QoS::AtMostOnce)),
        Err(ClientError::Closing(_))
    ));
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    resume_tx.send(()).unwrap();
    let mut second = broker::Broker::from_listener(listener, 0, resume).await;
    if resume {
        for index in 0..2u8 {
            let publish = second.read_publish_with_timeout(LIMIT).await.unwrap();
            assert_eq!(publish.payload.as_ref(), &[index]);
            if index == 0 {
                assert!(publish.dup);
            }
            second.ack(publish.pkid).await;
        }
        assert!(matches!(
            second.read_packet_with_timeout(LIMIT).await,
            Some(Packet::Disconnect)
        ));
        notice.wait_async().await.unwrap();
    } else {
        assert!(matches!(
            notice.wait_async().await,
            Err(DisconnectNoticeError::SessionReset)
        ));
        assert!(!matches!(
            second.read_packet_with_timeout(LIMIT).await,
            Some(Packet::Disconnect)
        ));
    }
    tokio::time::timeout(LIMIT, driver).await.unwrap().unwrap();
}

#[tokio::test]
async fn reconnect_preserves_fence_after_preceding_replayed_publishes() {
    reconnect_case(true).await;
}

#[tokio::test]
async fn connection_timeout_preserves_fence_and_reconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = options(listener.local_addr().unwrap().port());
    config.set_clean_session(false);
    let (client, mut eventloop) = AsyncClient::builder(config).capacity(2).build();
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(1);
    eventloop.set_network_options(network_options);
    client
        .try_publish("replay", "payload", PublishOptions::new(QoS::AtLeastOnce))
        .unwrap();
    let notice = client
        .try_disconnect_after_queued_with_timeout(Duration::from_secs(10))
        .unwrap();
    // Accept TCP but withhold CONNACK until the connection attempt times out.
    let (result, accepted) = tokio::join!(eventloop.poll(), listener.accept());
    let (stream, _) = accepted.unwrap();
    assert!(matches!(result, Err(ConnectionError::NetworkTimeout)));
    drop(stream);
    let driver = tokio::spawn(drive(eventloop));
    let mut peer = broker(listener).await;
    let publish = peer.read_publish_with_timeout(LIMIT).await.unwrap();
    assert_eq!(publish.payload.as_ref(), b"payload");
    peer.ack(publish.pkid).await;
    assert!(matches!(
        peer.read_packet_with_timeout(LIMIT).await,
        Some(Packet::Disconnect)
    ));
    notice.wait_async().await.unwrap();
    tokio::time::timeout(LIMIT, driver)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn missing_broker_session_fails_fence_without_disconnect() {
    reconnect_case(false).await;
}
use tokio::net::TcpListener;
#[allow(dead_code)]
mod broker;
const LIMIT: Duration = Duration::from_secs(3);

fn options(port: u16) -> MqttOptions {
    let mut options = MqttOptions::new("ordered-shutdown", ("127.0.0.1", port));
    options.set_inflight(1).set_max_request_batch(1);
    options
}
async fn broker(listener: TcpListener) -> broker::Broker {
    broker::Broker::from_listener(listener, 0, false).await
}
async fn drive(mut eventloop: EventLoop) -> Result<(), ConnectionError> {
    loop {
        match eventloop.poll().await {
            Ok(_) => {}
            Err(ConnectionError::RequestsDone) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

#[tokio::test]
async fn ordered_fence_drains_mixed_qos_backlog_beyond_channel_and_inflight_capacity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (client, eventloop) = AsyncClient::builder(options(port)).capacity(2).build();
    let driver = tokio::spawn(drive(eventloop));
    let producer = tokio::spawn(async move {
        for index in 0..12u8 {
            let qos = match index % 3 {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            };
            client
                .publish("ordered", vec![index], PublishOptions::new(qos))
                .await
                .unwrap();
        }
        let notice = client
            .disconnect_after_queued_with_timeout(LIMIT)
            .await
            .unwrap();
        assert!(matches!(
            client.try_publish("later", "payload", PublishOptions::new(QoS::AtMostOnce)),
            Err(ClientError::Closing(_))
        ));
        assert!(matches!(
            client.try_subscribe("later", QoS::AtMostOnce),
            Err(ClientError::Closing(_))
        ));
        notice.wait_async().await.unwrap();
    });
    let mut broker = broker(listener).await;
    let mut next = 0u8;
    let mut qos2 = std::collections::HashSet::new();
    loop {
        match broker.read_packet_with_timeout(LIMIT).await.unwrap() {
            Packet::Publish(publish) => {
                assert_eq!(publish.payload.as_ref(), &[next]);
                next += 1;
                match publish.qos {
                    QoS::AtMostOnce => {}
                    QoS::AtLeastOnce => broker.ack(publish.pkid).await,
                    QoS::ExactlyOnce => {
                        qos2.insert(publish.pkid);
                        broker.pubrec(publish.pkid).await;
                    }
                }
            }
            Packet::PubRel(rel) => {
                assert!(qos2.remove(&rel.pkid));
                broker.pubcomp(rel.pkid).await;
            }
            Packet::Disconnect => {
                assert_eq!(next, 12);
                assert!(qos2.is_empty());
                break;
            }
            other => panic!("unexpected packet: {other:?}"),
        }
    }
    tokio::time::timeout(LIMIT, producer)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(LIMIT, driver)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn timeout_includes_time_before_fence_observation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (client, eventloop) = AsyncClient::builder(options(port)).capacity(3).build();
    for _ in 0..2 {
        client
            .try_publish("ordered", "payload", PublishOptions::new(QoS::AtLeastOnce))
            .unwrap();
    }
    let notice = client
        .try_disconnect_after_queued_with_timeout(Duration::from_millis(100))
        .unwrap();
    let driver = tokio::spawn(drive(eventloop));
    let mut broker = broker(listener).await;
    broker.read_publish_with_timeout(LIMIT).await.unwrap();
    assert!(matches!(
        notice.wait_async().await,
        Err(DisconnectNoticeError::DisconnectTimeout)
    ));
    assert!(driver.await.unwrap().is_err());
    assert!(!matches!(
        broker.read_packet_with_timeout(LIMIT).await,
        Some(Packet::Disconnect)
    ));
}

#[tokio::test]
async fn zero_timeout_expires_without_opening_a_connection() {
    let (client, mut eventloop) = AsyncClient::builder(options(1)).capacity(1).build();
    let notice = client
        .try_disconnect_after_queued_with_timeout(Duration::ZERO)
        .unwrap();
    assert!(eventloop.poll().await.is_err());
    assert!(matches!(
        notice.wait_async().await,
        Err(DisconnectNoticeError::DisconnectTimeout)
    ));
}

#[tokio::test]
async fn full_try_and_cancelled_async_admission_leave_client_open() {
    let (client, eventloop) = AsyncClient::builder(options(1)).capacity(1).build();
    client
        .try_publish("first", "payload", PublishOptions::new(QoS::AtMostOnce))
        .unwrap();
    assert!(matches!(
        client.try_disconnect_after_queued(),
        Err(ClientError::RequestChannelFull(_))
    ));
    let mut admission = Box::pin(client.disconnect_after_queued());
    use std::future::Future;
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(admission.as_mut().poll(&mut context).is_pending());
    drop(admission);
    assert!(matches!(
        client.try_publish("second", "payload", PublishOptions::new(QoS::AtMostOnce)),
        Err(ClientError::RequestChannelFull(_))
    ));
    assert_eq!(eventloop.diagnostics().disconnect_fence_sequence, None);
}

#[tokio::test]
async fn receiver_loss_and_first_wins_are_typed() {
    let (client, eventloop) = AsyncClient::builder(options(1)).capacity(1).build();
    let notice = client.try_disconnect_after_queued().unwrap();
    assert!(matches!(
        client.try_disconnect_after_queued(),
        Err(ClientError::Closing(_))
    ));
    drop(eventloop);
    assert!(matches!(
        notice.wait_async().await,
        Err(DisconnectNoticeError::ReceiverTerminated)
    ));
}

#[tokio::test]
async fn external_sender_cannot_claim_managed_completion() {
    let (tx, rx) = flume::bounded(1);
    let client = AsyncClient::from_senders(tx);
    assert!(matches!(
        client.disconnect_after_queued().await,
        Err(ClientError::TrackingUnavailable)
    ));
    assert!(rx.is_empty());
}
