#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::TlsError as Error;
#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::tls_connect;
