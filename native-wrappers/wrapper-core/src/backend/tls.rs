#[cfg(feature = "use-rustls-no-provider")]
#[path = "tls_advanced.rs"]
mod advanced;

use super::{Error, ErrorKind, Result};
#[cfg(feature = "use-rustls-no-provider")]
use rumqttc_v4::tokio_rustls::rustls;

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
pub fn build_tls(config: &crate::TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    build_tls_for_layer(config, crate::TlsLayer::Broker)
}

pub fn build_tls_for_layer(
    config: &crate::TlsConfig,
    layer: crate::TlsLayer,
) -> Result<rumqttc_v4::TlsConfiguration> {
    #[cfg(not(feature = "use-rustls-no-provider"))]
    let _ = layer;
    config.validate_options()?;
    match config.backend {
        #[cfg(feature = "use-rustls-no-provider")]
        crate::TlsBackend::Rustls => build_rustls(config, layer),
        #[cfg(feature = "use-native-tls")]
        crate::TlsBackend::Native => build_native_tls(config),
        #[allow(unreachable_patterns)]
        _ => Err(Error::configuration("selected TLS backend is disabled")),
    }
}

#[cfg(feature = "use-rustls-no-provider")]
fn build_rustls(
    config: &crate::TlsConfig,
    layer: crate::TlsLayer,
) -> Result<rumqttc_v4::TlsConfiguration> {
    use rumqttc_v4::tokio_rustls::rustls::{
        ClientConfig, RootCertStore,
        client::WebPkiServerVerifier,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    };
    use std::sync::Arc;
    let invalid = || {
        Error::new(
            ErrorKind::Tls,
            "failed to construct rustls credentials or policy",
        )
    };
    let mut provider = rumqttc_core::rustls_crypto_provider().map_err(|_| invalid())?;
    if !config.cipher_suites.is_empty() {
        let mut selected = (*provider).clone();
        selected.cipher_suites = config
            .cipher_suites
            .iter()
            .map(|id| {
                provider
                    .cipher_suites
                    .iter()
                    .find(|suite| u16::from(suite.suite()) == *id)
                    .copied()
                    .ok_or_else(|| {
                        Error::configuration("requested TLS cipher suite is unavailable")
                    })
            })
            .collect::<Result<_>>()?;
        provider = Arc::new(selected);
    }
    let mut roots = match &config.roots {
        crate::TlsRootPolicy::Platform | crate::TlsRootPolicy::PlatformAndPem(_) => {
            rumqttc_core::rustls_native_root_store().map_err(|_| invalid())?
        }
        crate::TlsRootPolicy::Pem(_) => RootCertStore::empty(),
    };
    if let crate::TlsRootPolicy::Pem(pem) | crate::TlsRootPolicy::PlatformAndPem(pem) =
        &config.roots
    {
        let supplied = rumqttc_core::rustls_pem_root_store(pem).map_err(|_| invalid())?;
        roots.roots.extend(supplied.roots);
    }
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider));
    let builder = match config.version_policy {
        crate::TlsVersionPolicy::Default => builder.with_safe_default_protocol_versions(),
        #[cfg(feature = "tls12")]
        crate::TlsVersionPolicy::Tls12Only => {
            builder.with_protocol_versions(&[&rustls::version::TLS12])
        }
        #[cfg(not(feature = "tls12"))]
        crate::TlsVersionPolicy::Tls12Only => {
            return Err(Error::configuration("Rustls TLS 1.2 support is disabled"));
        }
        crate::TlsVersionPolicy::Tls13Only => {
            builder.with_protocol_versions(&[&rustls::version::TLS13])
        }
        #[cfg(feature = "tls12")]
        crate::TlsVersionPolicy::Tls12OrTls13 => {
            builder.with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        }
        #[cfg(not(feature = "tls12"))]
        crate::TlsVersionPolicy::Tls12OrTls13 => {
            builder.with_protocol_versions(&[&rustls::version::TLS13])
        }
    }
    .map_err(|_| Error::configuration("TLS cipher suites and versions have no usable overlap"))?;
    let has_hooks = config.verifier.is_some()
        || matches!(config.identity, Some(crate::TlsClientIdentity::External(_)));
    let verifier = if config.pins.is_empty() && !has_hooks {
        None
    } else {
        Some(
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots.clone()), provider)
                .build()
                .map_err(|_| invalid())?,
        )
    };
    let builder = builder.with_root_certificates(roots);
    let client = match &config.identity {
        Some(crate::TlsClientIdentity::RustlsPem {
            certificate,
            private_key,
        }) => {
            let chain = CertificateDer::pem_slice_iter(certificate)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| invalid())?;
            if chain.is_empty() {
                return Err(invalid());
            }
            let key = PrivateKeyDer::from_pem_slice(private_key.expose()).map_err(|_| invalid())?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|_| invalid())?
        }
        None | Some(crate::TlsClientIdentity::External(_)) => builder.with_no_client_auth(),
        _ => return Err(Error::configuration("rustls requires a PEM identity")),
    };
    finish_rustls(config, client, verifier, layer)
}

