use crate::{Error, Result, TlsConfig};

/// Owned proxy authentication; debug output never includes either credential.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ProxyCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProxyCredentials([REDACTED])")
    }
}

/// Broker DNS is resolved remotely by the proxy. Use an IP broker target for
/// pre-resolved addresses. SOCKS4 and local-DNS callbacks are not supported by
/// the underlying clients.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyConfig {
    Http {
        host: String,
        port: u16,
        credentials: Option<ProxyCredentials>,
        /// TLS to the proxy, independent of TLS to the broker.
        tls: Option<TlsConfig>,
    },
    Socks5 {
        host: String,
        port: u16,
        credentials: Option<ProxyCredentials>,
    },
}

impl ProxyConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let (host, port, credentials) = match self {
            Self::Http {
                host,
                port,
                credentials,
                ..
            } => {
                if !cfg!(feature = "http-proxy") {
                    return Err(Error::configuration("HTTP proxy feature is disabled"));
                }
                if credentials
                    .as_ref()
                    .is_some_and(|c| c.username.contains(':'))
                {
                    return Err(Error::configuration(
                        "HTTP proxy username cannot contain a colon",
                    ));
                }
                (host, port, credentials)
            }
            Self::Socks5 {
                host,
                port,
                credentials,
            } => {
                if !cfg!(feature = "socks-proxy") {
                    return Err(Error::configuration("SOCKS proxy feature is disabled"));
                }
                if credentials.as_ref().is_some_and(|c| {
                    c.username.is_empty()
                        || c.username.len() > 255
                        || c.password.is_empty()
                        || c.password.len() > 255
                }) {
                    return Err(Error::configuration(
                        "SOCKS5 credentials must each contain 1 to 255 bytes",
                    ));
                }
                (host, port, credentials)
            }
        };
        if host.is_empty() || host.contains(['\0', '\r', '\n']) || *port == 0 {
            return Err(Error::configuration("invalid proxy endpoint"));
        }
        if credentials.as_ref().is_some_and(|c| {
            c.username.contains(['\0', '\r', '\n']) || c.password.contains(['\0', '\r', '\n'])
        }) {
            return Err(Error::configuration("invalid proxy credentials"));
        }
        Ok(())
    }
}
