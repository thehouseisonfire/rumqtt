//! Enforceable TLS policy shared by every native transport layer.

use crate::{Error, Result, TlsBackend, TlsClientIdentity, TlsConfig};

pub const MAX_TLS_PINS: usize = 32;

/// Explicit allowed protocol sets. Default preserves the selected backend's defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsVersionPolicy {
    #[default]
    Default = 0,
    Tls12Only = 1,
    Tls13Only = 2,
    Tls12OrTls13 = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TlsPinTarget {
    LeafCertificate = 0,
    LeafSpki = 1,
}

/// SHA-256 of the leaf certificate DER or its complete DER `SubjectPublicKeyInfo`.
#[derive(Clone, PartialEq, Eq)]
pub struct TlsPin {
    pub target: TlsPinTarget,
    pub sha256: [u8; 32],
}

impl std::fmt::Debug for TlsPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsPin")
            .field("target", &self.target)
            .field("sha256", &"[REDACTED]")
            .finish()
    }
}

/// Supported enforcement, independent of negotiated protocol availability.
/// Each policy mask contains bit `1 << selector`. An unavailable backend has zero masks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TlsCapabilities {
    pub version_policies: u32,
    /// Platform=bit 0, supplied=bit 1, combined=bit 2.
    pub root_policies: u32,
    pub certificate_sha256_pins: bool,
    pub spki_sha256_pins: bool,
}

impl TlsBackend {
    #[must_use]
    pub const fn capabilities(self) -> TlsCapabilities {
        match self {
            Self::Rustls if cfg!(feature = "use-rustls-no-provider") => TlsCapabilities {
                version_policies: if cfg!(feature = "tls12") {
                    0b1111
                } else {
                    0b1101
                },
                root_policies: 0b111,
                certificate_sha256_pins: true,
                spki_sha256_pins: true,
            },
            Self::Native if cfg!(feature = "use-native-tls") => TlsCapabilities {
                version_policies: if cfg!(target_vendor = "apple") {
                    0b1011
                } else if cfg!(any(target_os = "windows", native_tls_version_bounds)) {
                    0b1111
                } else {
                    0b0001
                },
                root_policies: 0b111,
                certificate_sha256_pins: false,
                spki_sha256_pins: false,
            },
            _ => TlsCapabilities {
                version_policies: 0,
                root_policies: 0,
                certificate_sha256_pins: false,
                spki_sha256_pins: false,
            },
        }
    }
}

impl TlsConfig {
    /// Validate inputs and construct a temporary backend configuration without networking.
    ///
    /// Platform roots are consulted again when a client starts.
    ///
    /// # Errors
    /// Returns a configuration error for unsupported policies or malformed options,
    /// or a TLS error if credentials, roots, or backend construction fail.
    pub fn validate(&self) -> Result<()> {
        self.validate_options()?;
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        {
            crate::backend::build_tls(self)?;
        }
        Ok(())
    }

    pub(crate) fn validate_options(&self) -> Result<()> {
        let capabilities = self.backend.capabilities();
        if capabilities.version_policies == 0 {
            return Err(Error::configuration("selected TLS backend is disabled"));
        }
        if capabilities.version_policies & (1 << self.version_policy as u32) == 0 {
            return Err(Error::configuration(
                "TLS version policy is unsupported by this backend",
            ));
        }
        if self.pins.len() > MAX_TLS_PINS {
            return Err(Error::configuration(
                "TLS pin count exceeds the supported limit",
            ));
        }
        if !self.pins.is_empty() && !capabilities.certificate_sha256_pins {
            return Err(Error::configuration(
                "TLS pinning is unsupported by this backend",
            ));
        }
        if matches!(
            (&self.backend, &self.identity),
            (
                TlsBackend::Rustls,
                Some(TlsClientIdentity::NativePkcs12 { .. })
            ) | (
                TlsBackend::Native,
                Some(TlsClientIdentity::RustlsPem { .. })
            )
        ) {
            return Err(Error::configuration(
                "client identity is incompatible with selected TLS backend",
            ));
        }
        if self
            .alpn_protocols
            .iter()
            .any(|value| value.is_empty() || value.len() > 255)
        {
            return Err(Error::configuration(
                "ALPN identifiers must contain 1 to 255 bytes",
            ));
        }
        if self.backend == TlsBackend::Native
            && self
                .alpn_protocols
                .iter()
                .any(|value| std::str::from_utf8(value).is_err())
        {
            return Err(Error::configuration(
                "native TLS ALPN identifiers must be UTF-8",
            ));
        }
        Ok(())
    }
}
