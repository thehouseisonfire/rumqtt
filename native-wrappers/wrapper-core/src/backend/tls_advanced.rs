//! Rustls adapters with fresh failure state for every TLS connection.
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use crate::{
    MAX_TLS_CERTIFICATES, MAX_TLS_METADATA_BYTES, MAX_TLS_SIGNATURE_BYTES, TlsCallbackFailure,
    TlsCallbackReason as Reason, TlsCallbackStage as Stage, TlsExternalIdentity,
    TlsExternalIdentityConfig, TlsIdentityRequest, TlsLayer, TlsSigningRequest,
    TlsVerificationRequest, TlsVerifierConfig,
};
use parking_lot::Mutex;
use rumqttc_v4::tokio_rustls::{TlsConnector, rustls};
use rustls::client::{
    ResolvesClientCert,
    danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
};
use rustls::pki_types::{
    CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime, pem::PemObject,
};
use rustls::sign::{CertifiedKey, Signer, SigningKey};
use rustls::{ClientConfig, SignatureAlgorithm, SignatureScheme};
#[path = "tls_deferred.rs"]
mod deferred;

pub struct Identity {
    pub descriptor: TlsExternalIdentity,
    pub chain: Vec<CertificateDer<'static>>,
    pub algorithm: SignatureAlgorithm,
    pub spki: SubjectPublicKeyInfoDer<'static>,
}
fn invalid() -> crate::Error {
    crate::Error::configuration("invalid external TLS identity or signature schemes")
}

pub fn parse_identities(identities: &[TlsExternalIdentity]) -> crate::Result<Vec<Arc<Identity>>> {
    let provider = rumqttc_core::rustls_crypto_provider().map_err(|_| invalid())?;
    let supported = provider
        .signature_verification_algorithms
        .supported_schemes();
    identities
        .iter()
        .map(|descriptor| {
            let chain = CertificateDer::pem_slice_iter(&descriptor.certificate_pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| invalid())?;
            if chain.is_empty() || chain.len() > MAX_TLS_CERTIFICATES {
                return Err(invalid());
            }
            for cert in &chain {
                let (rest, _) = x509_parser::parse_x509_certificate(cert).map_err(|_| invalid())?;
                if !rest.is_empty() {
                    return Err(invalid());
                }
            }
            let (_, cert) =
                x509_parser::parse_x509_certificate(&chain[0]).map_err(|_| invalid())?;
            let key = cert.public_key();
            let oid = key.algorithm.algorithm.to_id_string();
            let (algorithm, eligible): (SignatureAlgorithm, Vec<u16>) = match oid.as_str() {
                "1.2.840.113549.1.1.1" => {
                    let x509_parser::public_key::PublicKey::RSA(rsa) =
                        key.parsed().map_err(|_| invalid())?
                    else {
                        return Err(invalid());
                    };
                    let bits = rsa.key_size();
                    if !(2048..=8192).contains(&bits) {
                        return Err(invalid());
                    }
                    (
                        SignatureAlgorithm::RSA,
                        vec![0x0804, 0x0805, 0x0806, 0x0401, 0x0501, 0x0601],
                    )
                }
                "1.2.840.10045.2.1" => {
                    let curve = key
                        .algorithm
                        .parameters
                        .as_ref()
                        .ok_or_else(invalid)?
                        .as_oid()
                        .map_err(|_| invalid())?
                        .to_id_string();
                    let scheme = match curve.as_str() {
                        "1.2.840.10045.3.1.7" => 0x0403,
                        "1.3.132.0.34" => 0x0503,
                        "1.3.132.0.35" => 0x0603,
                        _ => return Err(invalid()),
                    };
                    (SignatureAlgorithm::ECDSA, vec![scheme])
                }
                "1.3.101.112" => (SignatureAlgorithm::ED25519, vec![0x0807]),
                _ => return Err(invalid()),
            };
            if descriptor.signature_schemes.is_empty()
                || descriptor.signature_schemes.len() > 10
                || descriptor
                    .signature_schemes
                    .iter()
                    .enumerate()
                    .any(|(i, id)| {
                        !eligible.contains(id)
                            || !supported.contains(&SignatureScheme::from(*id))
                            || descriptor.signature_schemes[..i].contains(id)
                    })
            {
                return Err(invalid());
            }
            let spki = rustls::server::ParsedCertificate::try_from(&chain[0])
                .map_err(|_| invalid())?
                .subject_public_key_info();
            Ok(Arc::new(Identity {
                descriptor: descriptor.clone(),
                chain,
                algorithm,
                spki,
            }))
        })
        .collect()
}

