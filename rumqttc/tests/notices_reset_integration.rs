use rumqttc::v5::mqttbytes::QoS as V5QoS;
use rumqttc::v5::mqttbytes::v5::{ConnectReturnCode, Packet as V5Packet};
use rumqttc::v5::{AsyncClient as V5AsyncClient, Event as V5Event, MqttOptions as V5MqttOptions};
use rumqttc::{
    AsyncClient as V4AsyncClient, Event as V4Event, MqttOptions as V4MqttOptions,
    NoticeFailureReason, Outgoing, Packet as V4Packet, PublishNotice, PublishNoticeError,
    QoS as V4QoS, RequestNotice, RequestNoticeError,
};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::{task, time};

mod broker;
mod v5_broker;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn reserve_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn poll_until_v4_connack(eventloop: &mut rumqttc::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            match eventloop.poll().await {
                Ok(V4Event::Incoming(V4Packet::ConnAck(_))) => break,
                Ok(_) => continue,
                Err(error) => panic!("unexpected v4 connection error before ConnAck: {error:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for v4 ConnAck");
}

async fn poll_until_v5_connack(eventloop: &mut rumqttc::v5::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            match eventloop.poll().await {
                Ok(V5Event::Incoming(V5Packet::ConnAck(_))) => break,
                Ok(_) => continue,
                Err(error) => panic!("unexpected v5 connection error before ConnAck: {error:?}"),
            }
        }
    })
    .await
    .expect("timed out waiting for v5 ConnAck");
}

async fn poll_until_v4_outgoing_publish(eventloop: &mut rumqttc::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            match eventloop.poll().await {
                Ok(V4Event::Outgoing(Outgoing::Publish(_))) => break,
                Ok(_) => continue,
                Err(error) => {
                    panic!("unexpected v4 error while waiting for outgoing publish: {error:?}")
                }
            }
        }
    })
    .await
    .expect("timed out waiting for v4 outgoing publish");
}

async fn poll_until_v5_outgoing_publish(eventloop: &mut rumqttc::v5::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            match eventloop.poll().await {
                Ok(V5Event::Outgoing(Outgoing::Publish(_))) => break,
                Ok(_) => continue,
                Err(error) => {
                    panic!("unexpected v5 error while waiting for outgoing publish: {error:?}")
                }
            }
        }
    })
    .await
    .expect("timed out waiting for v5 outgoing publish");
}

async fn poll_until_v4_sub_and_unsub_outgoing(eventloop: &mut rumqttc::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        let mut saw_subscribe = false;
        let mut saw_unsubscribe = false;

        while !saw_subscribe || !saw_unsubscribe {
            match eventloop.poll().await {
                Ok(V4Event::Outgoing(Outgoing::Subscribe(_))) => saw_subscribe = true,
                Ok(V4Event::Outgoing(Outgoing::Unsubscribe(_))) => saw_unsubscribe = true,
                Ok(_) => continue,
                Err(error) => {
                    panic!("unexpected v4 error while waiting for subscribe/unsubscribe: {error:?}")
                }
            }
        }
    })
    .await
    .expect("timed out waiting for v4 subscribe/unsubscribe outgoings");
}

async fn poll_until_v5_sub_and_unsub_outgoing(eventloop: &mut rumqttc::v5::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        let mut saw_subscribe = false;
        let mut saw_unsubscribe = false;

        while !saw_subscribe || !saw_unsubscribe {
            match eventloop.poll().await {
                Ok(V5Event::Outgoing(Outgoing::Subscribe(_))) => saw_subscribe = true,
                Ok(V5Event::Outgoing(Outgoing::Unsubscribe(_))) => saw_unsubscribe = true,
                Ok(_) => continue,
                Err(error) => {
                    panic!("unexpected v5 error while waiting for subscribe/unsubscribe: {error:?}")
                }
            }
        }
    })
    .await
    .expect("timed out waiting for v5 subscribe/unsubscribe outgoings");
}

async fn poll_until_v4_connection_error(eventloop: &mut rumqttc::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            if eventloop.poll().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for v4 disconnection error");
}

