//! Owned synchronous TLS hooks and enforceable advanced policies.
use std::sync::Arc;
use std::time::Instant;

use crate::{Error, Result, TlsBackend};
use bytes::Bytes;

pub const MAX_TLS_CIPHER_SUITES: usize = 64;
pub const MAX_TLS_IDENTITIES: usize = 32;
pub const MAX_TLS_CERTIFICATES: usize = 32;
pub const MAX_TLS_METADATA_BYTES: usize = 1024 * 1024;
pub const MAX_TLS_KEY_ID_BYTES: usize = 256;
pub const MAX_TLS_SIGNATURE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsSniPolicy {
    #[default]
    Default = 0,
    Enabled = 1,
    Disabled = 2,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsResumptionPolicy {
    #[default]
    Default = 0,
    Disabled = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsLayer {
    Broker = 0,
    Proxy = 1,
    Redirect = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsCallbackStage {
    Verification = 0,
    IdentitySelection = 1,
    Signing = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[repr(u32)]
pub enum TlsCallbackReason {
    #[error("TLS callback rejected authentication")]
    Rejected = 1,
    #[error("TLS callback failed")]
    Failed = 2,
    #[error("invalid TLS callback response")]
    InvalidResponse = 3,
    #[error("invalid external TLS signature")]
    InvalidSignature = 4,
    #[error("TLS callback resource limit exceeded")]
    ResourceLimit = 5,
    #[error("TLS callback panicked")]
    Panic = 6,
    #[error("TLS callback deadline expired")]
    Timeout = 7,
    #[error("TLS callback temporarily unavailable")]
    Transient = 8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct TlsCallbackFailure {
    pub stage: TlsCallbackStage,
    pub reason: TlsCallbackReason,
    pub layer: TlsLayer,
}
impl TlsCallbackFailure {
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self.reason,
            TlsCallbackReason::Timeout | TlsCallbackReason::Transient
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TlsAdvancedCapabilities {
    pub sni_policies: u32,
    pub resumption_policies: u32,
    pub cipher_selection: bool,
    pub supplemental_verification: bool,
    pub external_identities: bool,
}
impl TlsBackend {
    #[must_use]
    pub const fn advanced_capabilities(self) -> TlsAdvancedCapabilities {
        match self {
            Self::Rustls if cfg!(feature = "use-rustls-no-provider") => TlsAdvancedCapabilities {
                sni_policies: 7,
                resumption_policies: 3,
                cipher_selection: true,
                supplemental_verification: true,
                external_identities: true,
            },
            Self::Native if cfg!(feature = "use-native-tls") => TlsAdvancedCapabilities {
                sni_policies: 7,
                resumption_policies: 1,
                cipher_selection: false,
                supplemental_verification: false,
                external_identities: false,
            },
            _ => TlsAdvancedCapabilities {
                sni_policies: 0,
                resumption_policies: 0,
                cipher_selection: false,
                supplemental_verification: false,
                external_identities: false,
            },
        }
    }

    /// List the selected provider's available cipher suites, in preference order.
    ///
    /// # Errors
    /// Returns an error if Rustls or its provider is unavailable.
    pub fn supported_cipher_suites(self) -> Result<Vec<u16>> {
        #[cfg(feature = "use-rustls-no-provider")]
        if self == Self::Rustls {
            return crate::backend::tls::supported_cipher_suites();
        }
        Err(Error::configuration("TLS cipher selection is unavailable"))
    }
    /// List supported external signature schemes in provider preference order.
    ///
    /// # Errors
    /// Returns an error if Rustls or its provider is unavailable.
    pub fn supported_signature_schemes(self) -> Result<Vec<u16>> {
        #[cfg(feature = "use-rustls-no-provider")]
        if self == Self::Rustls {
            return crate::backend::tls::supported_signature_schemes();
        }
        Err(Error::configuration("external TLS signing is unavailable"))
    }
}

/// Borrowed certificate policy inputs. Signatures are verified subsequently by TLS.
/// Certificate views are valid only during the synchronous callback.
pub struct TlsVerificationRequest<'a> {
    pub server_name: &'a str,
    pub layer: TlsLayer,
    pub certificates: &'a [&'a [u8]],
    pub ocsp_response: &'a [u8],
    pub unix_time: u64,
    pub deadline: Option<Instant>,
}
impl std::fmt::Debug for TlsVerificationRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsVerificationRequest([REDACTED])")
    }
}
/// Synchronous additional verification. Cannot authorize invalid certificates or pins.
pub trait TlsVerifier: Send + Sync + 'static {
    /// # Errors
    /// Return a typed rejection or failure to stop this handshake.
    fn verify(
        &self,
        request: &TlsVerificationRequest<'_>,
    ) -> std::result::Result<(), TlsCallbackReason>;
}
#[derive(Clone)]
pub struct TlsVerifierConfig(pub Arc<dyn TlsVerifier>);
impl std::fmt::Debug for TlsVerifierConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsVerifierConfig([REDACTED])")
    }
}
impl PartialEq for TlsVerifierConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TlsVerifierConfig {}