struct State {
    server_name: String,
    layer: TlsLayer,
    deadline: Option<Instant>,
    failure: Mutex<Option<TlsCallbackFailure>>,
    pending: Mutex<Option<Stage>>,
    tls_callbacks: Arc<crate::backend::TlsCallbackMonitor>,
}
impl State {
    fn fail_destruction(&self, stage: Stage, reason: Reason) {
        self.fail(stage, reason);
        // The connecting future may itself be getting dropped. Publish this
        // failure independently of the return path through io_failure().
        self.tls_callbacks.fail(TlsCallbackFailure {
            stage,
            reason,
            layer: self.layer,
        });
    }
    fn fail(&self, stage: Stage, reason: Reason) {
        let mut current = self.failure.lock();
        if reason == Reason::Panic && current.is_some_and(|f| f.reason == Reason::Timeout) {
            *current = None;
        }
        current.get_or_insert(TlsCallbackFailure {
            stage,
            reason,
            layer: self.layer,
        });
    }
    fn call<T>(
        &self,
        stage: Stage,
        callback: impl FnOnce() -> Result<T, Reason>,
    ) -> Result<T, rustls::Error> {
        let result = if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Err(Reason::Timeout)
        } else {
            let result = match catch_unwind(AssertUnwindSafe(|| {
                crate::runtime::with_host_callback(callback)
            })) {
                Ok(result) => result,
                Err(payload) => {
                    std::mem::forget(payload);
                    Err(Reason::Panic)
                }
            };
            if self.deadline.is_some_and(|d| Instant::now() >= d)
                && !matches!(result, Err(Reason::Panic))
            {
                Err(Reason::Timeout)
            } else {
                result
            }
        };
        result.map_err(|reason| {
            self.fail(stage, reason);
            rustls::Error::General("TLS callback failed".into())
        })
    }
    fn io_failure(&self) -> Option<io::Error> {
        self.failure.lock().map(|failure| {
            let kind = if failure.reason == Reason::Timeout {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::PermissionDenied
            };
            let error = io::Error::new(kind, failure);
            if failure.retryable() {
                error
            } else {
                rumqttc_core::TerminalTransportError::new(error).into_io()
            }
        })
    }
}
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsHandshakeState([REDACTED])")
    }
}
pub struct Connector {
    pub template: Arc<ClientConfig>,
    pub standard: Arc<dyn ServerCertVerifier>,
    pub verifier: Option<TlsVerifierConfig>,
    pub external: Option<TlsExternalIdentityConfig>,
    pub async_verifier: Option<crate::AsyncTlsVerifierConfig>,
    pub async_external: Option<crate::AsyncTlsExternalIdentityConfig>,
    pub identities: Vec<Arc<Identity>>,
    pub layer: TlsLayer,
    pub tls_callbacks: Arc<crate::backend::TlsCallbackMonitor>,
}
impl rumqttc_core::TlsHandshakeConnector for Connector {
    fn connect(
        &self,
        server_name: String,
        _port: u16,
        deadline: Option<Instant>,
        stream: rumqttc_core::DynAsyncReadWrite,
    ) -> Pin<Box<dyn Future<Output = io::Result<rumqttc_core::DynAsyncReadWrite>> + Send>> {
        let state = Arc::new(State {
            server_name,
            layer: self.layer,
            deadline,
            failure: Mutex::new(None),
            pending: Mutex::new(None),
            tls_callbacks: self.tls_callbacks.clone(),
        });
        let dispatch = if self.async_verifier.is_some() || self.async_external.is_some() {
            Some(deferred::Dispatch::new(self, state.clone()))
        } else {
            None
        };
        let mut config = (*self.template).clone();
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(Verifier {
                standard: self.standard.clone(),
                host: dispatch
                    .as_ref()
                    .and_then(deferred::Dispatch::verifier)
                    .or_else(|| self.verifier.clone()),
                state: state.clone(),
            }));
        let external = dispatch
            .as_ref()
            .and_then(deferred::Dispatch::external)
            .or_else(|| self.external.clone());
        if let Some(external) = external {
            config.client_auth_cert_resolver = Arc::new(Resolver {
                config: external,
                identities: self.identities.clone(),
                provider: config.crypto_provider().clone(),
                state: state.clone(),
            });
        }
        Box::pin(async move {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(handshake_timeout());
            }
            let name = ServerName::try_from(state.server_name.clone()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name")
            })?;
            let connector = TlsConnector::from(Arc::new(config));
            let handshake = connector.connect(name, stream);
            if let Some(dispatch) = dispatch {
                return dispatch.connect(handshake).await;
            }
            let result = if let Some(deadline) = deadline {
                if let Ok(result) = tokio::time::timeout_at(deadline.into(), handshake).await {
                    result
                } else {
                    return Err(state.io_failure().unwrap_or_else(handshake_timeout));
                }
            } else {
                handshake.await
            };
            if let Some(error) = state.io_failure() {
                return Err(error);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(handshake_timeout());
            }
            // Check callback state before any proxy, WebSocket or MQTT application writes.
            result.map(|stream| Box::new(stream) as rumqttc_core::DynAsyncReadWrite)
        })
    }
}
#[derive(Debug)]
struct Verifier {
    standard: Arc<dyn ServerCertVerifier>,
    host: Option<TlsVerifierConfig>,
    state: Arc<State>,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verified = self
            .standard
            .verify_server_cert(leaf, intermediates, name, ocsp, now)?;
        if let Some(host) = &self.host {
            self.state.call(Stage::Verification, || {
                let bytes = intermediates
                    .iter()
                    .fold(leaf.len().checked_add(ocsp.len()), |sum, cert| {
                        sum.and_then(|n| n.checked_add(cert.len()))
                    });
                if intermediates.len() >= MAX_TLS_CERTIFICATES
                    || bytes.is_none_or(|n| n > MAX_TLS_METADATA_BYTES)
                {
                    return Err(Reason::ResourceLimit);
                }
                let certificates: Vec<&[u8]> = std::iter::once(leaf.as_ref())
                    .chain(intermediates.iter().map(AsRef::as_ref))
                    .collect();
                host.0.verify(&TlsVerificationRequest {
                    server_name: &self.state.server_name,
                    layer: self.state.layer,
                    certificates: &certificates,
                    ocsp_response: ocsp,
                    unix_time: now.as_secs(),
                    deadline: self.state.deadline,
                })
            })?;
        }
        Ok(verified)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.standard.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.standard.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.standard.supported_verify_schemes()
    }
    fn root_hint_subjects(&self) -> Option<&[rustls::DistinguishedName]> {
        self.standard.root_hint_subjects()
    }
}
#[derive(Debug)]
struct Resolver {
    provider: Arc<rustls::crypto::CryptoProvider>,
    config: TlsExternalIdentityConfig,
    identities: Vec<Arc<Identity>>,
    state: Arc<State>,
}
impl ResolvesClientCert for Resolver {
    fn has_certs(&self) -> bool {
        true
    }
    fn resolve(&self, issuers: &[&[u8]], schemes: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        self.state
            .call(Stage::IdentitySelection, || {
                if issuers.len() > MAX_TLS_CERTIFICATES
                    || schemes.len() > 64
                    || issuers
                        .iter()
                        .try_fold(0usize, |sum, hint| sum.checked_add(hint.len()))
                        .is_none_or(|n| n > MAX_TLS_METADATA_BYTES)
                {
                    return Err(Reason::ResourceLimit);
                }
                let schemes: Vec<u16> = schemes.iter().map(|scheme| u16::from(*scheme)).collect();
                let selected = self.config.provider.select(&TlsIdentityRequest {
                    server_name: &self.state.server_name,
                    layer: self.state.layer,
                    issuer_hints: issuers,
                    signature_schemes: &schemes,
                    deadline: self.state.deadline,
                })?;
                selected
                    .map(|index| {
                        let identity = self.identities.get(index).ok_or(Reason::InvalidResponse)?;
                        if !identity
                            .descriptor
                            .signature_schemes
                            .iter()
                            .any(|scheme| schemes.contains(scheme))
                        {
                            return Err(Reason::InvalidResponse);
                        }
                        Ok(Arc::new(CertifiedKey::new(
                            identity.chain.clone(),
                            Arc::new(Key {
                                config: self.config.clone(),
                                identity: identity.clone(),
                                index,
                                provider: self.provider.clone(),
                                state: self.state.clone(),
                            }),
                        )))
                    })
                    .transpose()
            })
            .ok()
            .flatten()
    }
}
#[derive(Debug)]
struct Key {
    provider: Arc<rustls::crypto::CryptoProvider>,
    config: TlsExternalIdentityConfig,
    identity: Arc<Identity>,
    index: usize,
    state: Arc<State>,
}
impl SigningKey for Key {
    fn algorithm(&self) -> SignatureAlgorithm {
        self.identity.algorithm
    }
    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(self.identity.spki.as_ref().into())
    }
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        self.identity
            .descriptor
            .signature_schemes
            .iter()
            .map(|id| SignatureScheme::from(*id))
            .find(|scheme| offered.contains(scheme))
            .map(|scheme| {
                Box::new(ExternalSigner {
                    config: self.config.clone(),
                    identity: self.identity.clone(),
                    index: self.index,
                    provider: self.provider.clone(),
                    state: self.state.clone(),
                    scheme,
                }) as Box<dyn Signer>
            })
    }
}
#[derive(Debug)]
struct ExternalSigner {
    provider: Arc<rustls::crypto::CryptoProvider>,
    config: TlsExternalIdentityConfig,
    identity: Arc<Identity>,
    index: usize,
    state: Arc<State>,
    scheme: SignatureScheme,
}
impl Signer for ExternalSigner {
    fn scheme(&self) -> SignatureScheme {
        self.scheme
    }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        self.state.call(Stage::Signing, || {
            if message.len() > MAX_TLS_METADATA_BYTES {
                return Err(Reason::ResourceLimit);
            }
            let signature = self.config.provider.sign(&TlsSigningRequest {
                server_name: &self.state.server_name,
                layer: self.state.layer,
                identity_index: self.index,
                key_id: &self.identity.descriptor.key_id,
                signature_scheme: u16::from(self.scheme),
                message,
                deadline: self.state.deadline,
            })?;
            if signature.is_empty() || signature.len() > MAX_TLS_SIGNATURE_BYTES {
                return Err(Reason::InvalidResponse);
            }
            let algorithms = self
                .provider
                .signature_verification_algorithms
                .mapping
                .iter()
                .find(|(scheme, _)| *scheme == self.scheme)
                .ok_or(Reason::InvalidResponse)?
                .1;
            let cert = webpki::EndEntityCert::try_from(&self.identity.chain[0])
                .map_err(|_| Reason::InvalidSignature)?;
            if !algorithms.iter().any(|algorithm| {
                cert.verify_signature(*algorithm, message, &signature)
                    .is_ok()
            }) {
                return Err(Reason::InvalidSignature);
            }
            Ok(signature)
        })
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExternalTlsIdentity([REDACTED])")
    }
}
impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsConnector([REDACTED])")
    }
}

fn handshake_timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "TLS handshake deadline expired")
}
