use super::*;
use crate::mqttbytes::QoS;
use crate::{AsyncClient, NetworkOptions};
use futures_util::FutureExt;
use std::io::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const QOS1_PUBLISH: &[u8] = &[0x32, 6, 0, 1, b'a', 0, 7, 0];

async fn blocked_client(
    buffered_pings: usize,
    timeout: u64,
) -> (AsyncClient, EventLoop, DuplexStream) {
    let mut options = MqttOptions::new("response-write-deadline", "localhost");
    options.set_keep_alive(1).set_read_batch_size(1);
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    let mut network_options = NetworkOptions::new();
    network_options.set_connection_timeout(timeout);
    eventloop.options.set_network_options(network_options);
    eventloop.keepalive_timeout = Some(Box::pin(time::sleep(Duration::from_secs(1))));
    let (socket, peer) = tokio::io::duplex(8);
    let mut network = Network::new(socket, None);
    for _ in 0..4 {
        network.write(Packet::PingReq).await.unwrap();
        network.flush().await.unwrap();
    }
    for _ in 0..buffered_pings {
        network.write(Packet::PingReq).await.unwrap();
    }
    network.set_backpressure_boundary(1);
    eventloop.network = Some(network);
    (client, eventloop, peer)
}

fn assert_write_timeout(error: ConnectionError) {
    match error {
        ConnectionError::MqttState(StateError::Io(error)) => {
            assert_eq!(error.kind(), ErrorKind::TimedOut)
        }
        other => panic!("expected response write timeout, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn response_write_timeout_closes_transport_and_drains_events() {
    // A buffered frame makes feed() block; no buffered frames makes flush() block.
    for buffered in [0, 1] {
        let (client, mut eventloop, mut peer) = blocked_client(buffered, 1).await;
        peer.write_all(QOS1_PUBLISH).await.unwrap();
        let first = {
            let polling = eventloop.poll();
            tokio::pin!(polling);
            assert!(polling.as_mut().now_or_never().is_none());
            client.try_disconnect_now().unwrap();
            time::timeout(Duration::from_secs(2), polling)
                .await
                .expect("response writes must time out even with a queued disconnect")
                .unwrap()
        };
        assert!(matches!(first, Event::Incoming(Packet::Publish(publish)) if publish.pkid == 7));
        assert!(
            eventloop.network.is_none(),
            "a timed-out transport must already be closed"
        );
        assert!(!eventloop.immediate_disconnect_rx.is_empty());
        let mut bytes = [0; 8];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(peer.read(&mut [0]).await.unwrap(), 0);
        assert_eq!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PubAck(7))
        );
        assert_write_timeout(eventloop.poll().await.unwrap_err());
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::RequestsDone)
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn response_write_timeout_uses_one_budget_for_feeding_and_flushing() {
    let (_client, mut eventloop, mut peer) = blocked_client(4, 1).await;
    peer.write_all(QOS1_PUBLISH).await.unwrap();
    let first = {
        let polling = eventloop.poll();
        tokio::pin!(polling);
        assert!(polling.as_mut().now_or_never().is_none());
        time::advance(Duration::from_millis(600)).await;
        peer.read_exact(&mut [0; 8]).await.unwrap();
        // Feeding the PUBACK flushes the buffered pings and fills the transport
        // again. The final flush must retain the original deadline.
        assert!(polling.as_mut().now_or_never().is_none());
        time::advance(Duration::from_millis(500)).await;
        polling
            .as_mut()
            .now_or_never()
            .expect("flushing must not restart the write budget")
            .unwrap()
    };
    assert!(matches!(first, Event::Incoming(Packet::Publish(_))));
    assert!(eventloop.network.is_none());
    assert_eq!(
        eventloop.poll().await.unwrap(),
        Event::Outgoing(Outgoing::PubAck(7))
    );
    assert_write_timeout(eventloop.poll().await.unwrap_err());
}

#[tokio::test(start_paused = true)]
async fn response_write_timeout_preserves_completed_publish_notices() {
    for collision in [false, true] {
        let (_client, mut eventloop, mut peer) = blocked_client(1, 1).await;
        let (tx, notice) = PublishNoticeTx::new();
        let mut publish =
            crate::mqttbytes::v5::Publish::new("tracked", QoS::AtLeastOnce, vec![1], None);
        publish.pkid = 1;
        eventloop
            .state
            .handle_outgoing_packet_with_notice(
                Request::Publish(publish.clone()),
                Some(TrackedNoticeTx::Publish(tx)),
            )
            .unwrap();
        if collision {
            eventloop
                .state
                .handle_outgoing_packet(Request::Publish(publish))
                .unwrap();
            assert!(eventloop.state.collision.is_some());
        }
        eventloop.state.events.clear();
        peer.write_all(&[0x40, 2, 0, 1]).await.unwrap();
        let first = time::timeout(Duration::from_secs(2), eventloop.poll())
            .await
            .expect("both feeding and flushing must retain collected notices")
            .unwrap();
        assert!(matches!(first, Event::Incoming(Packet::PubAck(packet)) if packet.pkid == 1));
        assert!(
            matches!(notice.wait_async().await.unwrap(), PublishResult::Qos1(packet) if packet.pkid == 1)
        );
        assert!(eventloop.network.is_none());
        if collision {
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Outgoing(Outgoing::Publish(1))
            );
        }
        assert_write_timeout(eventloop.poll().await.unwrap_err());
    }
}