/// Public certificate chain and opaque host key reference, owned by the registration.
#[derive(Clone, PartialEq, Eq)]
pub struct TlsExternalIdentity {
    pub certificate_pem: Bytes,
    pub key_id: Bytes,
    /// IANA signature schemes, in signing preference order.
    pub signature_schemes: Vec<u16>,
}
impl std::fmt::Debug for TlsExternalIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsExternalIdentity([REDACTED])")
    }
}
pub struct TlsIdentityRequest<'a> {
    pub server_name: &'a str,
    pub layer: TlsLayer,
    pub issuer_hints: &'a [&'a [u8]],
    pub signature_schemes: &'a [u16],
    pub deadline: Option<Instant>,
}
pub struct TlsSigningRequest<'a> {
    pub server_name: &'a str,
    pub layer: TlsLayer,
    pub identity_index: usize,
    pub key_id: &'a [u8],
    pub signature_scheme: u16,
    /// Exactly the unhashed message provided by Rustls. Hash according to the scheme.
    pub message: &'a [u8],
    pub deadline: Option<Instant>,
}
macro_rules! redacted_request {
    ($t:ident) => {
        impl std::fmt::Debug for $t<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($t), "([REDACTED])"))
            }
        }
    };
}
redacted_request!(TlsIdentityRequest);
redacted_request!(TlsSigningRequest);
/// Callbacks run synchronously on the driver thread and may overlap across clients.
/// Return promptly; never wait for the same driver's MQTT progress. Keys stay in the host.
pub trait TlsIdentityProvider: Send + Sync + 'static {
    /// # Errors
    /// Return a typed failure; `Ok(None)` intentionally declines client authentication.
    fn select(
        &self,
        request: &TlsIdentityRequest<'_>,
    ) -> std::result::Result<Option<usize>, TlsCallbackReason>;
    /// # Errors
    /// Return a typed failure when the host cannot sign the exact message.
    fn sign(
        &self,
        request: &TlsSigningRequest<'_>,
    ) -> std::result::Result<Vec<u8>, TlsCallbackReason>;
}
#[derive(Clone)]
pub struct TlsExternalIdentityConfig {
    identities: Arc<[TlsExternalIdentity]>,
    pub(crate) provider: Arc<dyn TlsIdentityProvider>,
}
impl TlsExternalIdentityConfig {
    /// Copy and validate a fixed catalog without invoking host callbacks.
    ///
    /// # Errors
    /// Returns an error for malformed credentials, unsupported schemes or resource bounds.
    pub fn new(
        identities: Vec<TlsExternalIdentity>,
        provider: Arc<dyn TlsIdentityProvider>,
    ) -> Result<Self> {
        if identities.is_empty() || identities.len() > MAX_TLS_IDENTITIES {
            return Err(Error::configuration("invalid external TLS identity count"));
        }
        let mut bytes = 0usize;
        for identity in &identities {
            bytes = bytes
                .checked_add(identity.certificate_pem.len())
                .and_then(|n| n.checked_add(identity.key_id.len()))
                .ok_or_else(|| Error::configuration("external TLS catalog is too large"))?;
            if bytes > MAX_TLS_METADATA_BYTES
                || identity.key_id.is_empty()
                || identity.key_id.len() > MAX_TLS_KEY_ID_BYTES
            {
                return Err(Error::configuration(
                    "external TLS catalog exceeds resource bounds",
                ));
            }
        }
        #[cfg(feature = "use-rustls-no-provider")]
        crate::backend::tls::validate_external_identities(&identities)?;
        #[cfg(not(feature = "use-rustls-no-provider"))]
        {
            let _ = provider;
            return Err(Error::configuration(
                "external TLS identities require Rustls",
            ));
        }
        #[cfg(feature = "use-rustls-no-provider")]
        Ok(Self {
            identities: identities.into(),
            provider,
        })
    }
    #[must_use]
    pub fn identities(&self) -> &[TlsExternalIdentity] {
        &self.identities
    }
}
impl std::fmt::Debug for TlsExternalIdentityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsExternalIdentityConfig([REDACTED])")
    }
}
impl PartialEq for TlsExternalIdentityConfig {
    fn eq(&self, other: &Self) -> bool {
        self.identities == other.identities && Arc::ptr_eq(&self.provider, &other.provider)
    }
}
impl Eq for TlsExternalIdentityConfig {}

#[allow(clippy::redundant_pub_crate)] // This helper must not enter the public glob export.
pub(crate) fn callback_failure(
    error: &(dyn std::error::Error + 'static),
) -> Option<TlsCallbackFailure> {
    if let Some(failure) = error.downcast_ref::<TlsCallbackFailure>() {
        return Some(*failure);
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>()
        && let Some(inner) = error.get_ref()
        && let Some(failure) = callback_failure(inner)
    {
        return Some(failure);
    }
    error.source().and_then(callback_failure)
}
