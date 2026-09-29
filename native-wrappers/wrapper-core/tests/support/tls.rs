use std::io::{Read, Write};
use std::sync::Arc;

use rumqttc_wrapper_core::{TlsBackend, TlsConfig, TlsRootPolicy};

pub trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

pub struct Fixture {
    pub server: Arc<rustls::ServerConfig>,
    pub pem: String,
    #[allow(dead_code)]
    pub key_pem: String,
}

// This is a runtime operation in providerless builds, even though other
// configurations compile an empty body that Clippy could auto-fix to `const`.
#[allow(clippy::missing_const_for_fn)]
pub fn install_provider_for_providerless_client() {
    #[cfg(all(
        feature = "use-rustls-no-provider",
        not(any(feature = "use-rustls-ring", feature = "use-rustls-aws-lc"))
    ))]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

impl Fixture {
    pub fn new() -> Self {
        install_provider_for_providerless_client();
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
            "localhost".into(),
            "broker.invalid".into(),
            "127.0.0.1".into(),
            "127.0.0.2".into(),
            "::1".into(),
        ])
        .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
        Self {
            server: Arc::new(server),
            pem: cert.pem(),
            key_pem: signing_key.serialize_pem(),
        }
    }

    pub fn client(&self, backend: TlsBackend) -> TlsConfig {
        TlsConfig {
            backend,
            roots: TlsRootPolicy::Pem(self.pem.clone().into()),
            ..Default::default()
        }
    }
}

pub fn wrap(stream: Box<dyn Duplex>, server: Arc<rustls::ServerConfig>) -> Box<dyn Duplex> {
    Box::new(rustls::StreamOwned::new(
        rustls::ServerConnection::new(server).unwrap(),
        stream,
    ))
}
