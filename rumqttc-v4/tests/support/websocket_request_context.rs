use super::{process_websocket_connection, websocket_handshake_callback};
#[cfg(all(
    feature = "use-rustls-no-provider",
    any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
))]
use rumqttc::Transport;
use rumqttc::{AsyncClient, Broker, Event, Incoming, MqttOptions, WebSocketRequestContext};
use std::sync::{Arc, Mutex, atomic::AtomicUsize};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::accept_hdr_async;

trait TestStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TestStream for T {}

#[tokio::test]
async fn both_request_modifiers_preserve_routing_and_the_original_deadline() {
    for encrypted in [false, true] {
        if encrypted
            && !cfg!(all(
                feature = "use-rustls-no-provider",
                any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
            ))
        {
            continue;
        }
        for proxied in [false, true] {
            if proxied && !cfg!(feature = "http-proxy") {
                continue;
            }
            for fallible in [false, true] {
                timeout(
                    Duration::from_secs(5),
                    check_request_modifier_context(encrypted, proxied, fallible),
                )
                .await
                .unwrap();
            }
        }
    }
}

#[expect(clippy::result_large_err, reason = "tungstenite callback API")]
async fn check_request_modifier_context(encrypted: bool, proxied: bool, fallible: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    #[cfg(all(
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
    ))]
    let (tls_acceptor, ca) = super::make_rustls_wss_acceptor_and_ca();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        if proxied {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
            assert!(headers.starts_with(format!("CONNECT localhost:{port} ").as_bytes()));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
        }
        #[cfg(all(
            feature = "use-rustls-no-provider",
            any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
        ))]
        let stream: Box<dyn TestStream> = if encrypted {
            Box::new(tls_acceptor.accept(stream).await.unwrap())
        } else {
            Box::new(stream)
        };
        #[cfg(not(all(
            feature = "use-rustls-no-provider",
            any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
        )))]
        let stream: Box<dyn TestStream> = Box::new(stream);
        let mut websocket = accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.uri().path_and_query().unwrap().as_str(),
                    "/signed?attempt=1"
                );
                assert_eq!(request.headers()["x-context"], "verified");
                websocket_handshake_callback(request, response)
            },
        )
        .await
        .unwrap();
        process_websocket_connection(&mut websocket, &AtomicUsize::new(0), None).await;
    });

    let mut options = MqttOptions::new(
        "request-context",
        Broker::websocket(format!("ws://localhost:{port}/original")).unwrap(),
    );
    #[cfg(all(
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc")
    ))]
    if encrypted {
        options.set_transport(Transport::wss(ca, None, None));
    }
    #[cfg(feature = "http-proxy")]
    if proxied {
        options.set_proxy(rumqttc::Proxy::http("::1", 3128).with_credentials("user", "secret"));
    }
    let dial_target = if proxied {
        "[::1]:3128".to_owned()
    } else {
        format!("localhost:{port}")
    };
    let deadline = Arc::new(Mutex::new(None::<Instant>));
    let connector_deadline = deadline.clone();
    let expected_target = dial_target.clone();
    options.set_socket_connector(move |endpoint, network_options| {
        assert_eq!(endpoint, expected_target);
        *connector_deadline.lock().unwrap() = network_options.connection_deadline();
        async move {
            // Consume some of the attempt budget before the modifier runs.
            sleep(Duration::from_millis(20)).await;
            TcpStream::connect(("127.0.0.1", port)).await
        }
    });
    let modify = move |mut request: http::Request<()>| {
        let context = request
            .extensions()
            .get::<WebSocketRequestContext>()
            .unwrap();
        assert_eq!(context.broker_host, "localhost");
        assert_eq!(context.broker_port, port);
        assert_eq!(context.dial_target, dial_target);
        assert_eq!(
            context.tls_authority.as_deref(),
            encrypted.then_some("localhost")
        );
        let original_deadline = deadline.lock().unwrap().unwrap();
        assert_eq!(context.deadline, Some(original_deadline));
        assert!(original_deadline > Instant::now());
        assert_eq!(request.uri().path(), "/original");
        *request.uri_mut() = format!("ws://localhost:{port}/signed?attempt=1")
            .parse()
            .unwrap();
        request
            .headers_mut()
            .insert("x-context", "verified".parse().unwrap());
        request
    };
    if fallible {
        options.set_fallible_request_modifier(move |request| {
            let request = modify(request);
            async move { Ok::<_, std::io::Error>(request) }
        });
    } else {
        options.set_request_modifier(move |request| {
            let request = modify(request);
            async move { request }
        });
    }
    let (client, mut eventloop) = AsyncClient::builder(options).capacity(10).build();
    assert!(matches!(
        eventloop.poll().await.unwrap(),
        Event::Incoming(Incoming::ConnAck(_))
    ));
    client.disconnect().await.unwrap();
    eventloop.poll().await.unwrap();
    server.await.unwrap();
}
