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
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
mod tls;
#[cfg(feature = "websocket")]
mod websockets;

#[cfg(feature = "proxy")]
pub use proxy::{Proxy, ProxyAuth, ProxyError, ProxyType};
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
pub use websockets::{UrlError, ValidationError, WsAdapter, split_url, validate_response_headers};

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
    /// Injected native-tls TlsConnector for TLS, to allow more customisation.
    NativeConnector(TlsConnector),
}

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
impl TlsConfiguration {
    #[cfg(feature = "use-rustls-no-provider")]
    #[must_use]
    pub fn default_rustls() -> Self {
        let mut root_cert_store = RootCertStore::empty();
        for cert in load_native_certs().expect("could not load platform certs") {
            root_cert_store.add(cert).unwrap();
        }

        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();

        Self::Rustls(Arc::new(tls_config))
    }

    #[cfg(feature = "use-native-tls")]
    #[must_use]
    pub fn default_native() -> Self {
        Self::Native
    }
}

#[cfg(all(feature = "use-rustls-no-provider", not(feature = "use-native-tls")))]
#[allow(clippy::derivable_impls)]
impl Default for TlsConfiguration {
    fn default() -> Self {
        Self::default_rustls()
    }
}

#[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
#[allow(clippy::derivable_impls)]
impl Default for TlsConfiguration {
    fn default() -> Self {
        Self::default_native()
    }
}

#[cfg(feature = "use-rustls-no-provider")]
impl From<ClientConfig> for TlsConfiguration {
    fn from(config: ClientConfig) -> Self {
        TlsConfiguration::Rustls(Arc::new(config))
    }
}

#[cfg(feature = "use-native-tls")]
impl From<TlsConnector> for TlsConfiguration {
    fn from(connector: TlsConnector) -> Self {
        TlsConfiguration::NativeConnector(connector)
    }
}

/// Provides a way to configure low level network connection configurations
#[derive(Clone, Debug, Default)]
pub struct NetworkOptions {
    tcp_send_buffer_size: Option<u32>,
    tcp_recv_buffer_size: Option<u32>,
    tcp_nodelay: bool,
    conn_timeout: u64,
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    bind_device: Option<String>,
}

impl NetworkOptions {
    #[must_use]
    pub fn new() -> Self {
        NetworkOptions {
            tcp_send_buffer_size: None,
            tcp_recv_buffer_size: None,
            tcp_nodelay: false,
            conn_timeout: 5,
            #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
            bind_device: None,
        }
    }

    pub fn set_tcp_nodelay(&mut self, nodelay: bool) {
        self.tcp_nodelay = nodelay;
    }

    pub fn set_tcp_send_buffer_size(&mut self, size: u32) {
        self.tcp_send_buffer_size = Some(size);
    }

    pub fn set_tcp_recv_buffer_size(&mut self, size: u32) {
        self.tcp_recv_buffer_size = Some(size);
    }

    /// set connection timeout in secs
    pub fn set_connection_timeout(&mut self, timeout: u64) -> &mut Self {
        self.conn_timeout = timeout;
        self
    }

    /// get timeout in secs
    #[must_use]
    pub fn connection_timeout(&self) -> u64 {
        self.conn_timeout
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
}

/// Default TCP socket connection logic used by the MQTT event loop.
///
/// This resolves the host, applies [`NetworkOptions`] on each candidate socket,
/// and returns the first successful connection.
pub async fn default_socket_connect(
    host: String,
    network_options: NetworkOptions,
) -> io::Result<TcpStream> {
    let addrs = lookup_host(host).await?;
    let mut last_err = None;

    for addr in addrs {
        let socket = match addr {
            SocketAddr::V4(_) => TcpSocket::new_v4()?,
            SocketAddr::V6(_) => TcpSocket::new_v6()?,
        };

        socket.set_nodelay(network_options.tcp_nodelay)?;

        if let Some(send_buff_size) = network_options.tcp_send_buffer_size {
            socket.set_send_buffer_size(send_buff_size)?;
        }
        if let Some(recv_buffer_size) = network_options.tcp_recv_buffer_size {
            socket.set_recv_buffer_size(recv_buffer_size)?;
        }

        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        {
            if let Some(bind_device) = &network_options.bind_device {
                socket.bind_device(Some(bind_device.as_bytes()))?;
            }
        }

        match socket.connect(addr).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                last_err = Some(e);
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
    use super::TlsConfiguration;

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

    #[cfg(feature = "use-native-tls")]
    #[test]
    fn default_native_returns_native_variant() {
        assert!(matches!(
            TlsConfiguration::default_native(),
            TlsConfiguration::Native
        ));
    }
}
