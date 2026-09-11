#![cfg(feature = "ordered-shutdown")]

use rumqttc::mqttbytes::v5::Packet;
use rumqttc::*;
use std::time::Duration;
#[tokio::test]
async fn broker_redirect_fails_ordered_notice_without_following_redirect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (client, eventloop) = AsyncClient::builder(options(port)).capacity(2).build();
    client
        .try_publish("redirect", "payload", PublishOptions::new(QoS::AtLeastOnce))
        .unwrap();
    let notice = client
        .try_disconnect_after_queued_with_timeout(LIMIT)
        .unwrap();
    let driver = tokio::spawn(drive(eventloop));
    let mut broker = broker(listener).await;
    broker.read_publish_with_timeout(LIMIT).await.unwrap();
    broker
        .send_packet(Packet::Disconnect(Disconnect {
            reason_code: DisconnectReasonCode::UseAnotherServer,
            properties: Some(DisconnectProperties {
                server_reference: Some("elsewhere.example:1883".into()),
                session_expiry_interval: None,
                reason_string: None,
                user_properties: Vec::new(),
            }),
        }))
        .await;
    assert!(matches!(
        notice.wait_async().await,
        Err(DisconnectNoticeError::Redirected)
    ));
    assert!(matches!(
        driver.await.unwrap(),
        Err(ConnectionError::OrderedDisconnect(
            DisconnectNoticeError::Redirected
        ))
    ));
}

#[tokio::test]
async fn ordered_fence_wakes_publish_waiting_for_negotiated_capabilities() {
    let (client, _eventloop) = AsyncClient::builder(options(1))
        .publish_admission_policy(PublishAdmissionPolicy::RequireNegotiatedCapabilities)
        .capacity(2)
        .build();
    let mut publish =
        Box::pin(client.publish("pending", "payload", PublishOptions::new(QoS::AtLeastOnce)));
    use std::future::Future;
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(publish.as_mut().poll(&mut context).is_pending());
    drop(client.try_disconnect_after_queued().unwrap());
    assert!(matches!(
        tokio::time::timeout(LIMIT, publish).await.unwrap(),
        Err(ClientError::Closing(_))
    ));
}
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
#[tokio::test]
async fn ordered_disconnect_preserves_mqtt5_reason_and_properties() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (client, eventloop) = AsyncClient::builder(options(port)).capacity(1).build();
    let properties = DisconnectProperties {
        session_expiry_interval: Some(0),
        reason_string: Some("shutdown complete".into()),
        user_properties: vec![("source".into(), "ordered-test".into())],
        server_reference: None,
    };
    let reason = DisconnectReasonCode::DisconnectWithWillMessage;
    let notice = client
        .try_disconnect_after_queued_with_properties_timeout(reason, properties.clone(), LIMIT)
        .unwrap();
    let driver = tokio::spawn(drive(eventloop));
    let mut broker = broker(listener).await;
    let Some(Packet::Disconnect(disconnect)) = broker.read_packet_with_timeout(LIMIT).await else {
        panic!("expected disconnect");
    };
    assert_eq!(disconnect.reason_code, reason);
    assert_eq!(disconnect.properties, Some(properties));
    notice.wait_async().await.unwrap();
    driver.await.unwrap().unwrap();
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
#[tokio::test]
async fn replay_cleanup_rejection_fails_ordered_notice_after_session_resume() {
    for tracked in [false, true] {
        for fence_before_cleanup in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut config = options(port);
            config
                .set_clean_start(false)
                .set_session_expiry_interval(Some(60));
            let (client, mut eventloop) = AsyncClient::builder(config).capacity(2).build();
            let (connected, first) = tokio::join!(eventloop.poll(), broker(listener));
            assert!(matches!(
                connected.unwrap(),
                Event::Incoming(Packet::ConnAck(_))
            ));

            let publish_options =
                PublishOptions::new(QoS::AtLeastOnce).properties(PublishProperties {
                    topic_alias: Some(1),
                    ..Default::default()
                });
            let publish_notice = if tracked {
                Some(
                    client
                        .try_publish_tracked("", "payload", publish_options)
                        .unwrap(),
                )
            } else {
                client.try_publish("", "payload", publish_options).unwrap();
                None
            };
            let notice = fence_before_cleanup.then(|| {
                client
                    .try_disconnect_after_queued_with_timeout(LIMIT)
                    .unwrap()
            });
            // Lose the connection before polling the accepted publish out of the channel.
            drop(first);
            eventloop.clean();
            eventloop.clean();
            if let Some(publish_notice) = publish_notice {
                assert!(matches!(
                    publish_notice.wait_async().await,
                    Err(PublishNoticeError::TopicAliasReplayUnavailable(1))
                ));
            }
            let notice = notice.unwrap_or_else(|| {
                client
                    .try_disconnect_after_queued_with_timeout(LIMIT)
                    .unwrap()
            });
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            let driver = tokio::spawn(drive(eventloop));
            let mut second = broker::Broker::from_listener(
                listener,
                broker::ConnectBehavior::Accept {
                    session_saved: true,
                },
            )
            .await;
            assert!(matches!(
                tokio::time::timeout(LIMIT, notice.wait_async())
                    .await
                    .unwrap(),
                Err(DisconnectNoticeError::Publish(
                    PublishNoticeError::TopicAliasReplayUnavailable(1)
                ))
            ));
            assert!(matches!(
                driver.await.unwrap(),
                Err(ConnectionError::OrderedDisconnect(
                    DisconnectNoticeError::Publish(
                        PublishNoticeError::TopicAliasReplayUnavailable(1)
                    )
                ))
            ));
            assert!(!matches!(
                second.read_packet_with_timeout(LIMIT).await,
                Some(Packet::Disconnect(_))
            ));
        }
    }
}