#[tokio::test(start_paused = true)]
async fn response_write_timeout_also_bounds_the_keepalive_scan() {
    let (_client, mut eventloop, mut peer) = blocked_client(1, 1).await;
    eventloop
        .state
        .handle_outgoing_packet(Request::PingReq)
        .unwrap();
    eventloop.state.events.clear();
    peer.write_all(QOS1_PUBLISH).await.unwrap();
    let first = time::timeout(Duration::from_secs(2), eventloop.handle_keepalive_ping())
        .await
        .expect("ready-frame keepalive scans must bound response writes")
        .unwrap();
    assert!(matches!(first, Event::Incoming(Packet::Publish(_))));
    assert!(eventloop.network.is_none());
    assert_eq!(
        eventloop.poll().await.unwrap(),
        Event::Outgoing(Outgoing::PubAck(7))
    );
    assert_write_timeout(eventloop.poll().await.unwrap_err());
}

#[tokio::test(start_paused = true)]
async fn response_write_timeout_accepts_the_maximum_configured_timeout() {
    let (_client, mut eventloop, mut peer) = blocked_client(1, u64::MAX).await;
    peer.read_exact(&mut [0; 8]).await.unwrap();
    peer.write_all(QOS1_PUBLISH).await.unwrap();
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::Publish(_))
    ));
    assert!(eventloop.network.is_some());
}

