mod support;
#[allow(dead_code)]
#[path = "support/tls.rs"]
mod tls;
use bytes::Bytes;
use futures_util::StreamExt;
use rumqttc_wrapper_core::*;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use support::*;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}
type Stream = Box<dyn Duplex>;

struct MemoryIo {
    read: Arc<tokio::sync::Mutex<tokio::io::ReadHalf<DuplexStream>>>,
    write: Arc<tokio::sync::Mutex<tokio::io::WriteHalf<DuplexStream>>>,
}
impl TransportIo for MemoryIo {
    fn read(&self, max: usize) -> TransportIoFuture<Bytes> {
        let read = self.read.clone();
        Box::pin(async move {
            let mut data = vec![0; max.min(7)];
            let count = read.lock().await.read(&mut data).await?;
            data.truncate(count);
            Ok(Bytes::from(data))
        })
    }
    fn write(&self, bytes: Bytes) -> TransportIoFuture<usize> {
        let write = self.write.clone();
        Box::pin(async move {
            write
                .lock()
                .await
                .write(&bytes[..bytes.len().min(11)])
                .await
        })
    }
    fn flush(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.flush().await })
    }
    fn shutdown(&self) -> TransportIoFuture<()> {
        let write = self.write.clone();
        Box::pin(async move { write.lock().await.shutdown().await })
    }
}
#[derive(Clone)]
struct Profile {
    encrypted: bool,
    websocket: bool,
    proxy: u8,
}
struct MemoryConnector {
    requests: Arc<Mutex<Vec<TransportRequest>>>,
    profile: Profile,
    server: Arc<rustls::ServerConfig>,
    release: Arc<tokio::sync::Notify>,
}
impl TransportConnector for MemoryConnector {
    fn connect(&self, request: TransportRequest) -> TransportFuture {
        self.requests.lock().unwrap().push(request.clone());
        let profile = self.profile.clone();
        let server_tls = self.server.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(16384);
            tokio::spawn(async move { serve(server, request, profile, server_tls, release).await });
            let (read, write) = tokio::io::split(client);
            Ok(TransportConnection {
                io: Arc::new(MemoryIo {
                    read: Arc::new(tokio::sync::Mutex::new(read)),
                    write: Arc::new(tokio::sync::Mutex::new(write)),
                }),
                mode: TransportMode::Base,
                network_handling: NetworkHandling::NotApplicable,
            })
        })
    }
}
async fn packet(stream: &mut Stream) -> Bytes {
    let header = stream.read_u8().await.unwrap();
    let mut data = vec![header];
    let mut length = 0;
    for shift in [0, 7, 14, 21] {
        let byte = stream.read_u8().await.unwrap();
        data.push(byte);
        length |= usize::from(byte & 127) << shift;
        if byte < 128 {
            let start = data.len();
            data.resize(start + length, 0);
            stream.read_exact(&mut data[start..]).await.unwrap();
            return data.into();
        }
    }
    panic!("invalid fixture frame")
}
async fn proxy(stream: &mut Stream, kind: u8) {
    if kind == 3 {
        // SOCKS5 without authentication.
        assert_eq!(stream.read_u8().await.unwrap(), 5);
        let length = stream.read_u8().await.unwrap();
        let mut methods = vec![0; usize::from(length)];
        stream.read_exact(&mut methods).await.unwrap();
        assert!(methods.contains(&0));
        stream.write_all(&[5, 0]).await.unwrap();
        let mut header = [0; 4];
        stream.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..3], &[5, 1, 0]);
        let length = match header[3] {
            1 => 4,
            3 => usize::from(stream.read_u8().await.unwrap()),
            4 => 16,
            _ => panic!("invalid SOCKS address"),
        };
        let mut target = vec![0; length + 2];
        stream.read_exact(&mut target).await.unwrap();
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
    } else {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 4096);
        }
        assert!(request.starts_with(b"CONNECT localhost:1883 HTTP/1.1\r\n"));
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    }
}
#[expect(
    clippy::result_large_err,
    reason = "tungstenite handshake callback error type"
)]
async fn serve(
    server: DuplexStream,
    request: TransportRequest,
    profile: Profile,
    tls: Arc<rustls::ServerConfig>,
    release: Arc<tokio::sync::Notify>,
) {
    let mut stream: Stream = Box::new(server);
    if profile.proxy == 2 {
        stream = Box::new(
            tokio_rustls::TlsAcceptor::from(tls.clone())
                .accept(stream)
                .await
                .unwrap(),
        );
    }
    if profile.proxy > 0 {
        proxy(&mut stream, profile.proxy).await;
    }
    if profile.encrypted {
        stream = Box::new(
            tokio_rustls::TlsAcceptor::from(tls)
                .accept(stream)
                .await
                .unwrap(),
        );
    }
    let mqtt5 = request.protocol == ProtocolVersion::V5;
    let connack: &[u8] = if mqtt5 {
        &[0x20, 3, 0, 0, 0]
    } else {
        &[0x20, 2, 0, 0]
    };
    if profile.websocket {
        let mut socket = async_tungstenite::tokio::accept_hdr_async(
            stream,
            |_: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                response
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
                Ok(response)
            },
        )
        .await
        .unwrap();
        let connect = socket.next().await.unwrap().unwrap().into_data();
        assert_eq!(connect[0], 0x10);
        socket
            .send(tungstenite::Message::Binary(Bytes::copy_from_slice(
                connack,
            )))
            .await
            .unwrap();
        // Reconnect composition is tested independently of MQTT tracking here.
        if request.generation == 1 {
            release.notified().await;
        } else {
            let packet = socket.next().await.unwrap().unwrap().into_data();
            assert_eq!(packet[0] >> 4, 14);
        }
    } else {
        assert_eq!(packet(&mut stream).await[0], 0x10);
        stream.write_all(connack).await.unwrap();
        stream.flush().await.unwrap();
        if request.generation == 1 {
            release.notified().await;
        } else {
            assert_eq!(packet(&mut stream).await[0] >> 4, 14);
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Keep the scenario setup, actions, and assertions together"
)]
fn native_client_composes_and_reconnects_over_short_memory_io() {
    let fixture = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for backend in [TlsBackend::Rustls, TlsBackend::Native] {
            let tls_enabled = match backend {
                TlsBackend::Rustls => cfg!(feature = "use-rustls-no-provider"),
                TlsBackend::Native => cfg!(feature = "use-native-tls"),
            };
            for encrypted in [false, true] {
                if encrypted && !tls_enabled {
                    continue;
                }
                if !encrypted && backend == TlsBackend::Native {
                    continue;
                }
                for websocket in [false, true] {
                    if websocket && !cfg!(feature = "websocket") {
                        continue;
                    }
                    for proxy in 0..=3 {
                        if (proxy == 1 || proxy == 2) && !cfg!(feature = "http-proxy") {
                            continue;
                        }
                        if proxy == 2 && !tls_enabled {
                            continue;
                        }
                        if proxy == 3 && !cfg!(feature = "socks-proxy") {
                            continue;
                        }
                        let requests = Arc::new(Mutex::new(Vec::new()));
                        let release = Arc::new(tokio::sync::Notify::new());
                        let connector = Arc::new(MemoryConnector {
                            requests: requests.clone(),
                            profile: Profile {
                                encrypted,
                                websocket,
                                proxy,
                            },
                            server: fixture.server.clone(),
                            release: release.clone(),
                        });
                        let weak = Arc::downgrade(&connector);
                        let mut config = config(mqtt5, 1883);
                        config.common.broker = BrokerTarget::Tcp {
                            host: "localhost".into(),
                            port: 1883,
                        };
                        config.common.connector = Some(TransportConnectorConfig {
                            connector,
                            mode: TransportMode::Base,
                        });
                        config.common.transport = match (encrypted, websocket) {
                            (false, false) => TransportConfig::Tcp,
                            (true, false) => TransportConfig::Tls(fixture.client(backend)),
                            (false, true) => TransportConfig::WebSocket,
                            (true, true) => TransportConfig::Wss(fixture.client(backend)),
                        };
                        if websocket {
                            config.common.broker = BrokerTarget::WebSocket {
                                url: format!(
                                    "{}://localhost:1883/mqtt",
                                    if encrypted { "wss" } else { "ws" }
                                ),
                            };
                        }
                        if proxy > 0 {
                            config.common.proxy = Some(if proxy == 3 {
                                ProxyConfig::Socks5 {
                                    host: "proxy.invalid".into(),
                                    port: 3128,
                                    credentials: None,
                                }
                            } else {
                                ProxyConfig::Http {
                                    host: "proxy.invalid".into(),
                                    port: 3128,
                                    credentials: None,
                                    tls: (proxy == 2).then(|| fixture.client(backend)),
                                }
                            });
                        }
                        // The HTTPS proxy's hostname must be covered by its certificate.
                        if proxy == 2
                            && let Some(ProxyConfig::Http { host, .. }) = &mut config.common.proxy
                        {
                            *host = "localhost".into();
                        }
                        let started = Instant::now();
                        let mut client = support::start(config).unwrap();
                        let mut events = connected(&mut client);
                        release.notify_one();
                        until(&mut events, |event| {
                            matches!(event, WrapperEvent::Disconnected { .. })
                        });
                        until(&mut events, |event| {
                            matches!(event, WrapperEvent::Connected { .. })
                        });
                        client.closer().close(DEADLINE).unwrap();
                        drop(client);
                        assert!(weak.upgrade().is_none());
                        let requests = requests.lock().unwrap();
                        assert_eq!(requests.len(), 2);
                        for (index, request) in requests.iter().enumerate() {
                            assert_eq!(request.generation, index as u64 + 1);
                            assert!(request.deadline > started);
                            assert_eq!(
                                request.target,
                                if proxy == 0 {
                                    "localhost:1883"
                                } else if proxy == 2 {
                                    "localhost:3128"
                                } else {
                                    "proxy.invalid:3128"
                                }
                            );
                        }
                    }
                }
            }
        }
    }
}

