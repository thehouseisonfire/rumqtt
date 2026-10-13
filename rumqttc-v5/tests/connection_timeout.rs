use std::time::{Duration, Instant};

use rumqttc::{Event, EventLoop, MqttOptions, Packet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn connect_with_timeout(timeout: Duration) -> Instant {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut options = MqttOptions::new("timeout", "localhost");
    options.set_connect_timeout(timeout);
    options.set_socket_connector(move |_, network_options| {
        let (client, peer) = tokio::io::duplex(4096);
        tx.send((peer, network_options.connection_deadline().unwrap()))
            .unwrap();
        async move { Ok::<_, std::io::Error>(client) }
    });
    let mut eventloop = EventLoop::new(options, 1);

    let peer = tokio::spawn(async move {
        let (mut peer, deadline) = rx.recv().await.unwrap();
        assert_eq!(peer.read_u8().await.unwrap(), 0x10);
        let remaining = peer.read_u8().await.unwrap();
        assert!(remaining < 128, "test CONNECT fits a one-byte length");
        peer.read_exact(&mut vec![0; usize::from(remaining)])
            .await
            .unwrap();
        peer.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
            .await
            .unwrap();
        deadline
    });
    let event = tokio::time::timeout(Duration::from_secs(3), eventloop.poll())
        .await
        .expect("connection should finish promptly")
        .expect("connection should succeed");
    assert!(matches!(event, Event::Incoming(Packet::ConnAck(_))));
    peer.await.unwrap()
}

#[tokio::test]
async fn overflowing_connection_timeout_connects_with_tokios_far_future_deadline() {
    for timeout in [Duration::from_secs(u64::MAX), Duration::MAX] {
        assert!(Instant::now().checked_add(timeout).is_none());
        let earliest = tokio::time::sleep(timeout).deadline().into_std();
        let deadline = connect_with_timeout(timeout).await;
        let latest = tokio::time::sleep(timeout).deadline().into_std();
        assert!(deadline >= earliest && deadline <= latest);
    }
}

#[tokio::test]
async fn finite_connection_timeout_preserves_the_connector_deadline() {
    let timeout = Duration::from_secs(5);
    let earliest = Instant::now() + timeout;
    let deadline = connect_with_timeout(timeout).await;
    let latest = Instant::now() + timeout;
    assert!(deadline >= earliest && deadline <= latest);
}