#[tokio::test(start_paused = true)]
async fn response_write_deadline_is_restored_when_poll_is_cancelled() {
    async fn frame(peer: &mut DuplexStream) -> Vec<u8> {
        let header = peer.read_u8().await.unwrap();
        let len = peer.read_u8().await.unwrap();
        assert!(len < 128, "test frames use a one-byte remaining length");
        let mut bytes = vec![header, len];
        bytes.resize(usize::from(len) + 2, 0);
        peer.read_exact(&mut bytes[2..]).await.unwrap();
        bytes
    }

    let (client, mut eventloop, mut peer) = blocked_client(0, 1).await;
    eventloop.keepalive_timeout = None;
    peer.write_all(QOS1_PUBLISH).await.unwrap();
    {
        let polling = eventloop.poll();
        tokio::pin!(polling);
        assert!(polling.as_mut().now_or_never().is_none());
        // The PUBACK is buffered and its flush is blocked. Cancel the poll
        // through the same selection mechanism used by connection owners.
        tokio::select! {
            biased;
            () = std::future::ready(()) => {},
            result = polling => panic!("blocked response unexpectedly completed: {result:?}"),
        }
    }
    peer.read_exact(&mut [0; 8]).await.unwrap();
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::Publish(_))
    ));
    assert_eq!(
        eventloop.poll().await.unwrap(),
        Event::Outgoing(Outgoing::PubAck(7))
    );

    client
        .try_publish(
            "q",
            Vec::<u8>::new(),
            crate::PublishOptions::new(QoS::AtMostOnce),
        )
        .unwrap();
    let (event, (ack, request)) = time::timeout(Duration::from_secs(1), async {
        tokio::join!(eventloop.poll(), async {
            let ack = frame(&mut peer).await;
            let request = frame(&mut peer).await;
            (ack, request)
        })
    })
    .await
    .expect("a request must flush successfully after the cancelled response");
    assert_eq!(event.unwrap(), Event::Outgoing(Outgoing::Publish(0)));
    assert_eq!(ack, [0x40, 2, 0, 7]);
    assert_eq!(request[0], 0x30);

    // Fill the transport again and leave a frame buffered. A later request
    // must wait for backpressure to clear rather than use the expired deadline.
    let network = eventloop.network.as_mut().unwrap();
    for _ in 0..4 {
        network.write(Packet::PingReq).await.unwrap();
        network.flush().await.unwrap();
    }
    network.write(Packet::PingReq).await.unwrap();
    time::advance(Duration::from_secs(2)).await;
    client
        .try_publish(
            "later",
            Vec::<u8>::new(),
            crate::PublishOptions::new(QoS::AtMostOnce),
        )
        .unwrap();
    let (event, (buffered, request)) = {
        let polling = eventloop.poll();
        tokio::pin!(polling);
        assert!(
            polling.as_mut().now_or_never().is_none(),
            "a cancelled response deadline must not fail a later request"
        );
        time::timeout(Duration::from_secs(1), async {
            tokio::join!(polling, async {
                peer.read_exact(&mut [0; 8]).await.unwrap();
                let buffered = frame(&mut peer).await;
                let request = frame(&mut peer).await;
                (buffered, request)
            })
        })
        .await
        .expect("the later request must finish once backpressure clears")
    };
    assert_eq!(event.unwrap(), Event::Outgoing(Outgoing::Publish(0)));
    assert_eq!(buffered, [0xc0, 0]);
    assert_eq!(request[0], 0x30);
    assert_eq!(&request[4..9], b"later");
    assert!(eventloop.network.is_some());
}

