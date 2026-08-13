pub(crate) mod v4;
pub(crate) mod v5;

use crate::{DeliveryStatus, Error, ErrorKind, Result, TlsConfig};

pub(crate) enum AdapterDriver {
    V311(Box<rumqttc_v4::EventLoop>),
    V5(Box<rumqttc_v5::EventLoop>),
}

impl AdapterDriver {
    pub(crate) async fn run(
        self,
        context: crate::runtime::DriverContext,
    ) -> crate::runtime::TerminalStatus {
        match self {
            Self::V311(eventloop) => v4::run(eventloop, context).await,
            Self::V5(eventloop) => v5::run(eventloop, context).await,
        }
    }
}

pub(crate) fn build_tls(config: &TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    let client_auth = config
        .client_certificate
        .as_ref()
        .zip(config.private_key.as_ref())
        .map(|(certificate, key)| (certificate.to_vec(), key.to_vec()));
    let result = if let Some(ca) = &config.ca {
        rumqttc_v4::TlsConfiguration::try_rustls_with_pem_roots(ca, client_auth)
    } else {
        rumqttc_v4::TlsConfiguration::try_rustls_with_native_roots(client_auth)
    };
    result.map_err(|error| Error::sourced(ErrorKind::Tls, DeliveryStatus::NotApplicable, error))
}