#[cfg(feature = "use-rustls-no-provider")]
fn finish_rustls(
    config: &crate::TlsConfig,
    mut client: rustls::ClientConfig,
    verifier: Option<std::sync::Arc<rustls::client::WebPkiServerVerifier>>,
    layer: crate::TlsLayer,
) -> Result<rumqttc_v4::TlsConfiguration> {
    use rustls::client::Resumption;
    use std::sync::Arc;
    let has_hooks = config.verifier.is_some()
        || matches!(config.identity, Some(crate::TlsClientIdentity::External(_)));
    client.alpn_protocols.clone_from(&config.alpn_protocols);
    if config.sni_policy != crate::TlsSniPolicy::Default {
        client.enable_sni = config.sni_policy == crate::TlsSniPolicy::Enabled;
    }
    if config.resumption_policy == crate::TlsResumptionPolicy::Disabled
        || has_hooks
        || !config.pins.is_empty()
    {
        client.resumption = Resumption::disabled();
    }
    if let Some(standard) = verifier {
        let standard: Arc<dyn rustls::client::danger::ServerCertVerifier> =
            if config.pins.is_empty() {
                standard
            } else {
                Arc::new(PinnedVerifier {
                    standard,
                    pins: config.pins.clone(),
                })
            };
        if has_hooks {
            let external = match &config.identity {
                Some(crate::TlsClientIdentity::External(value)) => Some(value.clone()),
                _ => None,
            };
            let identities = external
                .as_ref()
                .map(|value| advanced::parse_identities(value.identities()))
                .transpose()?
                .unwrap_or_default();
            return Ok(rumqttc_v4::TlsConfiguration::Connector(Arc::new(
                advanced::Connector {
                    template: Arc::new(client),
                    standard,
                    verifier: config.verifier.clone(),
                    external,
                    identities,
                    layer,
                },
            )));
        }
        client.dangerous().set_certificate_verifier(standard);
    }
    Ok(rumqttc_v4::TlsConfiguration::Rustls(Arc::new(client)))
}

#[cfg(feature = "use-native-tls")]
fn build_native_tls(config: &crate::TlsConfig) -> Result<rumqttc_v4::TlsConfiguration> {
    use crate::TlsVersionPolicy;
    let invalid = || Error::new(ErrorKind::Tls, "failed to construct native TLS credentials");
    let mut builder = native_tls::TlsConnector::builder();
    if config.sni_policy != crate::TlsSniPolicy::Default {
        builder.use_sni(config.sni_policy == crate::TlsSniPolicy::Enabled);
    }
    match config.version_policy {
        TlsVersionPolicy::Default => {}
        TlsVersionPolicy::Tls12Only => {
            builder.min_protocol_version(Some(native_tls::Protocol::Tlsv12));
            builder.max_protocol_version(Some(native_tls::Protocol::Tlsv12));
        }
        TlsVersionPolicy::Tls13Only => {
            builder.min_protocol_version(Some(native_tls::Protocol::Tlsv13));
            builder.max_protocol_version(Some(native_tls::Protocol::Tlsv13));
        }
        TlsVersionPolicy::Tls12OrTls13 => {
            builder.min_protocol_version(Some(native_tls::Protocol::Tlsv12));
            builder.max_protocol_version(Some(native_tls::Protocol::Tlsv13));
        }
    }
    if let crate::TlsRootPolicy::Pem(ca) | crate::TlsRootPolicy::PlatformAndPem(ca) = &config.roots
    {
        builder.disable_built_in_roots(matches!(config.roots, crate::TlsRootPolicy::Pem(_)));
        // native-tls accepts one PEM certificate per call. Split a bundle explicitly.
        let pem = std::str::from_utf8(ca).map_err(|_| invalid())?;
        let mut rest = pem.trim();
        let mut count = 0;
        while !rest.is_empty() {
            if !rest.starts_with("-----BEGIN CERTIFICATE-----") {
                return Err(invalid());
            }
            let end = rest.find("-----END CERTIFICATE-----").ok_or_else(invalid)?
                + "-----END CERTIFICATE-----".len();
            builder.add_root_certificate(
                native_tls::Certificate::from_pem(&rest.as_bytes()[..end])
                    .map_err(|_| invalid())?,
            );
            count += 1;
            rest = rest[end..].trim();
        }
        if count == 0 {
            return Err(invalid());
        }
    }
    if let Some(crate::TlsClientIdentity::NativePkcs12 { identity, password }) = &config.identity {
        let password = std::str::from_utf8(password.expose()).map_err(|_| invalid())?;
        builder.identity(
            native_tls::Identity::from_pkcs12(identity.expose(), password)
                .map_err(|_| invalid())?,
        );
    }
    let alpn: Vec<&str> = config
        .alpn_protocols
        .iter()
        .map(|value| std::str::from_utf8(value).map_err(|_| invalid()))
        .collect::<Result<_>>()?;
    builder.request_alpns(&alpn);
    Ok(rumqttc_v4::TlsConfiguration::NativeConnector(
        builder.build().map_err(|_| invalid())?,
    ))
}