#[tokio::test(start_paused = true)]
async fn keepalive_scan_deadline_bounds_a_graceful_disconnect_read() {
    for timeout in [
        None,
        Some(Duration::from_millis(500)),
        Some(Duration::from_secs(2)),
    ] {
        let (_client, mut eventloop, _peer) = blocked_client(0, 10).await;
        eventloop
            .state
            .handle_outgoing_packet(Request::PingReq)
            .unwrap();
        eventloop.state.events.clear();
        eventloop.keepalive_scan_deadline = Some(Instant::now() + Duration::from_secs(1));
        eventloop.pending_disconnect = Some(PendingDisconnect::new(
            crate::Disconnect::new(crate::mqttbytes::v5::DisconnectReasonCode::NormalDisconnection),
            timeout,
        ));
        let error = time::timeout(Duration::from_secs(3), eventloop.poll_disconnect_drain())
            .await
            .expect("disconnect draining must retain the active keepalive scan deadline")
            .unwrap_err();
        if timeout == Some(Duration::from_millis(500)) {
            assert!(matches!(error, ConnectionError::DisconnectTimeout));
        } else {
            assert!(matches!(
                error,
                ConnectionError::MqttState(StateError::AwaitPingResp)
            ));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn keepalive_scan_deadline_bounds_ordinary_response_writes() {
    let (_client, mut eventloop, mut peer) = blocked_client(0, 10).await;
    eventloop
        .state
        .handle_outgoing_packet(Request::PingReq)
        .unwrap();
    eventloop.state.events.clear();
    eventloop.keepalive_scan_deadline = Some(Instant::now() + Duration::from_secs(1));
    peer.write_all(QOS1_PUBLISH).await.unwrap();
    let first = eventloop.network.as_mut().unwrap().read_frame().await;
    let event = time::timeout(Duration::from_secs(2), eventloop.handle_network_read(first))
        .await
        .expect("ordinary reads must not renew an active keepalive response budget")
        .unwrap();
    assert!(matches!(event, Event::Incoming(Packet::Publish(_))));
    assert!(eventloop.network.is_none());
    assert!(eventloop.keepalive_scan_deadline.is_none());
    assert_eq!(
        eventloop.poll().await.unwrap(),
        Event::Outgoing(Outgoing::PubAck(7))
    );
    assert_write_timeout(eventloop.poll().await.unwrap_err());
}

#[cfg(feature = "ordered-shutdown")]
async fn assert_ordered_deadline_preserves_incoming_events(scan: bool) {
    for _ in 0..100 {
        let (client, mut eventloop, _old_peer) = blocked_client(0, 10).await;
        let (socket, mut peer) = tokio::io::duplex(16);
        let mut network = Network::new(socket, None);
        // The transport has room for the first PUBACK, but not the next one.
        for _ in 0..6 {
            network.write(Packet::PingReq).await.unwrap();
            network.flush().await.unwrap();
        }
        eventloop.network = Some(network);
        if scan {
            eventloop
                .state
                .handle_outgoing_packet(Request::PingReq)
                .unwrap();
            eventloop.state.events.clear();
            eventloop.keepalive_timeout = Some(Box::pin(time::sleep(Duration::ZERO)));
        } else {
            eventloop.options.set_read_batch_size(2);
            eventloop.keepalive_timeout = None;
        }
        let mut packets = QOS1_PUBLISH.to_vec();
        let mut second = QOS1_PUBLISH.to_vec();
        second[6] = 8;
        packets.extend(second);
        peer.write_all(&packets).await.unwrap();
        tokio::task::yield_now().await;
        let (outcome, notice) = {
            let polling = eventloop.poll();
            tokio::pin!(polling);
            let mut wire = [0; 16];
            tokio::select! {
                result = polling.as_mut() => match result {
                    // Normal read arbitration delivers the first publish. Repeat
                    // until the expired timer selects the multi-packet scan.
                    Ok(Event::Incoming(Packet::Publish(p))) if p.pkid == 7 => continue,
                    other => panic!("unexpected first poll: {other:?}"),
                },
                result = peer.read_exact(&mut wire) => { result.unwrap(); },
            }
            assert_eq!(&wire[12..], &[0x40, 2, 0, 7]);
            let notice = client
                .try_disconnect_after_queued_with_timeout(Duration::ZERO)
                .unwrap();
            (polling.await, notice)
        };
        assert!(matches!(outcome.unwrap(), Event::Incoming(Packet::Publish(p)) if p.pkid == 7));
        assert!(eventloop.network.is_none());
        assert!(eventloop.disconnect_complete);
        assert!(eventloop.requests_rx.gate().is_terminated());
        assert_eq!(eventloop.shutdown_phase, crate::ShutdownPhase::TimedOut);
        let (checkpoint_done, checkpoint_ready) = tokio::sync::oneshot::channel();
        eventloop.terminal_checkpoint = Some(Box::pin(async move {
            checkpoint_ready.await.unwrap();
            Ok(())
        }));
        assert!(matches!(
            notice.wait_async().now_or_never(),
            Some(Err(crate::DisconnectNoticeError::DisconnectTimeout))
        ));
        // Closing at expiry prevents the blocked second PUBACK from being sent.
        assert_eq!(peer.read(&mut [0; 4]).await.unwrap(), 0);
        time::timeout(Duration::from_secs(1), async {
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Outgoing(Outgoing::PubAck(7))
            );
            let event = eventloop.poll().await.unwrap();
            assert!(matches!(event, Event::Incoming(Packet::Publish(p)) if p.pkid == 8));
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Outgoing(Outgoing::PubAck(8))
            );
            assert!(matches!(
                eventloop.poll().await,
                Err(ConnectionError::OrderedDisconnect(
                    crate::DisconnectNoticeError::DisconnectTimeout
                ))
            ));
        })
        .await
        .expect("committed events and the terminal error must not wait for checkpoint completion");
        {
            let polling = eventloop.poll();
            tokio::pin!(polling);
            assert!(polling.as_mut().now_or_never().is_none());
            checkpoint_done.send(()).unwrap();
            assert!(matches!(polling.await, Err(ConnectionError::RequestsDone)));
        }
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::RequestsDone)
        ));
        assert!(eventloop.state.events.is_empty());
        return;
    }
    // Delivering the first publish before starting the next response is also safe.
}

#[cfg(feature = "ordered-shutdown")]
#[tokio::test(start_paused = true)]
async fn ordered_deadline_delivers_acknowledged_scan_events_before_terminal_error() {
    assert_ordered_deadline_preserves_incoming_events(true).await;
}

