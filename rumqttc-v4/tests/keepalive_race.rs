use std::sync::{Arc, Mutex};
use std::time::Duration;

use rumqttc::mqttbytes::v4::Packet;
use rumqttc::{AsyncClient, ConnectionError, Event, EventLoop, MqttOptions, Outgoing, StateError};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const PUBLISH: &[u8] = &[0x30, 3, 0, 1, b'a'];
const QOS1_PUBLISH: &[u8] = &[0x32, 5, 0, 1, b'a', 0, 7];

async fn read_packet(peer: &mut DuplexStream) -> Vec<u8> {
    let header = peer.read_u8().await.unwrap();
    let len = peer.read_u8().await.unwrap();
    assert!(len < 128, "test packets use a one-byte remaining length");
    let mut bytes = vec![header, len];
    bytes.resize(usize::from(len) + 2, 0);
    peer.read_exact(&mut bytes[2..]).await.unwrap();
    bytes
}

async fn connected_with_outstanding_ping() -> (AsyncClient, EventLoop, DuplexStream) {
    let (socket, mut peer) = tokio::io::duplex(4096);
    peer.write_all(&[0x20, 2, 0, 0]).await.unwrap();
    let socket = Arc::new(Mutex::new(Some(socket)));
    let mut options = MqttOptions::new("ping-response", "localhost");
    options.set_keep_alive(1).set_read_batch_size(1);
    options.set_socket_connector(move |_, _| {
        let socket = socket.lock().unwrap().take().unwrap();
        async move { Ok::<_, std::io::Error>(socket) }
    });
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
    assert!(matches!(
        eventloop.poll().await,
        Ok(Event::Incoming(Packet::ConnAck(_)))
    ));
    assert_eq!(read_packet(&mut peer).await[0], 0x10);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        eventloop.poll().await,
        Ok(Event::Outgoing(Outgoing::PingReq))
    ));
    assert_eq!(read_packet(&mut peer).await, [0xc0, 0]);
    (client, eventloop, peer)
}

#[tokio::test(start_paused = true)]
async fn ready_pingresp_survives_an_expired_keepalive_timer() {
    // Exercise randomized arbitration as well as the timer handler's deterministic
    // unit test: the packet is buffered before the simulated executor stall.
    for _ in 0..100 {
        let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
        peer.write_all(&[0xd0, 0]).await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Incoming(Packet::PingResp))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn ready_pingresp_survives_cooperative_budget_exhaustion() {
    for _ in 0..100 {
        let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
        peer.write_all(&[0xd0, 0]).await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        std::future::poll_fn(|cx| {
            loop {
                let budget = std::task::ready!(tokio::task::coop::poll_proceed(cx));
                if !tokio::task::coop::has_budget_remaining() {
                    // Drop the uncommitted permit to leave exactly one budget unit,
                    // which the expired timer can consume before probing PINGRESP.
                    return std::task::Poll::Ready(());
                }
                budget.made_progress();
            }
        })
        .await;
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Incoming(Packet::PingResp))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn ready_pingresp_survives_multiple_scan_batches() {
    for queued in [128, 129, 513] {
        for _ in 0..100 {
            let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
            peer.write_all(&PUBLISH.repeat(queued)).await.unwrap();
            peer.write_all(&[0xd0, 0]).await.unwrap();
            tokio::time::advance(Duration::from_secs(2)).await;
            for _ in 0..queued {
                assert!(matches!(
                    eventloop.poll().await.unwrap(),
                    Event::Incoming(Packet::Publish(_))
                ));
            }
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Incoming(Packet::PingResp)
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn acknowledged_publishes_survive_multiple_scan_batches() {
    for _ in 0..100 {
        let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
        for pkid in 1..=129u8 {
            let mut packet = QOS1_PUBLISH.to_vec();
            packet[6] = pkid;
            peer.write_all(&packet).await.unwrap();
        }
        peer.write_all(&[0xd0, 0]).await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        for pkid in 1..=129u8 {
            let event = eventloop.poll().await.unwrap();
            assert!(
                matches!(event, Event::Incoming(Packet::Publish(publish)) if publish.pkid == u16::from(pkid))
            );
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Outgoing(Outgoing::PubAck(u16::from(pkid)))
            );
            assert_eq!(read_packet(&mut peer).await, [0x40, 2, 0, pkid]);
        }
        assert_eq!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::PingResp)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn ready_pingresp_after_a_publish_preserves_the_publish_and_ack() {
    for _ in 0..100 {
        let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
        peer.write_all(QOS1_PUBLISH).await.unwrap();
        peer.write_all(&[0xd0, 0]).await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Incoming(Packet::Publish(_)))
        ));
        assert_eq!(read_packet(&mut peer).await, [0x40, 2, 0, 7]);
        // Preserve the public outgoing acknowledgement before delivering PINGRESP.
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Outgoing(Outgoing::PubAck(7)))
        ));
        assert!(matches!(
            eventloop.poll().await,
            Ok(Event::Incoming(Packet::PingResp))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn missing_pingresp_still_times_out() {
    let (_client, mut eventloop, _peer) = connected_with_outstanding_ping().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::MqttState(StateError::AwaitPingResp))
    ));
}

