#![cfg(all(
    feature = "use-rustls-no-provider",
    not(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))
))]

use bytes::Bytes;
use rumqttc_wrapper_core::{
    ClientConfig, ErrorKind, NativeClient, TlsConfig, TlsRootPolicy, TransportConfig,
};
use std::time::Duration;

#[test]
fn providerless_rustls_requires_a_process_default_provider() {
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .unwrap()
        .cert
        .pem();

    let config = |mqtt5| {
        let mut config = if mqtt5 {
            ClientConfig::v5("provider-test", "localhost", 8883)
        } else {
            ClientConfig::v4("provider-test", "localhost", 8883)
        };
        config.common.transport = TransportConfig::Tls(TlsConfig {
            roots: TlsRootPolicy::Pem(Bytes::from(certificate.clone())),
            ..TlsConfig::default()
        });
        config
    };
    for mqtt5 in [false, true] {
        assert_eq!(
            NativeClient::start(config(mqtt5)).unwrap_err().kind(),
            ErrorKind::Tls
        );
    }
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap_or_else(|_| panic!("failed to install process default provider"));
    for mqtt5 in [false, true] {
        let client = NativeClient::start(config(mqtt5)).unwrap();
        client.closer().close_now(Duration::from_secs(5)).unwrap();
        client.join(Duration::from_secs(5)).unwrap();
    }
}
