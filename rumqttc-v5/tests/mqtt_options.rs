#[cfg(not(feature = "websocket"))]
use std::io;
#[cfg(not(feature = "websocket"))]
use std::sync::Arc;
#[cfg(not(feature = "websocket"))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(feature = "websocket"))]
#[tokio::test]
async fn v5_custom_socket_connector_is_invoked() {
    let called = Arc::new(AtomicBool::new(false));
    let called_flag = Arc::clone(&called);

    let mut options = rumqttc::MqttOptions::new("test-client-v5", "127.0.0.1");
    options.set_socket_connector(move |_host, _network_options| {
        let called_flag = Arc::clone(&called_flag);
        async move {
            called_flag.store(true, Ordering::Relaxed);
            Err::<tokio::net::TcpStream, io::Error>(io::Error::other(
                "forced socket connector failure",
            ))
        }
    });

    assert!(options.has_socket_connector());

    let (_client, mut eventloop) = rumqttc::AsyncClient::builder(options).capacity(10).build();
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

#[cfg(feature = "socks-proxy")]
async fn spawn_socks5_mqtt_peer() -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();

        assert_eq!(stream.read_u8().await.unwrap(), 5);
        let method_count = stream.read_u8().await.unwrap() as usize;
        let mut methods = vec![0; method_count];
        stream.read_exact(&mut methods).await.unwrap();
        assert_eq!(methods, [0]);
        stream.write_all(&[5, 0]).await.unwrap();

        assert_eq!(stream.read_u8().await.unwrap(), 5);
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        assert_eq!(stream.read_u8().await.unwrap(), 0);
        assert_eq!(stream.read_u8().await.unwrap(), 3);
        let domain_len = stream.read_u8().await.unwrap() as usize;
        let mut domain = vec![0; domain_len];
        stream.read_exact(&mut domain).await.unwrap();
        assert_eq!(&domain, b"broker.proxy.test");
        assert_eq!(stream.read_u16().await.unwrap(), 1883);
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();

        let mut connect = [0; 1024];
        let read = stream.read(&mut connect).await.unwrap();
        assert!(read > 0);
        assert_eq!(connect[0], 0x10);
        stream
            .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
            .await
            .unwrap();
    });
    (port, task)
}

#[cfg(feature = "socks-proxy")]
#[tokio::test]
async fn v5_connects_through_socks5_with_remote_dns() {
    use rumqttc::mqttbytes::v5::Packet;
    use rumqttc::{Event, Proxy};
    use tokio::time::{Duration, timeout};

    let (proxy_port, peer) = spawn_socks5_mqtt_peer().await;
    let mut options = rumqttc::MqttOptions::new("socks-v5", "broker.proxy.test");
    options.set_proxy(Proxy::socks5("127.0.0.1", proxy_port));
    let (_client, mut eventloop) = rumqttc::AsyncClient::builder(options).capacity(10).build();

    let event = timeout(Duration::from_secs(3), eventloop.poll())
        .await
        .expect("SOCKS5 connection timed out")
        .expect("SOCKS5 connection failed");
    assert!(matches!(event, Event::Incoming(Packet::ConnAck(_))));
    peer.await.unwrap();
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
async fn v5_fallible_request_modifier_error_propagates() {
    use rumqttc::{Broker, ConnectionError, EventLoop, MqttOptions};
    use tokio::time::{Duration, timeout};

    let (port, listener_task) = spawn_listener().await;

    let mut options = MqttOptions::new(
        "test-v5",
        Broker::websocket(format!("ws://127.0.0.1:{port}/mqtt")).expect("valid websocket broker"),
    );
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
    match listener_task.await {
        Ok(()) | Err(_) => {}
    }
}
