#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(feature = "use-rustls-no-provider")]
use rustls_native_certs::load_native_certs;
use tokio::net::{TcpSocket, TcpStream, lookup_host};
#[cfg(feature = "use-native-tls")]
use tokio_native_tls::native_tls::TlsConnector;
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

#[cfg(feature = "proxy")]
mod proxy;
mod scheduler;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
mod tls;
#[cfg(feature = "websocket")]
mod websockets;

#[cfg(feature = "proxy")]
pub use proxy::{Proxy, ProxyAuth, ProxyError, ProxyType};
pub use scheduler::{OutboundScheduler, RequestClass, RequestReadiness, ScheduledRequest};
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use tls::Error as TlsError;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use tls::tls_connect;
#[cfg(all(
    feature = "websocket",
    feature = "use-native-tls",
    not(feature = "use-rustls-no-provider")
))]
pub use tls::websocket_tls_connector;
#[cfg(all(
    feature = "websocket",
    feature = "use-rustls-no-provider",
    not(feature = "use-native-tls")
))]
pub use tls::websocket_tls_connector;
#[cfg(feature = "websocket")]
pub use websockets::{
    UrlError, ValidationError, WsAdapter, split_url, split_url_with_default_port,
    validate_response_headers,
};

#[cfg(not(feature = "websocket"))]
pub trait AsyncReadWrite:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin
{
}
#[cfg(not(feature = "websocket"))]
impl<T> AsyncReadWrite for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Sync + Unpin
{
}

#[cfg(feature = "websocket")]
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
#[cfg(feature = "websocket")]
impl<T> AsyncReadWrite for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

pub type DynAsyncReadWrite = Box<dyn AsyncReadWrite>;

/// Custom socket connector used to establish the underlying stream before optional proxy/TLS layers.
pub type SocketConnector = Arc<
    dyn Fn(
            String,
            NetworkOptions,
        ) -> Pin<Box<dyn Future<Output = Result<DynAsyncReadWrite, io::Error>> + Send>>
        + Send
        + Sync,
>;

/// TLS configuration method
#[derive(Clone, Debug)]
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub enum TlsConfiguration {
    #[cfg(feature = "use-rustls-no-provider")]
    Simple {
        /// ca certificate
        ca: Vec<u8>,
        /// alpn settings
        alpn: Option<Vec<Vec<u8>>>,
        /// tls `client_authentication`
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
    },
    #[cfg(feature = "use-native-tls")]
    SimpleNative {
        /// ca certificate
        ca: Vec<u8>,
        /// pkcs12 binary der and
        /// password for use with der
        client_auth: Option<(Vec<u8>, String)>,
    },
    #[cfg(feature = "use-rustls-no-provider")]
    /// Injected rustls `ClientConfig` for TLS, to allow more customisation.
    Rustls(Arc<ClientConfig>),
    #[cfg(feature = "use-native-tls")]
    /// Use default native-tls configuration
    Native,
    #[cfg(feature = "use-native-tls")]
    /// Injected native-tls `TlsConnector` for TLS, to allow more customisation.
    NativeConnector(TlsConnector),
}

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
impl TlsConfiguration {
    #[cfg(feature = "use-rustls-no-provider")]
    #[must_use]
    /// Builds a rustls client configuration backed by the platform root store.
    ///
    /// # Panics
    ///
    /// Panics if loading native certificates fails or a certificate cannot be
    /// inserted into the root store.
    pub fn default_rustls() -> Self {
        Self::try_default_rustls().expect("could not build rustls platform cert configuration")
    }

    #[cfg(feature = "use-rustls-no-provider")]
    /// Tries to build a rustls client configuration backed by the platform root store.
    ///
    /// # Errors
    ///
    /// Returns [`TlsError`] if loading native certificates fails or a certificate
    /// cannot be inserted into the root store.
    pub fn try_default_rustls() -> Result<Self, TlsError> {
        let builder = tls::rustls_client_config_builder()?;
        let mut root_cert_store = RootCertStore::empty();
        let certs = load_native_certs();
        if !certs.errors.is_empty() {
            return Err(TlsError::NativeCerts(certs.errors));
        }

        for cert in certs.certs {
            root_cert_store.add(cert)?;
        }

        let tls_config = builder
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        Ok(Self::Rustls(Arc::new(tls_config)))
    }

    #[cfg(feature = "use-native-tls")]
    #[must_use]
    /// Builds a native-tls configuration from PEM CA bytes and optional
    /// PKCS#12 client identity data.
    pub const fn simple_native(ca: Vec<u8>, client_auth: Option<(Vec<u8>, String)>) -> Self {
        Self::SimpleNative { ca, client_auth }
    }