async fn poll_until_v5_connection_error(eventloop: &mut rumqttc::v5::EventLoop) {
    time::timeout(TEST_TIMEOUT, async {
        loop {
            if eventloop.poll().await.is_err() {
                break;
            }
        }
    })
    .await
    .expect("timed out waiting for v5 disconnection error");
}

async fn prepare_v4_pending_tracked_publish() -> (rumqttc::EventLoop, PublishNotice) {
    let (listener, port) = reserve_listener().await;
    let broker_task = task::spawn(async move {
        let mut broker = broker::Broker::from_listener(listener, 0, false).await;
        let publish = broker
            .read_publish()
            .await
            .expect("broker did not receive tracked v4 publish");
        assert_eq!(publish.qos, V4QoS::AtLeastOnce);
    });

    let options = V4MqttOptions::new("v4-notice-reset", "127.0.0.1", port);
    let (client, mut eventloop) = V4AsyncClient::new(options, 10);

    poll_until_v4_connack(&mut eventloop).await;

    let notice = client
        .publish_tracked("hello/world", V4QoS::AtLeastOnce, false, vec![1, 2, 3])
        .await
        .expect("tracked v4 publish should be enqueued");

    poll_until_v4_outgoing_publish(&mut eventloop).await;
    broker_task.await.unwrap();

    poll_until_v4_connection_error(&mut eventloop).await;
    assert_eq!(eventloop.pending_len(), 1);

    (eventloop, notice)
}

async fn prepare_v5_pending_tracked_publish() -> (rumqttc::v5::EventLoop, PublishNotice) {
    let (listener, port) = reserve_listener().await;
    let broker_task = task::spawn(async move {
        let mut broker =
            v5_broker::Broker::from_listener(listener, ConnectReturnCode::Success, false).await;
        let packet = broker
            .read_packet()
            .await
            .expect("broker did not receive tracked v5 publish");
        match packet {
            V5Packet::Publish(publish) => {
                assert_eq!(publish.qos, V5QoS::AtLeastOnce);
            }
            packet => panic!("expected v5 publish packet, got {packet:?}"),
        }
    });

    let options = V5MqttOptions::new("v5-notice-reset", "127.0.0.1", port);
    let (client, mut eventloop) = V5AsyncClient::new(options, 10);

    poll_until_v5_connack(&mut eventloop).await;

    let notice = client
        .publish_tracked("hello/world", V5QoS::AtLeastOnce, false, vec![1, 2, 3])
        .await
        .expect("tracked v5 publish should be enqueued");

    poll_until_v5_outgoing_publish(&mut eventloop).await;
    broker_task.await.unwrap();

    poll_until_v5_connection_error(&mut eventloop).await;
    assert_eq!(eventloop.pending_len(), 1);

    (eventloop, notice)
}

#[tokio::test]
async fn v4_drain_pending_as_failed_fails_tracked_publish_notice_and_returns_count() {
    let (mut eventloop, notice) = prepare_v4_pending_tracked_publish().await;

    let drained = eventloop.drain_pending_as_failed(NoticeFailureReason::SessionReset);

    assert_eq!(drained, 1);
    assert!(eventloop.pending_is_empty());
    assert_eq!(
        notice.wait_async().await.unwrap_err(),
        PublishNoticeError::SessionReset
    );
}

#[tokio::test]
async fn v4_reset_session_state_fails_pending_tracked_notice() {
    let (mut eventloop, notice) = prepare_v4_pending_tracked_publish().await;

    eventloop.reset_session_state();

    assert!(eventloop.pending_is_empty());
    assert_eq!(
        notice.wait_async().await.unwrap_err(),
        PublishNoticeError::SessionReset
    );
}

