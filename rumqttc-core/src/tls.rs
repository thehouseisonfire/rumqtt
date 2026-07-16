#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::TlsConnector as RustlsConnector;
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::rustls::{
    self, ClientConfig, ConfigBuilder, RootCertStore, WantsVerifier,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, InvalidDnsNameError, PrivateKeyDer, ServerName, pem::PemObject},
};

#[cfg(feature = "use-rustls-no-provider")]
use std::convert::TryFrom;
#[cfg(feature = "use-rustls-no-provider")]
use std::sync::Arc;

use crate::{AsyncReadWrite, TlsConfiguration};

#[cfg(feature = "use-native-tls")]
use tokio_native_tls::TlsConnector as NativeTlsConnector;

#[cfg(feature = "use-native-tls")]
use tokio_native_tls::native_tls::{Error as NativeTlsError, Identity};

use std::io;
use std::net::AddrParseError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error parsing IP address
    #[error("Addr")]
    Addr(#[from] AddrParseError),
    /// I/O related error
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[cfg(feature = "use-rustls-no-provider")]
    /// Certificate/Name validation error
    #[error("Web Pki: {0}")]
    WebPki(#[from] webpki::Error),
    /// Invalid DNS name
    #[cfg(feature = "use-rustls-no-provider")]
    #[error("DNS name")]
    DNSName(#[from] InvalidDnsNameError),
    #[cfg(feature = "use-rustls-no-provider")]
    /// Error from rustls module
    #[error("TLS error: {0}")]
    TLS(#[from] rustls::Error),
    #[cfg(feature = "use-rustls-no-provider")]
    /// Errors returned while loading platform-native certificates.
    #[error("Native certificate loading failed: {0:?}")]
    NativeCerts(Vec<rustls_native_certs::Error>),
    #[cfg(feature = "use-rustls-no-provider")]
    /// No rustls crypto provider is available for implicit configuration.
    #[error(
        "No rustls CryptoProvider is available; install a process default provider or enable exactly one rustls provider feature"
    )]
    CryptoProviderUnavailable,
    #[cfg(feature = "use-rustls-no-provider")]
    #[error("PEM parsing error: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),
    #[cfg(feature = "use-rustls-no-provider")]
    /// No valid CA cert found
    #[error("No valid CA certificate provided")]
    NoValidCertInChain,
    #[cfg(feature = "use-rustls-no-provider")]
    /// No valid client cert found
    #[error("No valid certificate for client authentication in chain")]
    NoValidClientCertInChain,
    #[cfg(feature = "use-rustls-no-provider")]
    /// No valid key found
    #[error("No valid key in chain")]
    NoValidKeyInChain,
    #[cfg(feature = "use-native-tls")]
    #[error("Native TLS error {0}")]
    NativeTls(#[from] NativeTlsError),
    /// Unsupported or not-enabled TLS backend for the given configuration
    #[error("TLS configuration is unsupported by the enabled TLS backends")]
    UnsupportedBackendConfiguration,
}

#[cfg(feature = "use-rustls-no-provider")]
type RustlsClientConfigBuilder = ConfigBuilder<ClientConfig, WantsVerifier>;

#[cfg(feature = "use-rustls-no-provider")]
fn rustls_crypto_provider() -> Result<Arc<CryptoProvider>, Error> {
    if let Some(provider) = CryptoProvider::get_default() {
        return Ok(Arc::clone(provider));
    }

    let provider: Option<CryptoProvider> = {
        #[cfg(all(feature = "use-rustls-ring", not(feature = "use-rustls-aws-lc")))]
        {
            Some(rustls::crypto::ring::default_provider())
        }

        #[cfg(all(feature = "use-rustls-aws-lc", not(feature = "use-rustls-ring")))]
        {
            Some(rustls::crypto::aws_lc_rs::default_provider())
        }

        #[cfg(not(any(
            all(feature = "use-rustls-ring", not(feature = "use-rustls-aws-lc")),
            all(feature = "use-rustls-aws-lc", not(feature = "use-rustls-ring"))
        )))]
        {
            None
        }
    };

    provider
        .map(Arc::new)
        .ok_or(Error::CryptoProviderUnavailable)
}

#[cfg(feature = "use-rustls-no-provider")]
pub fn rustls_client_config_builder() -> Result<RustlsClientConfigBuilder, Error> {
    Ok(
        ClientConfig::builder_with_provider(rustls_crypto_provider()?)
            .with_safe_default_protocol_versions()?,
    )
}

#[cfg(feature = "use-rustls-no-provider")]
pub fn rustls_connector(tls_config: &TlsConfiguration) -> Result<RustlsConnector, Error> {
    let config = match tls_config {
        TlsConfiguration::Simple {
            ca,
            alpn,
            client_auth,
        } => {
            // Add ca to root store if the connection is TLS
            let mut root_cert_store = RootCertStore::empty();
            let certs = CertificateDer::pem_slice_iter(ca).collect::<Result<Vec<_>, _>>()?;

            root_cert_store.add_parsable_certificates(certs);

            if root_cert_store.is_empty() {
                return Err(Error::NoValidCertInChain);
            }

            let config = rustls_client_config_builder()?.with_root_certificates(root_cert_store);

            // Add der encoded client cert and key
            let mut config = if let Some(client) = client_auth.as_ref() {
                let certs =
                    CertificateDer::pem_slice_iter(&client.0).collect::<Result<Vec<_>, _>>()?;
                if certs.is_empty() {
                    return Err(Error::NoValidClientCertInChain);
                }

                let key = match PrivateKeyDer::from_pem_slice(&client.1) {
                    Ok(key) => key,
                    Err(rustls::pki_types::pem::Error::NoItemsFound) => {
                        return Err(Error::NoValidKeyInChain);
                    }
                    Err(err) => return Err(Error::Pem(err)),
                };

                config.with_client_auth_cert(certs, key)?
            } else {
                config.with_no_client_auth()
            };

            // Set ALPN
            if let Some(alpn) = alpn.as_ref() {
                config.alpn_protocols.extend_from_slice(alpn);
            }

            Arc::new(config)
        }
        TlsConfiguration::Rustls(tls_client_config) => Arc::clone(tls_client_config),
        #[allow(unreachable_patterns)]
        _ => unreachable!("This cannot be called for other TLS backends than Rustls"),
    };

    Ok(RustlsConnector::from(config))
}