    #[cfg(feature = "use-native-tls")]
    #[must_use]
    pub const fn default_native() -> Self {
        Self::Native
    }
}

#[cfg(all(feature = "use-rustls-no-provider", not(feature = "use-native-tls")))]
impl Default for TlsConfiguration {
    fn default() -> Self {
        Self::default_rustls()
    }
}

#[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
impl Default for TlsConfiguration {
    fn default() -> Self {
        Self::default_native()
    }
}

#[cfg(feature = "use-rustls-no-provider")]
impl From<ClientConfig> for TlsConfiguration {
    fn from(config: ClientConfig) -> Self {
        Self::Rustls(Arc::new(config))
    }
}

#[cfg(feature = "use-native-tls")]
impl From<TlsConnector> for TlsConfiguration {
    fn from(connector: TlsConnector) -> Self {
        Self::NativeConnector(connector)
    }
}

/// Provides a way to configure low level network connection configurations
#[derive(Clone, Debug, Default)]
pub struct NetworkOptions {
    tcp_send_buffer_size: Option<u32>,
    tcp_recv_buffer_size: Option<u32>,
    tcp_nodelay: bool,
    conn_timeout: u64,
    bind_addr: Option<SocketAddr>,
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    bind_device: Option<String>,
    #[cfg(target_os = "linux")]
    mptcp: bool,
}

impl NetworkOptions {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tcp_send_buffer_size: None,
            tcp_recv_buffer_size: None,
            tcp_nodelay: false,
            conn_timeout: 5,
            bind_addr: None,
            #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
            bind_device: None,
            #[cfg(target_os = "linux")]
            mptcp: false,
        }
    }

    pub const fn set_tcp_nodelay(&mut self, nodelay: bool) {
        self.tcp_nodelay = nodelay;
    }

    pub const fn set_tcp_send_buffer_size(&mut self, size: u32) {
        self.tcp_send_buffer_size = Some(size);
    }

    pub const fn set_tcp_recv_buffer_size(&mut self, size: u32) {
        self.tcp_recv_buffer_size = Some(size);
    }

    /// set connection timeout in secs
    pub const fn set_connection_timeout(&mut self, timeout: u64) -> &mut Self {
        self.conn_timeout = timeout;
        self
    }

    /// get timeout in secs
    #[must_use]
    pub const fn connection_timeout(&self) -> u64 {
        self.conn_timeout
    }

    /// Bind a connection to a specific local socket address.
    ///
    /// When the address uses a fixed nonzero port, the default multi-address
    /// dialer avoids overlapping attempts to prevent `AddrInUse`, which means
    /// same-family fallback attempts are no longer staggered in parallel.
    ///
    /// In that mode, an earlier candidate keeps the fixed port until it
    /// completes or the overall connect timeout expires. This preserves source
    /// port stability, but gives up happy-eyeballs-style fallback across
    /// same-family addresses.
    pub const fn set_bind_addr(&mut self, bind_addr: SocketAddr) -> &mut Self {
        self.bind_addr = Some(bind_addr);
        self
    }

    #[must_use]
    pub const fn bind_addr(&self) -> Option<SocketAddr> {
        self.bind_addr
    }

    /// bind connection to a specific network device by name
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))
    )]
    pub fn set_bind_device(&mut self, bind_device: &str) -> &mut Self {
        self.bind_device = Some(bind_device.to_string());
        self
    }

    /// Select Multipath TCP (MPTCP, RFC 8684) for outgoing connections.
    ///
    /// The default socket connector creates sockets with `IPPROTO_MPTCP`. If the local kernel
    /// reports that MPTCP is unavailable or disabled, it falls back to a regular TCP socket.
    /// Once an MPTCP socket has been created, connection errors are returned normally; peer and
    /// middlebox compatibility is handled by the kernel's MPTCP negotiation.
    ///
    /// Custom socket connectors receive this option but decide whether and how to honor it.
    #[cfg(target_os = "linux")]
    #[cfg_attr(docsrs, doc(cfg(target_os = "linux")))]
    pub const fn set_mptcp(&mut self, mptcp: bool) -> &mut Self {
        self.mptcp = mptcp;
        self
    }

    /// Returns whether the default socket connector should use Multipath TCP.
    #[must_use]
    #[cfg(target_os = "linux")]
    #[cfg_attr(docsrs, doc(cfg(target_os = "linux")))]
    pub const fn mptcp(&self) -> bool {
        self.mptcp
    }
}

