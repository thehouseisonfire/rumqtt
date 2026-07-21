use crate::default_socket_connect;
use crate::{AsyncReadWrite, NetworkOptions, SocketConnector};

use std::fmt;
use std::io;
use std::net::Ipv6Addr;

#[cfg(all(
    feature = "http-proxy",
    any(feature = "use-rustls-no-provider", feature = "use-native-tls")
))]
use crate::{TlsConfiguration, tls};

/// Protocol used to establish a proxy tunnel.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyProtocol {
    #[cfg(feature = "http-proxy")]
    Http,
    #[cfg(all(
        feature = "http-proxy",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    ))]
    Https,
    #[cfg(feature = "socks-proxy")]
    Socks5,
}

#[derive(Clone)]
enum ProxyKind {
    #[cfg(feature = "http-proxy")]
    Http,
    #[cfg(all(
        feature = "http-proxy",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    ))]
    Https(TlsConfiguration),
    #[cfg(feature = "socks-proxy")]
    Socks5,
}

#[derive(Clone)]
struct ProxyCredentials {
    username: String,
    password: String,
}

/// Configuration for an HTTP, HTTPS, or SOCKS5 proxy.
#[derive(Clone)]
pub struct Proxy {
    kind: ProxyKind,
    host: String,
    port: u16,
    credentials: Option<ProxyCredentials>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("Proxy");
        debug
            .field("protocol", &self.protocol())
            .field("host", &self.host)
            .field("port", &self.port);

        if let Some(credentials) = &self.credentials {
            debug
                .field("username", &credentials.username)
                .field("password", &"[REDACTED]");
        } else {
            debug.field("credentials", &Option::<()>::None);
        }

        debug.finish()
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProxyError {
    #[error("Socket connect: {0}.")]
    Io(#[from] io::Error),
    #[cfg(feature = "http-proxy")]
    #[error("HTTP proxy connect: {0}.")]
    Http(#[from] async_http_proxy::HttpError),
    #[cfg(feature = "socks-proxy")]
    #[error("SOCKS5 proxy connect: {0}.")]
    Socks5(#[from] tokio_socks::Error),

    #[cfg(all(
        feature = "http-proxy",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    ))]
    #[error("TLS connect: {0}.")]
    Tls(#[from] tls::Error),
}

impl Proxy {
    /// Creates an unauthenticated HTTP CONNECT proxy configuration.
    #[cfg(feature = "http-proxy")]
    pub fn http<S: Into<String>>(host: S, port: u16) -> Self {
        Self::new(ProxyKind::Http, host, port)
    }

    /// Creates an unauthenticated HTTPS CONNECT proxy configuration.
    #[cfg(all(
        feature = "http-proxy",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    ))]
    pub fn https<S: Into<String>>(host: S, port: u16, tls_config: TlsConfiguration) -> Self {
        Self::new(ProxyKind::Https(tls_config), host, port)
    }

    /// Creates an unauthenticated SOCKS5 proxy configuration.
    ///
    /// Broker hostnames are sent to the proxy for resolution. Supplying an IP
    /// address uses the corresponding SOCKS5 IPv4 or IPv6 address form.
    #[cfg(feature = "socks-proxy")]
    pub fn socks5<S: Into<String>>(host: S, port: u16) -> Self {
        Self::new(ProxyKind::Socks5, host, port)
    }

    fn new<S: Into<String>>(kind: ProxyKind, host: S, port: u16) -> Self {
        Self {
            kind,
            host: host.into(),
            port,
            credentials: None,
        }
    }

    /// Configures proxy username/password credentials.
    ///
    /// HTTP(S) proxies use HTTP Basic authentication. SOCKS5 proxies use the
    /// username/password method from RFC 1929.
    #[must_use]
    pub fn with_credentials<U, P>(mut self, username: U, password: P) -> Self
    where
        U: Into<String>,
        P: Into<String>,
    {
        self.credentials = Some(ProxyCredentials {
            username: username.into(),
            password: password.into(),
        });
        self
    }

    pub const fn protocol(&self) -> ProxyProtocol {
        match self.kind {
            #[cfg(feature = "http-proxy")]
            ProxyKind::Http => ProxyProtocol::Http,
            #[cfg(all(
                feature = "http-proxy",
                any(feature = "use-rustls-no-provider", feature = "use-native-tls")
            ))]
            ProxyKind::Https(_) => ProxyProtocol::Https,
            #[cfg(feature = "socks-proxy")]
            ProxyKind::Socks5 => ProxyProtocol::Socks5,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub const fn has_credentials(&self) -> bool {
        self.credentials.is_some()
    }

    /// Connects to a broker through this proxy.
    ///
    /// # Errors
    ///
    /// Returns [`ProxyError`] if the proxy socket, TLS handshake, or proxy
    /// protocol negotiation fails.
    pub async fn connect(
        self,
        broker_addr: &str,
        broker_port: u16,
        network_options: NetworkOptions,
        socket_connector: Option<SocketConnector>,
    ) -> Result<Box<dyn AsyncReadWrite>, ProxyError> {
        let proxy_addr = endpoint(&self.host, self.port);
        let tcp: Box<dyn AsyncReadWrite> = if let Some(connector) = socket_connector {
            connector(proxy_addr, network_options).await?
        } else {
            Box::new(default_socket_connect(proxy_addr, network_options).await?)
        };

        match self.kind {
            #[cfg(feature = "http-proxy")]
            ProxyKind::Http => http_connect(tcp, broker_addr, broker_port, self.credentials).await,
            #[cfg(all(
                feature = "http-proxy",
                any(feature = "use-rustls-no-provider", feature = "use-native-tls")
            ))]
            ProxyKind::Https(tls_config) => {
                let tcp = tls::tls_connect(&self.host, self.port, &tls_config, tcp).await?;
                http_connect(tcp, broker_addr, broker_port, self.credentials).await
            }
            #[cfg(feature = "socks-proxy")]
            ProxyKind::Socks5 => {
                let target = (broker_addr, broker_port);
                let stream = if let Some(credentials) = self.credentials {
                    tokio_socks::tcp::Socks5Stream::connect_with_password_and_socket(
                        tcp,
                        target,
                        &credentials.username,
                        &credentials.password,
                    )
                    .await?
                } else {
                    tokio_socks::tcp::Socks5Stream::connect_with_socket(tcp, target).await?
                };
                Ok(Box::new(stream))
            }
        }
    }
}