#[cfg(feature = "use-native-tls")]
pub fn native_tls_connector(tls_config: &TlsConfiguration) -> Result<NativeTlsConnector, Error> {
    let connector = match tls_config {
        TlsConfiguration::SimpleNative { ca, client_auth } => {
            let cert = native_tls::Certificate::from_pem(ca)?;

            let mut connector_builder = native_tls::TlsConnector::builder();
            connector_builder.add_root_certificate(cert);

            if let Some((der, password)) = client_auth {
                let identity = Identity::from_pkcs12(der, password)?;
                connector_builder.identity(identity);
            }

            connector_builder.build()?
        }
        TlsConfiguration::Native => native_tls::TlsConnector::new()?,
        TlsConfiguration::NativeConnector(connector) => connector.to_owned(),
        #[allow(unreachable_patterns)]
        _ => unreachable!("This cannot be called for other TLS backends than Native TLS"),
    };

    Ok(connector.into())
}

/// Returns the appropriate TLS connector for websocket connections based on the
/// TLS configuration.
#[cfg(all(
    feature = "websocket",
    feature = "use-native-tls",
    not(feature = "use-rustls-no-provider")
))]
/// Builds the native-TLS connector used by secure WebSocket transports.
///
/// # Errors
///
/// Returns [`Error::NativeTls`] if certificates, identities, or the platform
/// connector cannot be built, and [`Error::UnsupportedBackendConfiguration`]
/// for a non-native TLS configuration.
pub fn websocket_tls_connector(
    tls_config: &TlsConfiguration,
) -> Result<tokio_native_tls::TlsConnector, Error> {
    match tls_config {
        TlsConfiguration::Native
        | TlsConfiguration::NativeConnector(_)
        | TlsConfiguration::SimpleNative { .. } => {
            // For native-tls, we need to use the sync connector for websockets
            let connector = match tls_config {
                TlsConfiguration::SimpleNative { ca, client_auth } => {
                    let cert = native_tls::Certificate::from_pem(ca)?;
                    let mut connector_builder = native_tls::TlsConnector::builder();
                    connector_builder.add_root_certificate(cert);

                    if let Some((der, password)) = client_auth {
                        let identity = Identity::from_pkcs12(der, password)?;
                        connector_builder.identity(identity);
                    }

                    connector_builder.build()?
                }
                TlsConfiguration::Native => native_tls::TlsConnector::new()?,
                TlsConfiguration::NativeConnector(connector) => connector.to_owned(),
                // No need for catch-all: we're inside a match arm that only matches native-tls variants
            };
            Ok(connector.into())
        }
        #[allow(unreachable_patterns)]
        _ => Err(Error::UnsupportedBackendConfiguration),
    }
}

/// Returns the appropriate TLS connector for websocket connections based on the
/// TLS configuration.
#[cfg(all(
    feature = "websocket",
    feature = "use-rustls-no-provider",
    not(feature = "use-native-tls")
))]
pub fn websocket_tls_connector(
    tls_config: &TlsConfiguration,
) -> Result<tokio_rustls::TlsConnector, Error> {
    match tls_config {
        TlsConfiguration::Simple { .. } | TlsConfiguration::Rustls(_) => {
            let connector = rustls_connector(tls_config)?;
            Ok(connector)
        }
        #[allow(unreachable_patterns)]
        _ => panic!("Unknown or not enabled TLS backend configuration"),
    }
}

/// Establishes a TLS stream on top of an already connected transport.
///
/// # Errors
///
/// Returns any TLS configuration, server-name validation, or handshake error
/// produced by the selected backend.
///
pub async fn tls_connect(
    addr: &str,
    _port: u16,
    tls_config: &TlsConfiguration,
    tcp: Box<dyn AsyncReadWrite>,
) -> Result<Box<dyn AsyncReadWrite>, Error> {
    let tls: Box<dyn AsyncReadWrite> = match tls_config {
        #[cfg(feature = "use-rustls-no-provider")]
        TlsConfiguration::Simple { .. } | TlsConfiguration::Rustls(_) => {
            let connector = rustls_connector(tls_config)?;
            let domain = ServerName::try_from(addr)?.to_owned();
            Box::new(connector.connect(domain, tcp).await?)
        }
        #[cfg(feature = "use-native-tls")]
        TlsConfiguration::Native
        | TlsConfiguration::NativeConnector(_)
        | TlsConfiguration::SimpleNative { .. } => {
            let connector = native_tls_connector(tls_config)?;
            Box::new(connector.connect(addr, tcp).await?)
        }
        #[allow(unreachable_patterns)]
        _ => return Err(Error::UnsupportedBackendConfiguration),
    };
    Ok(tls)
}