struct RejectTlsConnector {
    server: Arc<rustls::ServerConfig>,
    rejected: Arc<std::sync::atomic::AtomicUsize>,
}
impl TransportConnector for RejectTlsConnector {
    fn connect(&self, _: TransportRequest) -> TransportFuture {
        let server_tls = self.server.clone();
        let rejected = self.rejected.clone();
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(16384);
            tokio::spawn(async move {
                if tokio_rustls::TlsAcceptor::from(server_tls)
                    .accept(server)
                    .await
                    .is_err()
                {
                    rejected.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
            let (read, write) = tokio::io::split(client);
            Ok(TransportConnection {
                io: Arc::new(MemoryIo {
                    read: Arc::new(tokio::sync::Mutex::new(read)),
                    write: Arc::new(tokio::sync::Mutex::new(write)),
                }),
                mode: TransportMode::Base,
                network_handling: NetworkHandling::NotApplicable,
            })
        })
    }
}

#[test]
fn custom_base_streams_do_not_bypass_broker_trust_validation() {
    let broker = tls::Fixture::new();
    let wrong_roots = tls::Fixture::new();
    for mqtt5 in [false, true] {
        for backend in [TlsBackend::Rustls, TlsBackend::Native] {
            if (backend == TlsBackend::Rustls && !cfg!(feature = "use-rustls-no-provider"))
                || (backend == TlsBackend::Native && !cfg!(feature = "use-native-tls"))
            {
                continue;
            }
            for websocket in [false, true] {
                if websocket && !cfg!(feature = "websocket") {
                    continue;
                }
                let rejected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let connector = Arc::new(RejectTlsConnector {
                    server: broker.server.clone(),
                    rejected: rejected.clone(),
                });
                let weak = Arc::downgrade(&connector);
                let mut config = config(mqtt5, 1883);
                config.common.broker = if websocket {
                    BrokerTarget::WebSocket {
                        url: "wss://localhost:1883/mqtt".into(),
                    }
                } else {
                    BrokerTarget::Tcp {
                        host: "localhost".into(),
                        port: 1883,
                    }
                };
                config.common.transport = if websocket {
                    TransportConfig::Wss(wrong_roots.client(backend))
                } else {
                    TransportConfig::Tls(wrong_roots.client(backend))
                };
                config.common.connector = Some(TransportConnectorConfig {
                    connector,
                    mode: TransportMode::Base,
                });
                let mut client = support::start(config).unwrap();
                let mut events = client.take_events().unwrap();
                let event = events
                    .recv_timeout(DEADLINE)
                    .unwrap()
                    .expect("missing trust rejection event");
                let WrapperEvent::Disconnected { error, .. } = event else {
                    panic!("untrusted broker was accepted")
                };
                assert!(
                    error.kind() == ErrorKind::Tls
                        || (websocket && error.kind() == ErrorKind::Network)
                );
                let deadline = Instant::now() + DEADLINE;
                while rejected.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "server did not observe the TLS rejection"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                client.closer().close_now(DEADLINE).unwrap();
                drop(client);
                assert!(weak.upgrade().is_none());
            }
        }
    }
}
