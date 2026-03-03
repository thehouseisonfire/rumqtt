#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
use rumqttc_core::TlsConfiguration;
#[cfg(feature = "use-rustls-no-provider")]
use rustls_native_certs::load_native_certs;
#[cfg(feature = "use-rustls-no-provider")]
use std::sync::Arc;
#[cfg(feature = "use-rustls-no-provider")]
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

#[cfg(feature = "use-rustls-no-provider")]
fn default_tls_configuration() -> TlsConfiguration {
    let mut root_cert_store = RootCertStore::empty();
    for cert in load_native_certs().expect("could not load platform certs") {
        root_cert_store.add(cert).unwrap();
    }

    let tls_config = ClientConfig::builder()
        .with_root_certificates(root_cert_store)
        .with_no_client_auth();

    TlsConfiguration::Rustls(Arc::new(tls_config))
}

#[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
fn default_tls_configuration() -> TlsConfiguration {
    TlsConfiguration::Native
}

/// Transport methods. Defaults to TCP.
#[derive(Clone)]
pub enum Transport {
    Tcp,
    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    Tls(TlsConfiguration),
    #[cfg(unix)]
    Unix,
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    Ws,
    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    Wss(TlsConfiguration),
}

impl Default for Transport {
    fn default() -> Self {
        Self::tcp()
    }
}

impl Transport {
    /// Use regular tcp as transport (default)
    #[must_use]
    pub fn tcp() -> Self {
        Self::Tcp
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[must_use]
    pub fn tls_with_default_config() -> Self {
        Self::tls_with_config(default_tls_configuration())
    }

    /// Use secure tcp with tls as transport
    #[cfg(feature = "use-rustls-no-provider")]
    #[must_use]
    pub fn tls(
        ca: Vec<u8>,
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
        alpn: Option<Vec<Vec<u8>>>,
    ) -> Self {
        let config = TlsConfiguration::Simple {
            ca,
            alpn,
            client_auth,
        };

        Self::tls_with_config(config)
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[must_use]
    pub fn tls_with_config(tls_config: TlsConfiguration) -> Self {
        Self::Tls(tls_config)
    }

    #[cfg(unix)]
    #[must_use]
    pub fn unix() -> Self {
        Self::Unix
    }

    /// Use websockets as transport
    #[cfg(feature = "websocket")]
    #[cfg_attr(docsrs, doc(cfg(feature = "websocket")))]
    pub fn ws() -> Self {
        Self::Ws
    }

    /// Use secure websockets with tls as transport
    #[cfg(all(feature = "use-rustls-no-provider", feature = "websocket"))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(feature = "use-rustls-no-provider", feature = "websocket")))
    )]
    pub fn wss(
        ca: Vec<u8>,
        client_auth: Option<(Vec<u8>, Vec<u8>)>,
        alpn: Option<Vec<Vec<u8>>>,
    ) -> Self {
        let config = TlsConfiguration::Simple {
            ca,
            client_auth,
            alpn,
        };

        Self::wss_with_config(config)
    }

    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    pub fn wss_with_config(tls_config: TlsConfiguration) -> Self {
        Self::Wss(tls_config)
    }

    #[cfg(all(
        any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
        feature = "websocket"
    ))]
    #[cfg_attr(
        docsrs,
        doc(cfg(all(
            any(feature = "use-rustls-no-provider", feature = "use-native-tls"),
            feature = "websocket"
        )))
    )]
    pub fn wss_with_default_config() -> Self {
        Self::Wss(default_tls_configuration())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-aws-lc", feature = "use-rustls-ring")
    ))]
    #[test]
    fn tls_default_config_uses_rustls() {
        match super::Transport::tls_with_default_config() {
            super::Transport::Tls(rumqttc_core::TlsConfiguration::Rustls(_)) => {}
            _ => panic!("expected rustls default tls configuration"),
        }
    }

    #[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
    #[test]
    fn tls_default_config_uses_native_tls_when_rustls_is_disabled() {
        match super::Transport::tls_with_default_config() {
            super::Transport::Tls(rumqttc_core::TlsConfiguration::Native) => {}
            _ => panic!("expected native-tls default tls configuration"),
        }
    }

    #[cfg(all(
        feature = "websocket",
        feature = "use-rustls-no-provider",
        any(feature = "use-rustls-aws-lc", feature = "use-rustls-ring")
    ))]
    #[test]
    fn wss_default_config_uses_rustls() {
        match super::Transport::wss_with_default_config() {
            super::Transport::Wss(rumqttc_core::TlsConfiguration::Rustls(_)) => {}
            _ => panic!("expected rustls default wss configuration"),
        }
    }

    #[cfg(all(
        feature = "websocket",
        feature = "use-native-tls",
        not(feature = "use-rustls-no-provider")
    ))]
    #[test]
    fn wss_default_config_uses_native_tls_when_rustls_is_disabled() {
        match super::Transport::wss_with_default_config() {
            super::Transport::Wss(rumqttc_core::TlsConfiguration::Native) => {}
            _ => panic!("expected native-tls default wss configuration"),
        }
    }
}