fn new_tcp_socket(addr: SocketAddr, network_options: &NetworkOptions) -> io::Result<TcpSocket> {
    #[cfg(target_os = "linux")]
    if network_options.mptcp {
        match create_mptcp_socket(addr) {
            Ok(socket) => {
                socket.set_nonblocking(true)?;
                return Ok(TcpSocket::from_std_stream(socket.into()));
            }
            Err(error) if is_mptcp_unavailable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = network_options;

    match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    }
}

#[cfg(target_os = "linux")]
fn create_mptcp_socket(addr: SocketAddr) -> io::Result<socket2::Socket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = match addr {
        SocketAddr::V4(_) => Domain::IPV4,
        SocketAddr::V6(_) => Domain::IPV6,
    };
    Socket::new(domain, Type::STREAM, Some(Protocol::MPTCP))
}

#[cfg(target_os = "linux")]
fn is_mptcp_unavailable(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|error_code| {
        matches!(
            error_code,
            libc::EINVAL | libc::EPROTONOSUPPORT | libc::ENOPROTOOPT
        )
    })
}

fn configure_tcp_socket(socket: &TcpSocket, network_options: &NetworkOptions) -> io::Result<()> {
    socket.set_nodelay(network_options.tcp_nodelay)?;

    if let Some(send_buff_size) = network_options.tcp_send_buffer_size {
        socket.set_send_buffer_size(send_buff_size)?;
    }
    if let Some(recv_buffer_size) = network_options.tcp_recv_buffer_size {
        socket.set_recv_buffer_size(recv_buffer_size)?;
    }

    if let Some(bind_addr) = network_options.bind_addr {
        socket.bind(bind_addr)?;
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    {
        if let Some(bind_device) = &network_options.bind_device {
            socket.bind_device(Some(bind_device.as_bytes()))?;
        }
    }

    Ok(())
}

/// Connects a single resolved socket address using the provided [`NetworkOptions`].
///
/// This is the per-address building block used by the default sequential dialer and by callers
/// that want to apply a custom scheduling policy across multiple resolved addresses.
///
/// # Errors
///
/// Returns any socket construction, socket configuration, or connect error encountered.
pub async fn connect_socket_addr(
    addr: SocketAddr,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let socket = new_tcp_socket(addr, &network_options)?;

    configure_tcp_socket(&socket, &network_options)?;
    socket.connect(addr).await
}

/// Default TCP socket connection logic used by the MQTT event loop.
///
/// This resolves the host, applies [`NetworkOptions`] on each candidate socket,
/// and returns the first successful connection.
///
/// # Errors
///
/// Returns any DNS lookup, socket configuration, or connect error encountered.
/// When multiple address candidates are available, the last connect error is
/// returned if they all fail.
pub async fn default_socket_connect(
    host: String,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let addrs = lookup_host(host).await?;
    let mut last_err = None;

    for addr in addrs {
        match connect_socket_addr(addr, network_options.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_err = Some(err);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    use super::TlsConfiguration;
    use super::{NetworkOptions, connect_socket_addr, default_socket_connect};
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use tokio::net::TcpListener;

    #[cfg(all(
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-aws-lc", feature = "use-rustls-ring")
    ))]
    #[test]
    fn default_rustls_returns_rustls_variant() {
        assert!(matches!(
            TlsConfiguration::default_rustls(),
            TlsConfiguration::Rustls(_)
        ));
    }

    #[cfg(all(
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-aws-lc", feature = "use-rustls-ring")
    ))]
    #[test]
    fn try_default_rustls_returns_rustls_variant() {
        assert!(matches!(
            TlsConfiguration::try_default_rustls(),
            Ok(TlsConfiguration::Rustls(_))
        ));
    }

    #[cfg(all(
        feature = "use-rustls-no-provider",
        not(feature = "use-rustls-aws-lc"),
        not(feature = "use-rustls-ring")
    ))]
    #[test]
    fn try_default_rustls_errors_without_crypto_provider() {
        assert!(matches!(
            TlsConfiguration::try_default_rustls(),
            Err(super::TlsError::CryptoProviderUnavailable)
        ));
    }

    #[cfg(feature = "use-native-tls")]
    #[test]
    fn default_native_returns_native_variant() {
        assert!(matches!(
            TlsConfiguration::default_native(),
            TlsConfiguration::Native
        ));
    }

    #[cfg(feature = "use-native-tls")]
    #[test]
    fn simple_native_returns_simple_native_variant() {
        let config = TlsConfiguration::simple_native(
            Vec::from("Test CA"),
            Some((vec![1, 2, 3], String::from("secret"))),
        );

        assert!(matches!(
            config,
            TlsConfiguration::SimpleNative {
                ca,
                client_auth: Some((identity, password))
            } if ca == b"Test CA" && identity == vec![1, 2, 3] && password == "secret"
        ));
    }

    #[tokio::test]
    async fn connect_socket_addr_succeeds_with_ipv4_bind_addr() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let listener_addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (stream, peer_addr) = listener.accept().await.unwrap();
            drop(stream);
            peer_addr
        });

        let mut network_options = NetworkOptions::new();
        network_options.set_bind_addr(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)));

        let stream = connect_socket_addr(listener_addr, network_options)
            .await
            .unwrap();
        let local_addr = stream.local_addr().unwrap();
        assert!(local_addr.ip().is_loopback());
        drop(stream);

        let peer_addr = accept.await.unwrap();
        assert_eq!(peer_addr.ip(), local_addr.ip());
    }

    #[tokio::test]
    async fn connect_socket_addr_returns_error_for_mismatched_bind_addr_family() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let listener_addr = listener.local_addr().unwrap();

        let mut network_options = NetworkOptions::new();
        network_options.set_bind_addr(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            0,
            0,
            0,
        )));

        let err = connect_socket_addr(listener_addr, network_options)
            .await
            .unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[tokio::test]
    async fn connect_socket_addr_succeeds_with_ipv6_bind_addr() {
        let listener = match TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await {
            Ok(listener) => listener,
            Err(_) => return,
        };
        let listener_addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (stream, peer_addr) = listener.accept().await.unwrap();
            drop(stream);
            peer_addr
        });

        let mut network_options = NetworkOptions::new();
        network_options.set_bind_addr(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            0,
            0,
            0,
        )));

        let stream = connect_socket_addr(listener_addr, network_options)
            .await
            .unwrap();
        let local_addr = stream.local_addr().unwrap();
        assert_eq!(local_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        drop(stream);

        let peer_addr = accept.await.unwrap();
        assert_eq!(peer_addr.ip(), local_addr.ip());
    }

    #[tokio::test]
    async fn default_socket_connect_still_connects_without_bind_addr() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });

        let stream = default_socket_connect(addr.to_string(), NetworkOptions::new())
            .await
            .unwrap();
        assert!(stream.local_addr().unwrap().ip().is_loopback());
        drop(stream);
        accept.await.unwrap();
    }

    #[test]
    fn bind_addr_returns_configured_socket_addr() {
        let bind_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1883));
        let mut network_options = NetworkOptions::new();
        network_options.set_bind_addr(bind_addr);

        assert_eq!(network_options.bind_addr(), Some(bind_addr));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mptcp_defaults_to_disabled_and_can_be_enabled() {
        let mut network_options = NetworkOptions::new();
        assert!(!network_options.mptcp());

        network_options.set_mptcp(true);
        assert!(network_options.mptcp());

        network_options.set_mptcp(false);
        assert!(!network_options.mptcp());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mptcp_fallback_is_limited_to_unavailable_protocol_errors() {
        for error_code in [libc::EINVAL, libc::EPROTONOSUPPORT, libc::ENOPROTOOPT] {
            let error = io::Error::from_raw_os_error(error_code);
            assert!(super::is_mptcp_unavailable(&error));
        }

        let unrelated_error = io::Error::from_raw_os_error(libc::EMFILE);
        assert!(!super::is_mptcp_unavailable(&unrelated_error));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn mptcp_connects_to_plain_ipv4_listener_and_preserves_socket_options() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let listener_addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (stream, peer_addr) = listener.accept().await.unwrap();
            drop(stream);
            peer_addr
        });

        let mut network_options = NetworkOptions::new();
        network_options.set_mptcp(true);
        network_options.set_tcp_nodelay(true);
        network_options.set_bind_addr(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)));

        let stream = connect_socket_addr(listener_addr, network_options)
            .await
            .unwrap();
        let local_addr = stream.local_addr().unwrap();
        assert!(stream.nodelay().unwrap());
        assert_eq!(local_addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        drop(stream);

        assert_eq!(accept.await.unwrap().ip(), local_addr.ip());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn mptcp_connects_to_plain_ipv6_listener_when_available() {
        let Ok(listener) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await else {
            return;
        };
        let listener_addr = listener.local_addr().unwrap();

        let accept = tokio::spawn(async move {
            let (stream, peer_addr) = listener.accept().await.unwrap();
            drop(stream);
            peer_addr
        });

        let mut network_options = NetworkOptions::new();
        network_options.set_mptcp(true);
        network_options.set_bind_addr(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::LOCALHOST,
            0,
            0,
            0,
        )));

        let stream = connect_socket_addr(listener_addr, network_options)
            .await
            .unwrap();
        let local_addr = stream.local_addr().unwrap();
        assert_eq!(local_addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        drop(stream);

        assert_eq!(accept.await.unwrap().ip(), local_addr.ip());
    }
}
