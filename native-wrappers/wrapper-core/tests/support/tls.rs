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

impl Fixture {
    pub fn new() -> Self {
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
            "localhost".into(),
            "broker.invalid".into(),
            "127.0.0.1".into(),
            "127.0.0.2".into(),
            "::1".into(),
        ])
        .unwrap();
        let server = rustls::ServerConfig::builder()
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