#[tokio::test]
async fn v4_state_drain_tracked_requests_as_failed_fails_subscribe_and_unsubscribe_notices() {
    let (listener, port) = reserve_listener().await;
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let broker_task = task::spawn(async move {
        let mut broker = broker::Broker::from_listener(listener, 0, false).await;

        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                packet = broker.read_packet() => match packet {
                    Some(V4Packet::PingReq) => broker.pingresp().await,
                    Some(_) => {}
                    None => break,
                }
            }
        }
    });

    let options = V4MqttOptions::new("v4-state-drain", "127.0.0.1", port);
    let (client, mut eventloop) = V4AsyncClient::new(options, 10);

    poll_until_v4_connack(&mut eventloop).await;

    let subscribe_notice = client
        .subscribe_tracked("hello/world", V4QoS::AtLeastOnce)
        .await
        .expect("tracked v4 subscribe should be enqueued");
    let unsubscribe_notice = client
        .unsubscribe_tracked("hello/world")
        .await
        .expect("tracked v4 unsubscribe should be enqueued");

    poll_until_v4_sub_and_unsub_outgoing(&mut eventloop).await;

    assert_eq!(eventloop.state.tracked_subscribe_len(), 1);
    assert_eq!(eventloop.state.tracked_unsubscribe_len(), 1);
    assert!(!eventloop.state.tracked_requests_is_empty());

    let drained = eventloop
        .state
        .drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);

    assert_eq!(drained, 2);
    assert!(eventloop.state.tracked_requests_is_empty());
    assert_eq!(
        subscribe_notice.wait_async().await.unwrap_err(),
        RequestNoticeError::SessionReset
    );
    assert_eq!(
        unsubscribe_notice.wait_async().await.unwrap_err(),
        RequestNoticeError::SessionReset
    );

    let _ = stop_tx.send(());
    broker_task.await.unwrap();
}

#[tokio::test]
async fn v5_drain_pending_as_failed_fails_tracked_publish_notice_and_returns_count() {
    let (mut eventloop, notice) = prepare_v5_pending_tracked_publish().await;

    let drained = eventloop.drain_pending_as_failed(NoticeFailureReason::SessionReset);

    assert_eq!(drained, 1);
    assert!(eventloop.pending_is_empty());
    assert_eq!(
        notice.wait_async().await.unwrap_err(),
        PublishNoticeError::SessionReset
    );
}

#[tokio::test]
async fn v5_reset_session_state_fails_pending_tracked_notice() {
    let (mut eventloop, notice) = prepare_v5_pending_tracked_publish().await;

    eventloop.reset_session_state();

    assert!(eventloop.pending_is_empty());
    assert_eq!(
        notice.wait_async().await.unwrap_err(),
        PublishNoticeError::SessionReset
    );
}

#[tokio::test]
async fn v5_state_drain_tracked_requests_as_failed_fails_subscribe_and_unsubscribe_notices() {
    let (listener, port) = reserve_listener().await;
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let broker_task = task::spawn(async move {
        let mut broker =
            v5_broker::Broker::from_listener(listener, ConnectReturnCode::Success, false).await;

        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = broker.tick() => {}
            }
        }
    });

    let options = V5MqttOptions::new("v5-state-drain", "127.0.0.1", port);
    let (client, mut eventloop) = V5AsyncClient::new(options, 10);

    poll_until_v5_connack(&mut eventloop).await;

    let subscribe_notice: RequestNotice = client
        .subscribe_tracked("hello/world", V5QoS::AtLeastOnce)
        .await
        .expect("tracked v5 subscribe should be enqueued");
    let unsubscribe_notice: RequestNotice = client
        .unsubscribe_tracked("hello/world")
        .await
        .expect("tracked v5 unsubscribe should be enqueued");

    poll_until_v5_sub_and_unsub_outgoing(&mut eventloop).await;

    assert_eq!(eventloop.state.tracked_subscribe_len(), 1);
    assert_eq!(eventloop.state.tracked_unsubscribe_len(), 1);
    assert!(!eventloop.state.tracked_requests_is_empty());

    let drained = eventloop
        .state
        .drain_tracked_requests_as_failed(NoticeFailureReason::SessionReset);

    assert_eq!(drained, 2);
    assert!(eventloop.state.tracked_requests_is_empty());
    assert_eq!(
        subscribe_notice.wait_async().await.unwrap_err(),
        RequestNoticeError::SessionReset
    );
    assert_eq!(
        unsubscribe_notice.wait_async().await.unwrap_err(),
        RequestNoticeError::SessionReset
    );

    let _ = stop_tx.send(());
    broker_task.await.unwrap();
}