#[cfg(feature = "ordered-shutdown")]
#[tokio::test(start_paused = true)]
async fn ordered_deadline_delivers_acknowledged_read_batch_before_terminal_error() {
    assert_ordered_deadline_preserves_incoming_events(false).await;
}

#[cfg(feature = "ordered-shutdown")]
async fn assert_deadline_enforced_during_deferred_scan_drain(
    publishes: u8,
    expire_during_drain: bool,
) {
    let (client, mut eventloop, _old_peer) = blocked_client(0, 10).await;
    let (socket, mut peer) = tokio::io::duplex(1024);
    eventloop.network = Some(Network::new(socket, None));
    eventloop.handle_keepalive_ping().await.unwrap();
    assert_eq!(peer.read_u16().await.unwrap(), 0xc000);
    let mut expected_acks = Vec::new();
    for pkid in 7..7 + publishes {
        let mut packet = QOS1_PUBLISH.to_vec();
        packet[6] = pkid;
        peer.write_all(&packet).await.unwrap();
        expected_acks.extend([0x40, 2, 0, pkid]);
    }
    let event = eventloop.handle_keepalive_ping().await.unwrap();
    assert!(matches!(event, Event::Incoming(Packet::Publish(p)) if p.pkid == 7));
    assert!(eventloop.pending_connection_error.is_some());
    let mut acks = vec![0; expected_acks.len()];
    peer.read_exact(&mut acks).await.unwrap();
    assert_eq!(acks, expected_acks);
    let timeout = if expire_during_drain {
        Duration::from_secs(1)
    } else {
        Duration::ZERO
    };
    let notice = client
        .try_disconnect_after_queued_with_timeout(timeout)
        .unwrap();
    let deadline = notice.deadline().unwrap();
    let completion = notice.wait_async();
    tokio::pin!(completion);
    let mut delivered = Vec::new();
    if expire_during_drain {
        delivered.push(eventloop.poll().await.unwrap());
        assert_eq!(delivered[0], Event::Outgoing(Outgoing::PubAck(7)));
        assert!(eventloop.network.is_some());
        assert!(completion.as_mut().now_or_never().is_none());
        time::sleep_until(Instant::from_std(deadline)).await;
    }
    delivered.push(eventloop.poll().await.unwrap());
    assert!(
        eventloop.network.is_none(),
        "expiry must close before draining the next event"
    );
    assert!(eventloop.disconnect_complete);
    assert!(eventloop.requests_rx.gate().is_terminated());
    assert_eq!(eventloop.shutdown_phase, crate::ShutdownPhase::TimedOut);
    assert!(matches!(
        completion.as_mut().now_or_never(),
        Some(Err(crate::DisconnectNoticeError::DisconnectTimeout))
    ));
    assert_eq!(peer.read(&mut [0; 1]).await.unwrap(), 0);
    for _ in delivered.len()..(usize::from(publishes) * 2 - 1) {
        delivered.push(eventloop.poll().await.unwrap());
    }
    let mut delivered = delivered.into_iter();
    assert_eq!(
        delivered.next().unwrap(),
        Event::Outgoing(Outgoing::PubAck(7))
    );
    for pkid in 8..7 + publishes {
        let event = delivered.next().unwrap();
        assert!(matches!(event, Event::Incoming(Packet::Publish(p)) if p.pkid == u16::from(pkid)));
        assert_eq!(
            delivered.next().unwrap(),
            Event::Outgoing(Outgoing::PubAck(u16::from(pkid)))
        );
    }
    assert!(delivered.next().is_none());
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::OrderedDisconnect(
            crate::DisconnectNoticeError::DisconnectTimeout
        ))
    ));
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::RequestsDone)
    ));
    assert!(eventloop.state.events.is_empty());
}

#[cfg(feature = "ordered-shutdown")]
#[tokio::test]
async fn ordered_deadline_enforced_before_deferred_scan_events() {
    assert_deadline_enforced_during_deferred_scan_drain(3, false).await;
    assert_deadline_enforced_during_deferred_scan_drain(3, true).await;
}

#[cfg(feature = "ordered-shutdown")]
#[tokio::test]
async fn ordered_deadline_preserves_the_last_deferred_acknowledgement() {
    assert_deadline_enforced_during_deferred_scan_drain(1, false).await;
}
