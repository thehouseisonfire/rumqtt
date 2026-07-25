/// Required byte length of an envelope domain tag.
pub const DOMAIN_TAG_LEN: usize = 8;
/// The only envelope version emitted and accepted by this release.
pub const ENVELOPE_VERSION_V1: u16 = 1;
/// Maximum length, including the leading dot, of a filename suffix.
pub const MAX_FILENAME_SUFFIX_LEN: usize = 32;
pub(crate) const HEADER_LEN: usize = 18;
pub(crate) const CHECKSUM_LEN: usize = 4;
pub(crate) const STREAM_CHUNK_SIZE: usize = 64 * 1024;
#[cfg(any(unix, windows))]
pub(crate) const STREAM_CHANNEL_CAPACITY: usize = 2;

/// The default maximum canonical blob payload size (64 MiB).
pub const DEFAULT_MAX_BLOB_SIZE: u64 = 64 * 1024 * 1024;
/// Default bound for concurrently active different-key operations.
pub const DEFAULT_MAX_CONCURRENT_OPERATIONS: usize = 4;

/// Immutable identity of one application blob format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobFormatIdentity {
    pub(crate) domain_tag: [u8; DOMAIN_TAG_LEN],
    pub(crate) filename_suffix: String,
    pub(crate) envelope_version: u16,
}

impl BlobFormatIdentity {
    /// Validates and constructs a format identity.
    ///
    /// The domain must contain exactly eight bytes. The suffix must start with
    /// `.` and contain only lowercase ASCII letters, digits, `_`, or `-`.
    ///
    /// # Errors
    ///
    /// Returns a precise configuration error for an invalid domain length,
    /// unsafe suffix, or unsupported envelope version.
    pub fn new(
        domain_tag: impl AsRef<[u8]>,
        filename_suffix: impl Into<String>,
        envelope_version: u16,
    ) -> Result<Self, AtomicBlobStoreConfigError> {
        let domain = domain_tag.as_ref();
        let domain_tag = <[u8; DOMAIN_TAG_LEN]>::try_from(domain).map_err(|_| {
            AtomicBlobStoreConfigError::InvalidDomainTagLength {
                found: domain.len(),
            }
        })?;
        let filename_suffix = filename_suffix.into();
        validate_suffix(&filename_suffix)?;
        if envelope_version != ENVELOPE_VERSION_V1 {
            return Err(
                AtomicBlobStoreConfigError::UnsupportedConfiguredEnvelopeVersion {
                    found: envelope_version,
                },
            );
        }
        Ok(Self {
            domain_tag,
            filename_suffix,
            envelope_version,
        })
    }

    #[must_use]
    pub const fn domain_tag(&self) -> &[u8; DOMAIN_TAG_LEN] {
        &self.domain_tag
    }

    #[must_use]
    pub fn filename_suffix(&self) -> &str {
        &self.filename_suffix
    }

    #[must_use]
    pub const fn envelope_version(&self) -> u16 {
        self.envelope_version
    }
}

/// Configuration shared by the blocking and Tokio store facades.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicBlobStoreOptions {
    pub(crate) format: BlobFormatIdentity,
    pub(crate) max_blob_size: u64,
    pub(crate) max_concurrent_operations: NonZeroUsize,
}

impl AtomicBlobStoreOptions {
    #[must_use]
    ///
    /// # Panics
    ///
    /// The compile-time default concurrency is asserted to be nonzero.
    pub const fn new(format: BlobFormatIdentity) -> Self {
        Self {
            format,
            max_blob_size: DEFAULT_MAX_BLOB_SIZE,
            max_concurrent_operations: NonZeroUsize::new(DEFAULT_MAX_CONCURRENT_OPERATIONS)
                .expect("the default concurrency is nonzero"),
        }
    }

    #[must_use]
    pub const fn with_max_blob_size(mut self, maximum: u64) -> Self {
        self.max_blob_size = maximum;
        self
    }

    #[must_use]
    pub const fn with_max_concurrent_operations(mut self, maximum: NonZeroUsize) -> Self {
        self.max_concurrent_operations = maximum;
        self
    }

    #[must_use]
    pub const fn format(&self) -> &BlobFormatIdentity {
        &self.format
    }

    #[must_use]
    pub const fn max_blob_size(&self) -> u64 {
        self.max_blob_size
    }

    #[must_use]
    pub const fn max_concurrent_operations(&self) -> NonZeroUsize {
        self.max_concurrent_operations
    }
}

/// Invalid immutable store configuration.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AtomicBlobStoreConfigError {
    #[error("domain tag must contain exactly {DOMAIN_TAG_LEN} bytes; found {found}")]
    InvalidDomainTagLength { found: usize },
    #[error(
        "filename suffix must match \\.[a-z0-9_-]+ and be at most {MAX_FILENAME_SUFFIX_LEN} bytes"
    )]
    InvalidFilenameSuffix,
    #[error("configured envelope version {found} is not supported")]
    UnsupportedConfiguredEnvelopeVersion { found: u16 },
    #[error("namespace must be one non-empty normal path component")]
    InvalidNamespace,
    #[error("maximum blob size {maximum} leaves no room for the envelope header and checksum")]
    InvalidMaximumBlobSize { maximum: u64 },
}

fn validate_suffix(suffix: &str) -> Result<(), AtomicBlobStoreConfigError> {
    let valid = (2..=MAX_FILENAME_SUFFIX_LEN).contains(&suffix.len())
        && suffix.starts_with('.')
        && suffix[1..].bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(AtomicBlobStoreConfigError::InvalidFilenameSuffix)
    }
}
use std::num::NonZeroUsize;