#[cfg(feature = "use-rustls-no-provider")]
mod pinning {
    use std::sync::Arc;

    use rumqttc_v4::tokio_rustls::rustls::{
        self, DigitallySignedStruct, SignatureScheme,
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        server::ParsedCertificate,
    };
    use sha2::{Digest, Sha256};

    use crate::{TlsPin, TlsPinTarget};

    #[derive(Debug)]
    pub(super) struct PinnedVerifier {
        pub standard: Arc<dyn ServerCertVerifier>,
        pub pins: Vec<TlsPin>,
    }

    impl ServerCertVerifier for PinnedVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            server_name: &ServerName<'_>,
            ocsp_response: &[u8],
            now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            let verified = self.standard.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            )?;
            let certificate: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
            let spki = if self
                .pins
                .iter()
                .any(|pin| pin.target == TlsPinTarget::LeafSpki)
            {
                Some(
                    Sha256::digest(
                        ParsedCertificate::try_from(end_entity)?
                            .subject_public_key_info()
                            .as_ref(),
                    )
                    .into(),
                )
            } else {
                None
            };
            if self.pins.iter().any(|pin| match pin.target {
                TlsPinTarget::LeafCertificate => pin.sha256 == certificate,
                TlsPinTarget::LeafSpki => Some(pin.sha256) == spki,
            }) {
                Ok(verified)
            } else {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ))
            }
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            self.standard.verify_tls12_signature(message, cert, dss)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            self.standard.verify_tls13_signature(message, cert, dss)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.standard.supported_verify_schemes()
        }

        fn root_hint_subjects(&self) -> Option<&[rustls::DistinguishedName]> {
            self.standard.root_hint_subjects()
        }
    }
}

#[cfg(feature = "use-rustls-no-provider")]
use pinning::PinnedVerifier;

#[cfg(feature = "use-rustls-no-provider")]
pub fn supported_cipher_suites() -> Result<Vec<u16>> {
    let provider = rumqttc_core::rustls_crypto_provider()
        .map_err(|_| Error::configuration("Rustls provider is unavailable"))?;
    Ok(provider
        .cipher_suites
        .iter()
        .map(|suite| u16::from(suite.suite()))
        .collect())
}
#[cfg(feature = "use-rustls-no-provider")]
pub fn supported_signature_schemes() -> Result<Vec<u16>> {
    let provider = rumqttc_core::rustls_crypto_provider()
        .map_err(|_| Error::configuration("Rustls provider is unavailable"))?;
    Ok(provider
        .signature_verification_algorithms
        .supported_schemes()
        .into_iter()
        .map(u16::from)
        .filter(|id| {
            matches!(
                id,
                0x0401
                    | 0x0501
                    | 0x0601
                    | 0x0804
                    | 0x0805
                    | 0x0806
                    | 0x0403
                    | 0x0503
                    | 0x0603
                    | 0x0807
            )
        })
        .collect())
}
#[cfg(feature = "use-rustls-no-provider")]
pub fn validate_external_identities(identities: &[crate::TlsExternalIdentity]) -> Result<()> {
    advanced::parse_identities(identities)?;
    Ok(())
}
