pub use rumqttc_core::{
    UrlError, ValidationError, WsAdapter, split_url, validate_response_headers,
};

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub use rumqttc_core::split_url_with_default_port;
