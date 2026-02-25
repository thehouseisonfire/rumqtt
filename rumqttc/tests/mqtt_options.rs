use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::test]
async fn v4_custom_socket_connector_is_invoked() {
    let called = Arc::new(AtomicBool::new(false));
    let called_flag = called.clone();

    let mut options = rumqttc::MqttOptions::new("test-client", "127.0.0.1", 1883);
    options.set_socket_connector(move |_host, _network_options| {
        let called_flag = called_flag.clone();
        async move {
            called_flag.store(true, Ordering::Relaxed);
            Err::<tokio::net::TcpStream, io::Error>(io::Error::other(
                "forced socket connector failure",
            ))
        }
    });

    assert!(options.has_socket_connector());

    let (_client, mut eventloop) = rumqttc::AsyncClient::new(options, 10);
    let error = eventloop
        .poll()
        .await
        .expect_err("custom socket connector should force poll failure");

    assert!(matches!(error, rumqttc::ConnectionError::Io(_)));
    assert!(
        called.load(Ordering::Relaxed),
        "custom socket connector should have been called"
    );
}

#[tokio::test]
async fn v5_custom_socket_connector_is_invoked() {
    let called = Arc::new(AtomicBool::new(false));
    let called_flag = called.clone();

    let mut options = rumqttc::v5::MqttOptions::new("test-client-v5", "127.0.0.1", 1883);
    options.set_socket_connector(move |_host, _network_options| {
        let called_flag = called_flag.clone();
        async move {
            called_flag.store(true, Ordering::Relaxed);
            Err::<tokio::net::TcpStream, io::Error>(io::Error::other(
                "forced socket connector failure",
            ))
        }
    });

    assert!(options.has_socket_connector());

    let (_client, mut eventloop) = rumqttc::v5::AsyncClient::new(options, 10);
    let error = eventloop
        .poll()
        .await
        .expect_err("custom socket connector should force poll failure");

    assert!(matches!(error, rumqttc::v5::ConnectionError::Io(_)));
    assert!(
        called.load(Ordering::Relaxed),
        "custom socket connector should have been called"
    );
}
