#[cfg(not(feature = "websocket"))]
use std::io;
#[cfg(not(feature = "websocket"))]
use std::sync::Arc;
#[cfg(not(feature = "websocket"))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "websocket"))]
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

#[cfg(not(feature = "websocket"))]
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

#[cfg(feature = "websocket")]
#[derive(Debug)]
struct RequestModifierTestError(&'static str);

#[cfg(feature = "websocket")]
impl std::fmt::Display for RequestModifierTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(feature = "websocket")]
impl std::error::Error for RequestModifierTestError {}

#[cfg(feature = "websocket")]
async fn spawn_listener() -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let port = listener
        .local_addr()
        .expect("listener should have addr")
        .port();
    let task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });

    (port, task)
}

#[cfg(feature = "websocket")]
#[tokio::test]
async fn v4_fallible_request_modifier_error_propagates() {
    use rumqttc::{ConnectionError, EventLoop, MqttOptions, Transport};
    use tokio::time::{Duration, timeout};

    let (port, listener_task) = spawn_listener().await;

    let mut options = MqttOptions::new("test-v4", format!("ws://127.0.0.1:{port}/mqtt"), port);
    options.set_transport(Transport::Ws);
    options.set_fallible_request_modifier(|_req| async move {
        Err(RequestModifierTestError("modifier failed intentionally"))
    });

    let mut eventloop = EventLoop::new(options, 10);
    let poll_result = timeout(Duration::from_secs(3), eventloop.poll()).await;

    match poll_result {
        Ok(Err(ConnectionError::RequestModifier(error))) => {
            assert!(error.to_string().contains("modifier failed intentionally"));
        }
        Ok(Err(other)) => panic!("expected RequestModifier error, got {other:?}"),
        Ok(Ok(event)) => panic!("expected error, got event {event:?}"),
        Err(_) => panic!("poll timed out"),
    }

    listener_task.abort();
    let _ = listener_task.await;
}

#[cfg(feature = "websocket")]
#[tokio::test]
async fn v5_fallible_request_modifier_error_propagates() {
    use rumqttc::Transport;
    use rumqttc::v5::{ConnectionError, EventLoop, MqttOptions};
    use tokio::time::{Duration, timeout};

    let (port, listener_task) = spawn_listener().await;

    let mut options = MqttOptions::new("test-v5", format!("ws://127.0.0.1:{port}/mqtt"), port);
    options.set_transport(Transport::Ws);
    options.set_fallible_request_modifier(|_req| async move {
        Err(RequestModifierTestError("modifier failed intentionally"))
    });

    let mut eventloop = EventLoop::new(options, 10);
    let poll_result = timeout(Duration::from_secs(3), eventloop.poll()).await;

    match poll_result {
        Ok(Err(ConnectionError::RequestModifier(error))) => {
            assert!(error.to_string().contains("modifier failed intentionally"));
        }
        Ok(Err(other)) => panic!("expected RequestModifier error, got {other:?}"),
        Ok(Ok(event)) => panic!("expected error, got event {event:?}"),
        Err(_) => panic!("poll timed out"),
    }

    listener_task.abort();
    let _ = listener_task.await;
}