#[tokio::test(start_paused = true)]
async fn partial_pingresp_does_not_wait_past_the_timeout() {
    let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
    peer.write_all(&[0xd0]).await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        eventloop.poll().await,
        Err(ConnectionError::MqttState(StateError::AwaitPingResp))
    ));
}

#[tokio::test(start_paused = true)]
async fn continuous_unrelated_packets_cannot_hide_a_missing_pingresp() {
    let (_client, mut eventloop, mut peer) = connected_with_outstanding_ping().await;
    let mut network_options = rumqttc::NetworkOptions::new();
    network_options.set_connection_timeout(1);
    eventloop.set_network_options(network_options);
    peer.write_all(&PUBLISH.repeat(256)).await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..512 {
        // Keep packets ready across scan batches while the same deadline elapses.
        tokio::time::advance(Duration::from_millis(5)).await;
        peer.write_all(PUBLISH).await.unwrap();
        match eventloop.poll().await {
            Err(ConnectionError::MqttState(StateError::AwaitPingResp)) => return,
            Ok(Event::Incoming(Packet::Publish(_))) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
    panic!("incoming traffic postponed the ping-response timeout indefinitely");
}

#[tokio::test(start_paused = true)]
async fn acknowledged_publishes_are_delivered_before_timeout_and_fresh_reconnect() {
    for _ in 0..100 {
        let (socket, mut peer) = tokio::io::duplex(4096);
        let (reconnect_socket, mut reconnect_peer) = tokio::io::duplex(4096);
        peer.write_all(&[0x20, 2, 0, 0]).await.unwrap();
        reconnect_peer.write_all(&[0x20, 2, 0, 0]).await.unwrap();
        let sockets = Arc::new(Mutex::new(std::collections::VecDeque::from([
            socket,
            reconnect_socket,
        ])));
        let mut options = MqttOptions::new("ping-response", "localhost");
        options.set_keep_alive(1).set_read_batch_size(1);
        options.set_socket_connector(move |_, _| {
            let socket = sockets
                .lock()
                .unwrap()
                .pop_front()
                .expect("only two connections expected");
            async move { Ok::<_, std::io::Error>(socket) }
        });
        let (_client, mut eventloop) = AsyncClient::builder(options).capacity(4).build();
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        assert_eq!(read_packet(&mut peer).await[0], 0x10);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            eventloop.poll().await.unwrap(),
            Event::Outgoing(Outgoing::PingReq)
        );
        assert_eq!(read_packet(&mut peer).await, [0xc0, 0]);
        for pkid in 1..=3u8 {
            let mut packet = QOS1_PUBLISH.to_vec();
            packet[6] = pkid;
            peer.write_all(&packet).await.unwrap();
        }
        tokio::time::advance(Duration::from_secs(2)).await;

        for pkid in 1..=3u8 {
            assert!(
                matches!(eventloop.poll().await.unwrap(), Event::Incoming(Packet::Publish(publish)) if publish.pkid == u16::from(pkid))
            );
            assert_eq!(
                eventloop.poll().await.unwrap(),
                Event::Outgoing(Outgoing::PubAck(u16::from(pkid)))
            );
            assert_eq!(read_packet(&mut peer).await, [0x40, 2, 0, pkid]);
        }
        assert!(matches!(
            eventloop.poll().await,
            Err(ConnectionError::MqttState(StateError::AwaitPingResp))
        ));
        assert!(matches!(
            eventloop.poll().await.unwrap(),
            Event::Incoming(Packet::ConnAck(_))
        ));
        assert_eq!(read_packet(&mut reconnect_peer).await[0], 0x10);
        assert!(eventloop.state.events.is_empty());
    }
}