fn endpoint(host: &str, port: u16) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(feature = "http-proxy")]
async fn http_connect(
    mut stream: Box<dyn AsyncReadWrite>,
    host: &str,
    port: u16,
    credentials: Option<ProxyCredentials>,
) -> Result<Box<dyn AsyncReadWrite>, ProxyError> {
    if let Some(credentials) = credentials {
        async_http_proxy::http_connect_tokio_with_basic_auth(
            &mut stream,
            host,
            port,
            &credentials.username,
            &credentials.password,
        )
        .await?;
    } else {
        async_http_proxy::http_connect_tokio(&mut stream, host, port).await?;
    }
    Ok(stream)
}

#[cfg(all(test, feature = "socks-proxy"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn single_stream_connector(stream: DuplexStream) -> SocketConnector {
        let stream = Arc::new(Mutex::new(Some(stream)));
        Arc::new(move |_, _| {
            let stream = stream.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let stream: Box<dyn AsyncReadWrite> = Box::new(stream);
                Ok(stream)
            })
        })
    }

    async fn serve_socks5(
        mut stream: DuplexStream,
        expected_target: String,
        expected_port: u16,
        expected_credentials: Option<(String, String)>,
    ) {
        assert_eq!(stream.read_u8().await.unwrap(), 5);
        let method_count = stream.read_u8().await.unwrap() as usize;
        let mut methods = vec![0; method_count];
        stream.read_exact(&mut methods).await.unwrap();

        if let Some((username, password)) = expected_credentials {
            assert_eq!(methods, [0, 2]);
            stream.write_all(&[5, 2]).await.unwrap();

            assert_eq!(stream.read_u8().await.unwrap(), 1);
            let username_len = stream.read_u8().await.unwrap() as usize;
            let mut actual_username = vec![0; username_len];
            stream.read_exact(&mut actual_username).await.unwrap();
            let password_len = stream.read_u8().await.unwrap() as usize;
            let mut actual_password = vec![0; password_len];
            stream.read_exact(&mut actual_password).await.unwrap();
            assert_eq!(actual_username, username.as_bytes());
            assert_eq!(actual_password, password.as_bytes());
            stream.write_all(&[1, 0]).await.unwrap();
        } else {
            assert_eq!(methods, [0]);
            stream.write_all(&[5, 0]).await.unwrap();
        }

        assert_eq!(stream.read_u8().await.unwrap(), 5);
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        assert_eq!(stream.read_u8().await.unwrap(), 0);
        let address_type = stream.read_u8().await.unwrap();
        let actual_target = match address_type {
            1 => {
                let mut octets = [0; 4];
                stream.read_exact(&mut octets).await.unwrap();
                std::net::Ipv4Addr::from(octets).to_string()
            }
            3 => {
                let length = stream.read_u8().await.unwrap() as usize;
                let mut domain = vec![0; length];
                stream.read_exact(&mut domain).await.unwrap();
                String::from_utf8(domain).unwrap()
            }
            4 => {
                let mut octets = [0; 16];
                stream.read_exact(&mut octets).await.unwrap();
                Ipv6Addr::from(octets).to_string()
            }
            other => panic!("unexpected SOCKS5 address type {other}"),
        };
        assert_eq!(actual_target, expected_target);
        assert_eq!(stream.read_u16().await.unwrap(), expected_port);

        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();

        let mut ping = [0; 4];
        stream.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"ping");
        stream.write_all(b"pong").await.unwrap();
    }

    async fn exercise_socks5(
        target: &str,
        expected_target: &str,
        credentials: Option<(&str, &str)>,
    ) {
        let (client_stream, server_stream) = tokio::io::duplex(1024);
        let available_stream = Arc::new(Mutex::new(Some(client_stream)));
        let connector_stream = Arc::clone(&available_stream);
        let connected_endpoint = Arc::new(Mutex::new(None));
        let endpoint_capture = Arc::clone(&connected_endpoint);

        let connector: SocketConnector = Arc::new(move |endpoint, network_options| {
            *endpoint_capture.lock().unwrap() = Some(endpoint);
            assert_eq!(network_options.connection_timeout(), 17);
            let stream = connector_stream.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let stream: Box<dyn AsyncReadWrite> = Box::new(stream);
                Ok(stream)
            })
        });

        let server = tokio::spawn(serve_socks5(
            server_stream,
            expected_target.to_owned(),
            1883,
            credentials.map(|(username, password)| (username.to_owned(), password.to_owned())),
        ));

        let mut network_options = NetworkOptions::new();
        network_options.set_connection_timeout(17);
        let mut proxy = Proxy::socks5("::1", 1080);
        if let Some((username, password)) = credentials {
            proxy = proxy.with_credentials(username, password);
        }

        let mut stream = proxy
            .connect(target, 1883, network_options, Some(connector))
            .await
            .unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut pong = [0; 4];
        stream.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        server.await.unwrap();
        assert_eq!(
            connected_endpoint.lock().unwrap().as_deref(),
            Some("[::1]:1080")
        );
    }

    #[tokio::test]
    async fn socks5_uses_proxy_dns_and_password_authentication() {
        exercise_socks5(
            "broker.internal.example",
            "broker.internal.example",
            Some(("proxy-user", "proxy-password")),
        )
        .await;
    }

    #[tokio::test]
    async fn socks5_encodes_ipv4_and_ipv6_targets() {
        exercise_socks5("192.0.2.10", "192.0.2.10", None).await;
        exercise_socks5("2001:db8::10", "2001:db8::10", None).await;
    }

    #[tokio::test]
    async fn socks5_rejects_invalid_credentials() {
        let (client_stream, _server_stream) = tokio::io::duplex(64);
        let connector = single_stream_connector(client_stream);

        let result = Proxy::socks5("127.0.0.1", 1080)
            .with_credentials("", "password")
            .connect(
                "broker.example",
                1883,
                NetworkOptions::new(),
                Some(connector),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("invalid credentials unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ProxyError::Socks5(tokio_socks::Error::InvalidAuthValues(_))
        ));
    }

    #[tokio::test]
    async fn socks5_maps_authentication_and_connect_failures() {
        let (client_stream, mut server_stream) = tokio::io::duplex(128);
        let server = tokio::spawn(async move {
            let mut greeting = [0; 4];
            server_stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 2, 0, 2]);
            server_stream.write_all(&[5, 2]).await.unwrap();

            assert_eq!(server_stream.read_u8().await.unwrap(), 1);
            let username_len = server_stream.read_u8().await.unwrap() as usize;
            let mut username = vec![0; username_len];
            server_stream.read_exact(&mut username).await.unwrap();
            let password_len = server_stream.read_u8().await.unwrap() as usize;
            let mut password = vec![0; password_len];
            server_stream.read_exact(&mut password).await.unwrap();
            server_stream.write_all(&[1, 1]).await.unwrap();
        });
        let result = Proxy::socks5("127.0.0.1", 1080)
            .with_credentials("user", "wrong")
            .connect(
                "broker.example",
                1883,
                NetworkOptions::new(),
                Some(single_stream_connector(client_stream)),
            )
            .await;
        assert!(matches!(
            result,
            Err(ProxyError::Socks5(tokio_socks::Error::PasswordAuthFailure(
                1
            )))
        ));
        server.await.unwrap();

        let (client_stream, mut server_stream) = tokio::io::duplex(128);
        let server = tokio::spawn(async move {
            let mut greeting = [0; 3];
            server_stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            server_stream.write_all(&[5, 0]).await.unwrap();

            let mut request_prefix = [0; 5];
            server_stream.read_exact(&mut request_prefix).await.unwrap();
            assert_eq!(&request_prefix[..4], &[5, 1, 0, 3]);
            let remaining = request_prefix[4] as usize + 2;
            let mut target = vec![0; remaining];
            server_stream.read_exact(&mut target).await.unwrap();
            server_stream
                .write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let result = Proxy::socks5("127.0.0.1", 1080)
            .connect(
                "broker.example",
                1883,
                NetworkOptions::new(),
                Some(single_stream_connector(client_stream)),
            )
            .await;
        assert!(matches!(
            result,
            Err(ProxyError::Socks5(tokio_socks::Error::ConnectionRefused))
        ));
        server.await.unwrap();
    }

    #[test]
    fn proxy_accessors_and_debug_are_safe() {
        let proxy = Proxy::socks5("proxy.example", 1080).with_credentials("alice", "secret");
        assert_eq!(proxy.protocol(), ProxyProtocol::Socks5);
        assert_eq!(proxy.host(), "proxy.example");
        assert_eq!(proxy.port(), 1080);
        assert!(proxy.has_credentials());

        let debug = format!("{proxy:?}");
        assert!(debug.contains("alice"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
    }
}
