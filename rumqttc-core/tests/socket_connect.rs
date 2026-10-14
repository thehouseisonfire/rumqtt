use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use rumqttc_core::{NetworkOptions, default_socket_connect};
use tokio::net::TcpListener;

#[tokio::test]
async fn public_dialer_preserves_socket_options_and_ephemeral_source_binding() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let target = listener.local_addr().unwrap();
    let mut options = NetworkOptions::new();
    options.set_bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    options.set_tcp_nodelay(true);
    options.set_tcp_send_buffer_size(8192);
    options.set_tcp_recv_buffer_size(8192);
    let stream = tokio::time::timeout(
        Duration::from_secs(5),
        default_socket_connect(target.to_string(), options),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stream.peer_addr().unwrap(), target);
    assert_eq!(stream.local_addr().unwrap().ip(), Ipv4Addr::LOCALHOST);
    assert!(stream.nodelay().unwrap());
    let (_, peer) = listener.accept().await.unwrap();
    assert_eq!(peer, stream.local_addr().unwrap());
}

#[tokio::test]
async fn public_dialer_preserves_fixed_source_port() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let target = listener.local_addr().unwrap();
    let reserved = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let source = reserved.local_addr().unwrap();
    drop(reserved);
    let mut options = NetworkOptions::new();
    options.set_bind_addr(source);
    let stream = tokio::time::timeout(
        Duration::from_secs(5),
        default_socket_connect(target.to_string(), options),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stream.local_addr().unwrap(), source);
    let (_, peer) = listener.accept().await.unwrap();
    assert_eq!(peer, source);
}

#[tokio::test]
async fn public_dialer_connects_to_ipv6_literal_when_available() {
    let Ok(listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await else {
        return;
    };
    let target = listener.local_addr().unwrap();
    let stream = tokio::time::timeout(
        Duration::from_secs(5),
        default_socket_connect(target.to_string(), NetworkOptions::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stream.peer_addr().unwrap(), target);
}