async fn reconnect_case(resume: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = options(port);
    config
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60));
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
    let mut second = broker::Broker::from_listener(
        listener,
        broker::ConnectBehavior::Accept {
            session_saved: resume,
        },
    )
    .await;
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
            Some(Packet::Disconnect(_))
        ));
        notice.wait_async().await.unwrap();
    } else {
        assert!(matches!(
            notice.wait_async().await,
            Err(DisconnectNoticeError::SessionReset)
        ));
        assert!(!matches!(
            second.read_packet_with_timeout(LIMIT).await,
            Some(Packet::Disconnect(_))
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
    config
        .set_clean_start(false)
        .set_session_expiry_interval(Some(60))
        .set_connect_timeout(Duration::from_millis(100));
    let (client, mut eventloop) = AsyncClient::builder(config).capacity(2).build();
    client
        .try_publish("replay", "payload", PublishOptions::new(QoS::AtLeastOnce))
        .unwrap();
    let notice = client
        .try_disconnect_after_queued_with_timeout(Duration::from_secs(10))
        .unwrap();
    // Accept TCP but withhold CONNACK until the connection attempt times out.
    let (result, accepted) = tokio::join!(eventloop.poll(), listener.accept());
    let (stream, _) = accepted.unwrap();
    assert!(matches!(result, Err(ConnectionError::Timeout(_))));
    drop(stream);
    let driver = tokio::spawn(drive(eventloop));
    let mut peer = broker(listener).await;
    let publish = peer.read_publish_with_timeout(LIMIT).await.unwrap();
    assert_eq!(publish.payload.as_ref(), b"payload");
    peer.ack(publish.pkid).await;
    assert!(matches!(
        peer.read_packet_with_timeout(LIMIT).await,
        Some(Packet::Disconnect(_))
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
#[tokio::test]
async fn negative_acknowledgements_fail_the_collective_notice() {
    use rumqttc::mqttbytes::v5::{
        PubAck, PubAckReason, PubComp, PubCompReason, PubRec, PubRecReason,
    };
    for phase in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (client, eventloop) = AsyncClient::builder(options(port)).capacity(2).build();
        let qos = if phase == 0 {
            QoS::AtLeastOnce
        } else {
            QoS::ExactlyOnce
        };
        client
            .publish("rejected", "payload", PublishOptions::new(qos))
            .await
            .unwrap();
        let notice = client
            .disconnect_after_queued_with_timeout(LIMIT)
            .await
            .unwrap();
        let driver = tokio::spawn(drive(eventloop));
        let mut broker = broker(listener).await;
        let publish = broker.read_publish_with_timeout(LIMIT).await.unwrap();
        let expected = match phase {
            0 => {
                let mut ack = PubAck::new(publish.pkid, None);
                ack.reason = PubAckReason::NotAuthorized;
                broker.send_packet(Packet::PubAck(ack)).await;
                PublishNoticeError::V5PubAck(PubAckReason::NotAuthorized)
            }
            1 => {
                let mut ack = PubRec::new(publish.pkid, None);
                ack.reason = PubRecReason::NotAuthorized;
                broker.send_packet(Packet::PubRec(ack)).await;
                PublishNoticeError::V5PubRec(PubRecReason::NotAuthorized)
            }
            _ => {
                broker.pubrec(publish.pkid).await;
                assert!(matches!(
                    broker.read_packet_with_timeout(LIMIT).await,
                    Some(Packet::PubRel(_))
                ));
                let mut ack = PubComp::new(publish.pkid, None);
                ack.reason = PubCompReason::PacketIdentifierNotFound;
                broker.send_packet(Packet::PubComp(ack)).await;
                PublishNoticeError::V5PubComp(PubCompReason::PacketIdentifierNotFound)
            }
        };
        assert!(
            matches!(notice.wait_async().await, Err(DisconnectNoticeError::Publish(error)) if error == expected)
        );
        assert!(driver.await.unwrap().is_err());
        assert!(!matches!(
            broker.read_packet_with_timeout(LIMIT).await,
            Some(Packet::Disconnect(_))
        ));
    }
}
use tokio::net::TcpListener;
#[allow(dead_code)]
mod broker;
const LIMIT: Duration = Duration::from_secs(3);

fn options(port: u16) -> MqttOptions {
    let mut options = MqttOptions::new("ordered-shutdown", ("127.0.0.1", port));
    options
        .set_outgoing_inflight_upper_limit(1)
        .set_max_request_batch(1);
    options
}
async fn broker(listener: TcpListener) -> broker::Broker {
    broker::Broker::from_listener(
        listener,
        broker::ConnectBehavior::Accept {
            session_saved: false,
        },
    )
    .await
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
            Packet::Disconnect(_) => {
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
        Some(Packet::Disconnect(_))
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
