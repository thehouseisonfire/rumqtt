#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::TlsConnector as RustlsConnector;
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::rustls::{
    self, ClientConfig, RootCertStore,
    pki_types::{CertificateDer, InvalidDnsNameError, PrivateKeyDer, ServerName, pem::PemObject},
};

#[cfg(feature = "use-rustls-no-provider")]
use std::convert::TryFrom;
#[cfg(feature = "use-rustls-no-provider")]
use std::sync::Arc;

use crate::TlsConfiguration;
use crate::framed::AsyncReadWrite;

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
    #[cfg(all(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[error(
        "rustls TLS configuration is not supported for websockets when both `use-rustls-no-provider` and `use-native-tls` features are enabled. async-tungstenite uses native-tls for WSS in this configuration. Use a native-tls TlsConfiguration variant or disable `use-native-tls`."
    )]
    TlsBackendConflict,
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

            let config = ClientConfig::builder().with_root_certificates(root_cert_store);

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
        TlsConfiguration::Rustls(tls_client_config) => tls_client_config.clone(),
        #[allow(unreachable_patterns)]
        _ => unreachable!("This cannot be called for other TLS backends than Rustls"),
    };

    Ok(RustlsConnector::from(config))
}

#[cfg(feature = "use-native-tls")]
pub async fn native_tls_connector(
    tls_config: &TlsConfiguration,
) -> Result<NativeTlsConnector, Error> {
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
/// TLS configuration. When both rustls and native-tls features are enabled,
/// rustls is preferred.
#[cfg(all(
    feature = "websocket",
    feature = "use-native-tls",
    not(feature = "use-rustls-no-provider")
))]
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
        _ => panic!("Unknown or not enabled TLS backend configuration"),
    }
}

/// Returns the appropriate TLS connector for websocket connections based on the
/// TLS configuration. When both rustls and native-tls features are enabled,
/// rustls is preferred.
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

/// Returns the appropriate TLS connector for websocket connections based on the
/// TLS configuration. When both rustls and native-tls features are enabled,
/// async-tungstenite uses native-tls as the connector type.
/// NOTE: When both features are enabled, only native-tls TlsConfiguration variants
/// are supported for websockets. Using rustls variants will return TlsBackendConflict error.
#[cfg(all(
    feature = "websocket",
    feature = "use-rustls-no-provider",
    feature = "use-native-tls"
))]
pub fn websocket_tls_connector(
    tls_config: &TlsConfiguration,
) -> Result<tokio_native_tls::TlsConnector, Error> {
    match tls_config {
        TlsConfiguration::Simple { .. } | TlsConfiguration::Rustls(_) => {
            Err(Error::TlsBackendConflict)
        }
        TlsConfiguration::Native
        | TlsConfiguration::NativeConnector(_)
        | TlsConfiguration::SimpleNative { .. } => {
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
                _ => unreachable!(),
            };
            Ok(connector.into())
        }
        #[allow(unreachable_patterns)]
        _ => panic!("Unknown or not enabled TLS backend configuration"),
    }
}

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
            let connector = native_tls_connector(tls_config).await?;
            Box::new(connector.connect(addr, tcp).await?)
        }
        #[allow(unreachable_patterns)]
        _ => panic!("Unknown or not enabled TLS backend configuration"),
    };
    Ok(tls)
}

#[cfg(test)]
mod tests {
    #[cfg(all(
        feature = "use-rustls-no-provider",
        feature = "use-native-tls",
        feature = "websocket"
    ))]
    #[test]
    fn websocket_rustls_config_returns_error_when_both_features_enabled() {
        // When both TLS features are enabled, using rustls config for websockets
        // should return TlsBackendConflict error
        let result = websocket_tls_connector(&TlsConfiguration::Native);
        assert!(result.is_ok(), "Native config should work");

        // SimpleNative should also work (will fail due to empty CA, but not with TlsBackendConflict)
        let simple_native = TlsConfiguration::SimpleNative {
            ca: vec![],
            client_auth: None,
        };
        let result = websocket_tls_connector(&simple_native);
        assert!(result.is_err());
        assert!(!matches!(result, Err(Error::TlsBackendConflict)));

        // TlsConfiguration::Simple uses rustls and should return TlsBackendConflict
        let simple_rustls = TlsConfiguration::Simple {
            ca: vec![],
            alpn: None,
            client_auth: None,
        };
        let result = websocket_tls_connector(&simple_rustls);
        assert!(
            matches!(result, Err(Error::TlsBackendConflict)),
            "TlsConfiguration::Simple should return TlsBackendConflict"
        );
    }
}
