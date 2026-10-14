use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use rumqttc::{
    ConnAck, ConnAckProperties, ConnectReturnCode, ConnectionError, Disconnect,
    DisconnectProperties, DisconnectReasonCode, Event, EventLoop, MqttOptions, Packet, StateError,
};
use tokio::io::{AsyncWriteExt, DuplexStream};

// Preserve callers that expose the observation API through their own const functions.
const fn take_failure(eventloop: &mut EventLoop) -> Option<Packet> {
    eventloop.take_connection_failure_packet()
}

fn eventloop_with_peers(count: usize) -> (EventLoop, Vec<DuplexStream>) {
    let (sockets, peers): (VecDeque<_>, Vec<_>) =
        (0..count).map(|_| tokio::io::duplex(4096)).unzip();
    let sockets = Arc::new(Mutex::new(sockets));
    let mut options = MqttOptions::new("failure-observation", "benchmark.invalid");
    options.set_socket_connector(move |_, _| {
        let socket = sockets.lock().unwrap().pop_front();
        async move { socket.ok_or_else(|| std::io::Error::other("no connection left")) }
    });
    (EventLoop::new(options, 1), peers)
}

async fn send(peer: &mut DuplexStream, packet: &Packet) {
    let mut bytes = BytesMut::new();
    packet.write(&mut bytes, None).unwrap();
    peer.write_all(&bytes).await.unwrap();
}

fn connack(code: ConnectReturnCode, properties: Option<ConnAckProperties>) -> Packet {
    Packet::ConnAck(ConnAck {
        session_present: false,
        code,
        properties,
    })
}

fn refused_connack() -> Packet {
    connack(
        ConnectReturnCode::NotAuthorized,
        Some(ConnAckProperties {
            session_expiry_interval: None,
            receive_max: None,
            max_qos: None,
            retain_available: None,
            max_packet_size: None,
            assigned_client_identifier: None,
            topic_alias_max: None,
            reason_string: Some("credentials refused".into()),
            user_properties: vec![
                ("detail".into(), "first".into()),
                ("detail".into(), "second".into()),
            ],
            wildcard_subscription_available: None,
            subscription_identifiers_available: None,
            shared_subscription_available: None,
            server_keep_alive: None,
            response_information: None,
            server_reference: None,
            authentication_method: None,
            authentication_data: None,
        }),
    )
}

#[tokio::test]
async fn broker_disconnect_observation_preserves_properties_and_clone_ownership() {
    let (mut eventloop, mut peers) = eventloop_with_peers(1);
    send(&mut peers[0], &connack(ConnectReturnCode::Success, None)).await;
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));

    let disconnect = Packet::Disconnect(Disconnect {
        reason_code: DisconnectReasonCode::NotAuthorized,
        properties: Some(DisconnectProperties {
            session_expiry_interval: None,
            reason_string: Some("permission revoked".into()),
            user_properties: vec![
                ("detail".into(), "first".into()),
                ("detail".into(), "second".into()),
            ],
            server_reference: Some("another.example:1883".into()),
        }),
    });
    send(&mut peers[0], &disconnect).await;
    assert_eq!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(disconnect.clone())
    );

    let (mut cloned, _) = eventloop_with_peers(0);
    cloned.state = eventloop.state.clone();
    assert_eq!(take_failure(&mut eventloop), Some(disconnect.clone()));
    assert_eq!(take_failure(&mut eventloop), None);
    assert_eq!(take_failure(&mut cloned), Some(disconnect));
    assert_eq!(take_failure(&mut cloned), None);

    cloned.state = eventloop.state.clone();
    assert_eq!(take_failure(&mut cloned), None);
    assert!(matches!(
        eventloop.poll().await.unwrap_err(),
        ConnectionError::MqttState(StateError::ServerDisconnect {
            reason_code: DisconnectReasonCode::NotAuthorized,
            reason_string: Some(reason),
        }) if reason == "permission revoked"
    ));
    assert_eq!(take_failure(&mut eventloop), None);
}

#[tokio::test]
async fn rejected_connack_observation_is_owned_and_consumed_once_after_poll_error() {
    let (mut eventloop, mut peers) = eventloop_with_peers(1);
    let rejected = refused_connack();
    send(&mut peers[0], &rejected).await;
    assert!(matches!(
        eventloop.poll().await.unwrap_err(),
        ConnectionError::ConnectionRefused(ConnectReturnCode::NotAuthorized)
    ));
    assert_eq!(take_failure(&mut eventloop), Some(rejected));
    assert_eq!(take_failure(&mut eventloop), None);
}

#[tokio::test]
async fn a_new_connection_clears_an_unconsumed_failure_observation() {
    let (mut eventloop, mut peers) = eventloop_with_peers(2);
    send(&mut peers[0], &refused_connack()).await;
    assert!(matches!(
        eventloop.poll().await.unwrap_err(),
        ConnectionError::ConnectionRefused(ConnectReturnCode::NotAuthorized)
    ));

    send(&mut peers[1], &connack(ConnectReturnCode::Success, None)).await;
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Packet::ConnAck(_))
    ));
    assert_eq!(take_failure(&mut eventloop), None);
}
