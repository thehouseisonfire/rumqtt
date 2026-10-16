use futures_util::FutureExt;
use rumqttc::mqttbytes::v5::Packet;
use rumqttc::{EventLoop, MqttOptions};
use std::time::Duration;
use tokio::net::TcpListener;

#[allow(dead_code)]
mod broker;

#[tokio::test]
async fn buffered_packets_are_returned_in_order_without_waiting_for_socket_data() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut eventloop = EventLoop::new(
        MqttOptions::new(
            "broker-buffering",
            ("127.0.0.1", listener.local_addr().unwrap().port()),
        ),
        1,
    );
    let (connected, mut broker) = tokio::join!(
        eventloop.poll(),
        broker::Broker::from_listener(
            listener,
            broker::ConnectBehavior::Accept {
                session_saved: false,
            }
        )
    );
    connected.unwrap();

    // Model a bulk read while leaving the connected socket idle.
    broker.incoming.extend([Packet::PingReq, Packet::PingResp]);
    for expected in [Packet::PingReq, Packet::PingResp] {
        let packet = broker
            .read_packet_with_timeout(Duration::from_secs(3))
            .now_or_never()
            .expect("buffered packets must be returned immediately");
        assert_eq!(packet, Some(expected));
    }
    assert!(
        broker
            .read_packet_with_timeout(Duration::from_secs(3))
            .now_or_never()
            .is_none(),
        "an empty queue must wait for socket data"
    );
}
