//! Bounded, crash-consistent keyed blob snapshots on trusted local filesystems.
//!
//! The store accepts opaque key and payload bytes. See the crate README for the
//! exact format, non-goals, interruption guarantees, and trust boundary.
//!
//! # Platform and filesystem scope
//!
//! This implementation supports Unix and Windows. Other targets compile, but
//! [`AtomicBlobStore::open`] returns [`AtomicBlobStoreError::UnsupportedPlatform`]. It is
//! intended for ordinary local filesystems; it does not detect or certify NFS,
//! SMB, container volumes, virtual disks, filesystem or mount behavior,
//! controller caches, or persistence under arbitrary power loss.
//!
//! A successful save means the `atomic-write-file` Unix commit operation
//! synchronized its temporary file, atomically renamed it over the canonical
//! path, and synchronized the containing namespace directory. These observable
//! operations establish canonical-path old-or-new process-interruption
//! semantics, not a universal hardware power-loss guarantee.
//!
//! # Trust and concurrency model
//!
//! The configured root and its ancestors must be trusted and controlled by the
//! application. Hash-derived filenames prevent canonical blob key bytes from
//! directly constructing paths outside that root. The store does not defend
//! against another process or attacker that can modify the root, symlink or
//! directory replacement, Windows reparse points, or concurrent blob
//! manipulation.
//!
//! Coordination is process-local and shared by clones of one store. It provides
//! same-key FIFO execution and cancellation-safe completion, but no
//! cross-process locking, distributed locking, leases, fencing,
//! compare-and-swap, or multi-writer coordination. Applications must ensure
//! only one process and one active blob owner writes a key.
//!
//! # Data and recovery limitations
//!
//! CRC32C detects accidental corruption only. The store provides no encryption,
//! authentication, cryptographic integrity, or tamper resistance. Corruption
//! fails closed and is left untouched.
//!
//! Only the canonical configured-suffix path is authoritative. Windows cleanup
//! recognizes only store-owned staging names; Unix never parses
//! dependency-private temporary names.

#[cfg(any(unix, windows))]
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::future::Future;
use std::io;
#[cfg(any(unix, windows))]
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg(any(unix, windows))]
use std::sync::mpsc;

#[cfg(any(unix, windows))]
use tokio::sync::oneshot;

/// Required byte length of an envelope domain tag.
pub const DOMAIN_TAG_LEN: usize = 8;
/// The only envelope version emitted and accepted by this release.
pub const ENVELOPE_VERSION_V1: u16 = 1;
/// Maximum length, including the leading dot, of a filename suffix.
pub const MAX_FILENAME_SUFFIX_LEN: usize = 32;
const HEADER_LEN: usize = 18;
const CHECKSUM_LEN: usize = 4;

/// The default maximum canonical blob payload size (64 MiB).
pub const DEFAULT_MAX_BLOB_SIZE: u64 = 64 * 1024 * 1024;
/// Default bound for concurrently active different-key operations.
pub const DEFAULT_MAX_CONCURRENT_OPERATIONS: usize = 4;

/// Immutable identity of one application blob format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobFormatIdentity {
    domain_tag: [u8; DOMAIN_TAG_LEN],
    filename_suffix: String,
    envelope_version: u16,
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

/// Observable state of a canonical blob.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobState {
    Absent,
    Present,
}

/// Metadata returned without opening or decoding blob contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobInspection {
    pub state: BlobState,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// Location assigned to a quarantined blob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuarantineInfo {
    pub identifier: String,
    pub diagnostic_path: PathBuf,
}

/// One cleanup entry that could not be inspected or removed.
#[derive(Debug)]
pub struct CleanupFailure {
    pub identifier: String,
    pub source: io::Error,
}

/// Outcome of a store-wide stale temporary-file cleanup.
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed: Vec<String>,
    pub skipped: Vec<String>,
    pub failures: Vec<CleanupFailure>,
}

/// Configuration for a [`AtomicBlobStore`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtomicBlobStoreOptions {
    format: BlobFormatIdentity,
    max_blob_size: u64,
    max_concurrent_operations: NonZeroUsize,
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
    #[error("maximum blob size {maximum} cannot be represented safely on this target")]
    InvalidMaximumBlobSize { maximum: u64 },
}

/// Filesystem operation associated with an ordinary I/O error.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOperation {
    ResolveCurrentDirectory,
    NormalizeRoot,
    StartInitialization,
    StartCoordinator,
    InspectRoot,
    InspectNamespace,
    CreateNamespace,
    OpenRootDirectory,
    SyncRootDirectory,
    OpenBlob,
    ReadEnvelope,
    OpenAtomicWriter,
    WriteEnvelope,
    RemoveBlob,
    OpenNamespaceDirectory,
    SyncNamespaceDirectory,
    InspectBlob,
    QuarantineBlob,
    GenerateIdentifier,
    EnumerateTemporaryFiles,
    RemoveTemporaryFile,
    RefreshClearStagingAge,
}

impl std::fmt::Display for StoreOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ResolveCurrentDirectory => "resolve the current directory",
            Self::NormalizeRoot => "normalize the configured root path",
            Self::StartInitialization => "start store initialization",
            Self::StartCoordinator => "start the blob-store coordinator",
            Self::InspectRoot => "inspect configured root",
            Self::InspectNamespace => "inspect namespace directory",
            Self::CreateNamespace => "create namespace directory",
            Self::OpenRootDirectory => "open configured root directory",
            Self::SyncRootDirectory => "synchronize configured root directory",
            Self::OpenBlob => "open blob",
            Self::ReadEnvelope => "read blob envelope",
            Self::OpenAtomicWriter => "open atomic blob writer",
            Self::WriteEnvelope => "write blob envelope",
            Self::RemoveBlob => "remove blob",
            Self::OpenNamespaceDirectory => "open namespace directory",
            Self::SyncNamespaceDirectory => "synchronize namespace directory",
            Self::InspectBlob => "inspect blob",
            Self::QuarantineBlob => "quarantine blob",
            Self::GenerateIdentifier => "generate a diagnostic identifier",
            Self::EnumerateTemporaryFiles => "enumerate owned temporary files",
            Self::RemoveTemporaryFile => "remove an owned temporary file",
            Self::RefreshClearStagingAge => "refresh clear-staging age",
        })
    }
}

/// Envelope section that ended prematurely.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeSection {
    Magic,
    Version,
    PayloadLength,
    Payload,
    Checksum,
}

impl std::fmt::Display for EnvelopeSection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Magic => "magic",
            Self::Version => "version",
            Self::PayloadLength => "payload length",
            Self::Payload => "payload",
            Self::Checksum => "checksum",
        })
    }
}

/// Protocol-neutral failure from the blob store.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum AtomicBlobStoreError {
    #[error("invalid store configuration: {0}")]
    Configuration(#[from] AtomicBlobStoreConfigError),
    #[error("I/O failure while attempting to {operation}: {source}")]
    Io {
        operation: StoreOperation,
        #[source]
        source: io::Error,
    },
    #[error("atomic blob commit failed: {source}")]
    AtomicCommit {
        #[source]
        source: io::Error,
    },
    #[error("configured root does not exist")]
    RootDoesNotExist,
    #[error("configured root is not a directory")]
    RootIsNotDirectory,
    #[error("namespace path exists but is not a directory")]
    NamespacePathIsNotDirectory,
    #[error("blob envelope belongs to a different domain")]
    InvalidEnvelopeDomain {
        expected: [u8; DOMAIN_TAG_LEN],
        found: [u8; DOMAIN_TAG_LEN],
    },
    #[error("unsupported blob envelope version {found}")]
    UnsupportedEnvelopeVersion { found: u16 },
    #[error("blob envelope ended while reading {section}")]
    TruncatedEnvelope { section: EnvelopeSection },
    #[error("blob payload size {size} exceeds configured maximum {maximum}")]
    BlobTooLarge { size: u64, maximum: u64 },
    #[error("declared blob payload length {declared} cannot be represented safely")]
    InvalidPayloadLength { declared: u64 },
    #[error("blob envelope contains trailing data")]
    TrailingData,
    #[error("blob checksum mismatch: stored {expected:#010x}, calculated {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
    #[error("file-backed blob storage is unsupported on {platform}")]
    UnsupportedPlatform { platform: &'static str },
    #[error("blob-store operation coordination failed")]
    CoordinationFailure,
    #[error("canonical blob to quarantine does not exist")]
    QuarantineSourceMissing,
    #[error("blob quarantine failed: {source}")]
    QuarantineCommit {
        #[source]
        source: io::Error,
    },
    #[error("blob was moved to quarantine, but namespace synchronization failed: {source}")]
    QuarantineNamespaceSync {
        quarantine: QuarantineInfo,
        #[source]
        source: io::Error,
    },
    #[error("stale temporary-file cleanup is unsupported on {platform}")]
    CleanupUnsupported { platform: &'static str },
    #[error("cleanup minimum age must be greater than zero")]
    InvalidCleanupAge,
    #[cfg(any(unix, windows))]
    #[error("failed to obtain a random diagnostic identifier: {source}")]
    IdentifierGeneration {
        #[source]
        source: getrandom::Error,
    },
    #[error("blob path has an unexpected file type")]
    UnexpectedFileType,
    #[error("store-wide maintenance coordination failed")]
    MaintenanceCoordinationFailure,
}

/// A boxed future returned by core store operations.
pub type BlobStoreFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, AtomicBlobStoreError>> + Send + 'static>>;

/// Protocol-neutral file-backed blob store.
///
/// Clones share one FIFO coordinator whose lifetime is independent of any
/// caller-owned Tokio runtime. Submission occurs when an operation method is
/// called, before its returned future is polled.
#[derive(Clone)]
pub struct AtomicBlobStore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AtomicBlobStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AtomicBlobStore")
            .field("namespace", &self.inner.config.namespace)
            .field("format", &self.inner.config.format)
            .field("max_blob_size", &self.inner.config.maximum)
            .field(
                "max_concurrent_operations",
                &self.inner.config.max_concurrent_operations,
            )
            .field("coordination", &"owned")
            .finish_non_exhaustive()
    }
}

struct Inner {
    config: Arc<StoreConfig>,
    #[cfg(any(unix, windows))]
    submissions: mpsc::Sender<CoordinatorEvent>,
    #[cfg(all(test, unix))]
    registry_entries: Arc<std::sync::atomic::AtomicUsize>,
}

struct StoreConfig {
    namespace: PathBuf,
    format: BlobFormatIdentity,
    maximum: u64,
    max_concurrent_operations: usize,
    #[cfg(all(test, unix))]
    hook: Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoreConfig")
            .field("namespace", &self.namespace)
            .field("format", &self.format)
            .field("maximum", &self.maximum)
            .field("max_concurrent_operations", &self.max_concurrent_operations)
            .finish_non_exhaustive()
    }
}

impl AtomicBlobStore {
    /// Constructs a store and creates the namespace directory when necessary.
    ///
    /// The configured root must already exist. Relative roots are resolved
    /// against the current directory during construction, so later changes to
    /// the process current directory do not redirect store operations.
    /// `namespace` must be one normal path component. All filesystem work is
    /// isolated from Tokio worker threads.
    ///
    /// The root and its ancestors are trusted. Construction does not add
    /// cross-process locking or protection against hostile filesystem changes.
    /// On targets other than Unix and Windows this method returns
    /// [`AtomicBlobStoreError::UnsupportedPlatform`].
    ///
    /// # Errors
    ///
    /// Returns a validation, platform, filesystem, or coordination error when
    /// the store cannot be initialized with the requested guarantees.
    pub async fn open(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = validate_namespace(namespace.as_ref())?;
        validate_maximum(options.max_blob_size)?;

        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, namespace, options);
            return Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }

        #[cfg(any(unix, windows))]
        {
            let maximum = options.max_blob_size;
            let format = options.format;
            let max_concurrent_operations = options.max_concurrent_operations.get();
            let config = initialize_in_background(
                root,
                namespace,
                format,
                maximum,
                max_concurrent_operations,
            )
            .await?;
            Self::from_config(config)
        }
    }

    #[cfg(any(unix, windows))]
    fn from_config(config: StoreConfig) -> Result<Self, AtomicBlobStoreError> {
        let config = Arc::new(config);
        let (submissions, receiver) = mpsc::channel();
        #[cfg(all(test, unix))]
        let registry_entries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let coordinator_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::StartCoordinator,
                source,
            })?;
        let scheduler_config = Arc::clone(&config);
        #[cfg(all(test, unix))]
        let scheduler_registry_entries = Arc::clone(&registry_entries);
        std::thread::Builder::new()
            .name("atomic-blob-store-coordinator".into())
            .spawn(move || {
                let blocking_executor = coordinator_runtime.handle().clone();
                run_scheduler(
                    &scheduler_config,
                    &receiver,
                    &blocking_executor,
                    #[cfg(all(test, unix))]
                    &scheduler_registry_entries,
                );
            })
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::StartCoordinator,
                source,
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                submissions,
                #[cfg(all(test, unix))]
                registry_entries,
            }),
        })
    }

    #[cfg(all(test, unix))]
    async fn open_with_test_hook(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
        hook: Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = validate_namespace(namespace.as_ref())?;
        validate_maximum(options.max_blob_size)?;
        let maximum = options.max_blob_size;
        let format = options.format;
        let max_concurrent_operations = options.max_concurrent_operations.get();
        let mut config =
            initialize_in_background(root, namespace, format, maximum, max_concurrent_operations)
                .await?;
        config.hook = Some(hook);
        Self::from_config(config)
    }

    /// Loads the canonical payload associated with `canonical_key`.
    ///
    /// Only a genuinely absent canonical blob returns `Ok(None)`.
    #[must_use]
    pub fn load(&self, canonical_key: &[u8]) -> BlobStoreFuture<Option<Vec<u8>>> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::Load { sender },
                receiver,
            )
        }
    }

    /// Saves an opaque canonical payload for `canonical_key`.
    #[must_use]
    pub fn save(&self, canonical_key: &[u8], payload: Vec<u8>) -> BlobStoreFuture<()> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (canonical_key, payload);
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::Save { payload, sender },
                receiver,
            )
        }
    }

    /// Clears the canonical blob for `canonical_key`.
    #[must_use]
    pub fn clear(&self, canonical_key: &[u8]) -> BlobStoreFuture<()> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::Clear { sender },
                receiver,
            )
        }
    }

    /// Inspects metadata without opening, decoding, or mutating the blob.
    #[must_use]
    pub fn inspect(&self, canonical_key: &[u8]) -> BlobStoreFuture<BlobInspection> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::Inspect { sender },
                receiver,
            )
        }
    }

    /// Atomically moves a canonical blob to a randomized diagnostic name.
    #[must_use]
    pub fn quarantine(&self, canonical_key: &[u8]) -> BlobStoreFuture<QuarantineInfo> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::Quarantine { sender },
                receiver,
            )
        }
    }

    /// Removes stale store-owned Windows staging files behind a store-wide FIFO barrier.
    #[must_use]
    pub fn cleanup_stale_temporary_files(
        &self,
        minimum_age: Duration,
    ) -> BlobStoreFuture<CleanupReport> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = minimum_age;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            if minimum_age.is_zero() {
                return Box::pin(async { Err(AtomicBlobStoreError::InvalidCleanupAge) });
            }
            let (sender, receiver) = oneshot::channel();
            if self
                .inner
                .submissions
                .send(CoordinatorEvent::Maintenance(MaintenanceSubmission {
                    minimum_age: Some(minimum_age),
                    sender,
                    completion_sender: self.inner.submissions.clone(),
                }))
                .is_err()
            {
                return Box::pin(async {
                    Err(AtomicBlobStoreError::MaintenanceCoordinationFailure)
                });
            }
            Box::pin(async move {
                receiver
                    .await
                    .unwrap_or(Err(AtomicBlobStoreError::MaintenanceCoordinationFailure))
            })
        }
    }

    /// Waits until every operation submitted before this call has completed.
    ///
    /// Later submissions are not part of this barrier. Dropping the returned
    /// future discards only its result; the barrier remains ordered.
    #[must_use]
    pub fn flush(&self) -> BlobStoreFuture<()> {
        #[cfg(not(any(unix, windows)))]
        {
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = oneshot::channel();
            if self
                .inner
                .submissions
                .send(CoordinatorEvent::Maintenance(MaintenanceSubmission {
                    minimum_age: None,
                    sender,
                    completion_sender: self.inner.submissions.clone(),
                }))
                .is_err()
            {
                return Box::pin(async {
                    Err(AtomicBlobStoreError::MaintenanceCoordinationFailure)
                });
            }
            Box::pin(async move {
                receiver
                    .await
                    .unwrap_or(Err(AtomicBlobStoreError::MaintenanceCoordinationFailure))
                    .map(|_| ())
            })
        }
    }

    /// Returns a diagnostic path for an opaque key.
    ///
    /// This path is not a stable storage API and callers must not read, write,
    /// rename, or delete it while the store is in use.
    #[must_use]
    pub fn blob_path(&self, canonical_key: &[u8]) -> PathBuf {
        self.inner
            .config
            .namespace
            .join(blob_filename(&self.inner.config.format, canonical_key))
    }
}

#[cfg(any(unix, windows))]
async fn initialize_in_background(
    root: PathBuf,
    namespace: PathBuf,
    format: BlobFormatIdentity,
    maximum: u64,
    max_concurrent_operations: usize,
) -> Result<StoreConfig, AtomicBlobStoreError> {
    let (sender, receiver) = oneshot::channel();
    std::thread::Builder::new()
        .name("atomic-blob-store-initialize".into())
        .spawn(move || {
            let result =
                initialize_platform(root, namespace, format, maximum, max_concurrent_operations);
            let _ = sender.send(result);
        })
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::StartInitialization,
            source,
        })?;
    receiver
        .await
        .unwrap_or(Err(AtomicBlobStoreError::CoordinationFailure))
}

/// Returns the stable full-BLAKE3 blob filename for canonical key bytes.
#[must_use]
pub fn blob_filename(format: &BlobFormatIdentity, canonical_key: &[u8]) -> String {
    format!(
        "{}{}",
        blake3::hash(canonical_key).to_hex(),
        format.filename_suffix
    )
}

#[cfg(any(unix, windows))]
fn key_hash(canonical_key: &[u8]) -> [u8; 32] {
    *blake3::hash(canonical_key).as_bytes()
}

fn validate_namespace(namespace: &OsStr) -> Result<PathBuf, AtomicBlobStoreError> {
    let path = Path::new(namespace);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => {
            Ok(PathBuf::from(component))
        }
        _ => Err(AtomicBlobStoreConfigError::InvalidNamespace.into()),
    }
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

fn validate_maximum(maximum: u64) -> Result<(), AtomicBlobStoreError> {
    let envelope_capacity = usize::try_from(maximum)
        .ok()
        .and_then(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN));
    if envelope_capacity.is_none() {
        return Err(AtomicBlobStoreConfigError::InvalidMaximumBlobSize { maximum }.into());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn unsupported_future<T: Send + 'static>() -> BlobStoreFuture<T> {
    Box::pin(async {
        Err(AtomicBlobStoreError::UnsupportedPlatform {
            platform: std::env::consts::OS,
        })
    })
}

#[cfg(any(unix, windows))]
fn submit<T: Send + 'static>(
    submissions: &mpsc::Sender<CoordinatorEvent>,
    key_hash: [u8; 32],
    operation: Operation,
    receiver: oneshot::Receiver<Result<T, AtomicBlobStoreError>>,
) -> BlobStoreFuture<T> {
    let submission = Submission {
        key_hash,
        operation,
        completion_sender: submissions.clone(),
    };
    if submissions
        .send(CoordinatorEvent::Submission(submission))
        .is_err()
    {
        return Box::pin(async { Err(AtomicBlobStoreError::CoordinationFailure) });
    }
    Box::pin(async move {
        receiver
            .await
            .unwrap_or(Err(AtomicBlobStoreError::CoordinationFailure))
    })
}

#[cfg(any(unix, windows))]
struct Submission {
    key_hash: [u8; 32],
    operation: Operation,
    completion_sender: mpsc::Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
struct QueuedOperation {
    operation: Operation,
    completion_sender: mpsc::Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
enum Operation {
    Load {
        sender: oneshot::Sender<Result<Option<Vec<u8>>, AtomicBlobStoreError>>,
    },
    Save {
        payload: Vec<u8>,
        sender: oneshot::Sender<Result<(), AtomicBlobStoreError>>,
    },
    Clear {
        sender: oneshot::Sender<Result<(), AtomicBlobStoreError>>,
    },
    Inspect {
        sender: oneshot::Sender<Result<BlobInspection, AtomicBlobStoreError>>,
    },
    Quarantine {
        sender: oneshot::Sender<Result<QuarantineInfo, AtomicBlobStoreError>>,
    },
}

#[cfg(any(unix, windows))]
enum BlockingResult {
    Load(Result<Option<Vec<u8>>, AtomicBlobStoreError>),
    Save(Result<(), AtomicBlobStoreError>),
    Clear(Result<(), AtomicBlobStoreError>),
    Inspect(Result<BlobInspection, AtomicBlobStoreError>),
    Quarantine(Result<QuarantineInfo, AtomicBlobStoreError>),
}

#[cfg(any(unix, windows))]
struct Completion {
    key_hash: [u8; 32],
    outcome: Option<(Operation, BlockingResult)>,
}

#[cfg(any(unix, windows))]
enum CoordinatorEvent {
    Submission(Submission),
    Completion(Completion),
    Maintenance(MaintenanceSubmission),
    MaintenanceCompletion(MaintenanceCompletion),
}

#[cfg(any(unix, windows))]
struct MaintenanceSubmission {
    minimum_age: Option<Duration>,
    sender: oneshot::Sender<Result<CleanupReport, AtomicBlobStoreError>>,
    completion_sender: mpsc::Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
struct MaintenanceCompletion {
    outcome: Option<MaintenanceOutcome>,
}

#[cfg(any(unix, windows))]
enum PendingEvent {
    Submission(Submission),
    Maintenance(MaintenanceSubmission),
}

#[cfg(any(unix, windows))]
type MaintenanceOutcome = (
    oneshot::Sender<Result<CleanupReport, AtomicBlobStoreError>>,
    Result<CleanupReport, AtomicBlobStoreError>,
);

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_lines)]
fn run_scheduler(
    config: &Arc<StoreConfig>,
    receiver: &mpsc::Receiver<CoordinatorEvent>,
    blocking_executor: &tokio::runtime::Handle,
    #[cfg(all(test, unix))] registry_entries: &std::sync::atomic::AtomicUsize,
) {
    let mut queues: HashMap<[u8; 32], VecDeque<QueuedOperation>> = HashMap::new();
    let mut active = HashSet::new();
    let mut pending = VecDeque::new();
    let mut maintenance_active = false;

    while let Ok(event) = receiver.recv() {
        match event {
            CoordinatorEvent::Submission(submission) => {
                if maintenance_active || !pending.is_empty() {
                    pending.push_back(PendingEvent::Submission(submission));
                    continue;
                }
                let key_hash = submission.key_hash;
                queues
                    .entry(key_hash)
                    .or_default()
                    .push_back(QueuedOperation {
                        operation: submission.operation,
                        completion_sender: submission.completion_sender,
                    });
                #[cfg(all(test, unix))]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
                dispatch_if_idle(
                    key_hash,
                    config,
                    &mut queues,
                    &mut active,
                    blocking_executor,
                );
                dispatch_available(config, &mut queues, &mut active, blocking_executor);
            }
            CoordinatorEvent::Completion(completion) => {
                let outcome = completion.outcome;
                active.remove(&completion.key_hash);
                dispatch_if_idle(
                    completion.key_hash,
                    config,
                    &mut queues,
                    &mut active,
                    blocking_executor,
                );
                dispatch_available(config, &mut queues, &mut active, blocking_executor);
                if !active.contains(&completion.key_hash)
                    && queues
                        .get(&completion.key_hash)
                        .is_some_and(VecDeque::is_empty)
                {
                    queues.remove(&completion.key_hash);
                }
                #[cfg(all(test, unix))]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
                if let Some((operation, result)) = outcome {
                    deliver(operation, result);
                }
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    blocking_executor,
                );
            }
            CoordinatorEvent::Maintenance(submission) => {
                pending.push_back(PendingEvent::Maintenance(submission));
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    blocking_executor,
                );
            }
            CoordinatorEvent::MaintenanceCompletion(completion) => {
                maintenance_active = false;
                if let Some((sender, result)) = completion.outcome {
                    let _send_result = sender.send(result);
                }
                advance_pending_if_ready(
                    config,
                    &mut queues,
                    &mut active,
                    &mut pending,
                    &mut maintenance_active,
                    blocking_executor,
                );
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn advance_pending_if_ready(
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    pending: &mut VecDeque<PendingEvent>,
    maintenance_active: &mut bool,
    blocking_executor: &tokio::runtime::Handle,
) {
    if *maintenance_active {
        return;
    }

    loop {
        match pending.front() {
            Some(PendingEvent::Maintenance(_)) => {
                if !active.is_empty() || queues.values().any(|queue| !queue.is_empty()) {
                    return;
                }
                let Some(PendingEvent::Maintenance(submission)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                *maintenance_active = true;
                dispatch_maintenance(config, submission, blocking_executor);
                return;
            }
            Some(PendingEvent::Submission(_)) => {
                let Some(PendingEvent::Submission(submission)) = pending.pop_front() else {
                    unreachable!("the pending event kind was inspected above");
                };
                let key_hash = submission.key_hash;
                queues
                    .entry(key_hash)
                    .or_default()
                    .push_back(QueuedOperation {
                        operation: submission.operation,
                        completion_sender: submission.completion_sender,
                    });
                dispatch_if_idle(key_hash, config, queues, active, blocking_executor);
            }
            None => return,
        }
    }
}

#[cfg(any(unix, windows))]
fn dispatch_maintenance(
    config: &Arc<StoreConfig>,
    submission: MaintenanceSubmission,
    blocking_executor: &tokio::runtime::Handle,
) {
    let config = Arc::clone(config);
    blocking_executor.spawn_blocking(move || {
        let MaintenanceSubmission {
            minimum_age,
            sender,
            completion_sender,
        } = submission;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            minimum_age.map_or_else(
                || Ok(CleanupReport::default()),
                |minimum_age| cleanup_stale_files(&config, minimum_age),
            )
        }));
        let outcome = result.ok().map(|result| (sender, result));
        let _send_result = completion_sender.send(CoordinatorEvent::MaintenanceCompletion(
            MaintenanceCompletion { outcome },
        ));
    });
}

#[cfg(any(unix, windows))]
fn dispatch_if_idle(
    key_hash: [u8; 32],
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    blocking_executor: &tokio::runtime::Handle,
) {
    if active.contains(&key_hash) || active.len() >= config.max_concurrent_operations {
        return;
    }

    let Some(next_operation) = queues.get_mut(&key_hash).and_then(VecDeque::pop_front) else {
        return;
    };
    let QueuedOperation {
        operation,
        completion_sender,
    } = next_operation;
    let config = Arc::clone(config);
    active.insert(key_hash);
    blocking_executor.spawn_blocking(move || {
        let path = config.namespace.join(format!(
            "{}{}",
            blake3::Hash::from_bytes(key_hash).to_hex(),
            config.format.filename_suffix()
        ));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_owned_operation(&config, &path, operation)
        }))
        .ok();
        let _send_result = completion_sender.send(CoordinatorEvent::Completion(Completion {
            key_hash,
            outcome,
        }));
    });
}

#[cfg(any(unix, windows))]
fn dispatch_available(
    config: &Arc<StoreConfig>,
    queues: &mut HashMap<[u8; 32], VecDeque<QueuedOperation>>,
    active: &mut HashSet<[u8; 32]>,
    blocking_executor: &tokio::runtime::Handle,
) {
    while active.len() < config.max_concurrent_operations {
        let Some(key_hash) = queues
            .iter()
            .find_map(|(key, queue)| (!queue.is_empty() && !active.contains(key)).then_some(*key))
        else {
            break;
        };
        dispatch_if_idle(key_hash, config, queues, active, blocking_executor);
    }
}

#[cfg(any(unix, windows))]
fn run_owned_operation(
    config: &StoreConfig,
    path: &Path,
    operation: Operation,
) -> (Operation, BlockingResult) {
    match operation {
        Operation::Load { sender } => {
            let result = load_blob(config, path);
            (Operation::Load { sender }, BlockingResult::Load(result))
        }
        Operation::Save { payload, sender } => {
            #[cfg(any(unix, windows))]
            let result = save_blob(config, path, &payload);
            #[cfg(not(any(unix, windows)))]
            let result = Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
            (
                Operation::Save { payload, sender },
                BlockingResult::Save(result),
            )
        }
        Operation::Clear { sender } => {
            let result = clear_blob(config, path);
            (Operation::Clear { sender }, BlockingResult::Clear(result))
        }
        Operation::Inspect { sender } => (
            Operation::Inspect { sender },
            BlockingResult::Inspect(inspect_blob(config, path)),
        ),
        Operation::Quarantine { sender } => (
            Operation::Quarantine { sender },
            BlockingResult::Quarantine(quarantine_blob(config, path)),
        ),
    }
}

#[cfg(any(unix, windows))]
fn deliver(operation: Operation, result: BlockingResult) {
    match (operation, result) {
        (Operation::Load { sender }, BlockingResult::Load(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Save { sender, .. }, BlockingResult::Save(result))
        | (Operation::Clear { sender }, BlockingResult::Clear(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Inspect { sender }, BlockingResult::Inspect(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Quarantine { sender }, BlockingResult::Quarantine(result)) => {
            let _send_result = sender.send(result);
        }
        _ => unreachable!("operation and result kinds must match"),
    }
}

#[cfg(any(unix, windows))]
fn initialize_platform(
    mut root: PathBuf,
    namespace_component: PathBuf,
    format: BlobFormatIdentity,
    maximum: u64,
    max_concurrent_operations: usize,
) -> Result<StoreConfig, AtomicBlobStoreError> {
    #[cfg(windows)]
    {
        root = std::path::absolute(&root).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::NormalizeRoot,
            source,
        })?;
    }

    #[cfg(unix)]
    if root.is_relative() {
        let current_directory =
            std::env::current_dir().map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::ResolveCurrentDirectory,
                source,
            })?;
        root = current_directory.join(root);
    }

    let metadata = match std::fs::metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(AtomicBlobStoreError::RootDoesNotExist);
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::InspectRoot,
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(AtomicBlobStoreError::RootIsNotDirectory);
    }

    let namespace = root.join(namespace_component);
    match std::fs::create_dir(&namespace) {
        Ok(()) => {
            #[cfg(unix)]
            sync_directory(
                &root,
                StoreOperation::OpenRootDirectory,
                StoreOperation::SyncRootDirectory,
            )?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata =
                std::fs::metadata(&namespace).map_err(|source| AtomicBlobStoreError::Io {
                    operation: StoreOperation::InspectNamespace,
                    source,
                })?;
            if !metadata.is_dir() {
                return Err(AtomicBlobStoreError::NamespacePathIsNotDirectory);
            }
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::CreateNamespace,
                source,
            });
        }
    }

    Ok(StoreConfig {
        namespace,
        format,
        maximum,
        max_concurrent_operations,
        #[cfg(all(test, unix))]
        hook: None,
    })
}

#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestStage {
    BeforeEnvelope,
    AfterEnvelope,
    BeforeAtomicOpen,
    DuringWrite,
    BeforeCommit,
    CommitError,
    AfterCommit,
    BeforeRemove,
    AfterRemove,
    BeforeDirectorySync,
    AfterDirectorySync,
    BeforeCleanup,
}

#[cfg(all(test, unix))]
fn hit_test_stage(
    config: &StoreConfig,
    stage: TestStage,
    operation: StoreOperation,
) -> Result<(), AtomicBlobStoreError> {
    let Some(hook) = &config.hook else {
        return Ok(());
    };
    hook(stage).map_err(|source| AtomicBlobStoreError::Io { operation, source })
}

#[cfg(any(unix, windows))]
#[cfg_attr(not(any(test, feature = "bench-instrumentation")), allow(dead_code))]
fn encode_envelope(
    format: &BlobFormatIdentity,
    payload: &[u8],
    maximum: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    let (header, checksum) = envelope_parts(format, payload, maximum)?;
    let mut envelope = Vec::with_capacity(header.len() + payload.len() + checksum.len());
    envelope.extend_from_slice(&header);
    envelope.extend_from_slice(payload);
    envelope.extend_from_slice(&checksum);
    Ok(envelope)
}

#[cfg(any(unix, windows))]
fn envelope_parts(
    format: &BlobFormatIdentity,
    payload: &[u8],
    maximum: u64,
) -> Result<([u8; HEADER_LEN], [u8; CHECKSUM_LEN]), AtomicBlobStoreError> {
    let size = u64::try_from(payload.len())
        .map_err(|_| AtomicBlobStoreError::InvalidPayloadLength { declared: u64::MAX })?;
    if size > maximum {
        return Err(AtomicBlobStoreError::BlobTooLarge { size, maximum });
    }
    payload
        .len()
        .checked_add(HEADER_LEN + CHECKSUM_LEN)
        .ok_or(AtomicBlobStoreError::InvalidPayloadLength { declared: size })?;
    let mut header = [0; HEADER_LEN];
    header[..DOMAIN_TAG_LEN].copy_from_slice(format.domain_tag());
    header[DOMAIN_TAG_LEN..DOMAIN_TAG_LEN + 2]
        .copy_from_slice(&format.envelope_version().to_be_bytes());
    header[DOMAIN_TAG_LEN + 2..].copy_from_slice(&size.to_be_bytes());
    let checksum = crc32c::crc32c_append(crc32c::crc32c(&header), payload).to_be_bytes();
    Ok((header, checksum))
}

#[cfg(any(unix, windows))]
fn load_blob(config: &StoreConfig, path: &Path) -> Result<Option<Vec<u8>>, AtomicBlobStoreError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(None);
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::OpenBlob,
                source,
            });
        }
    };
    decode_reader(&config.format, &mut file, config.maximum).map(Some)
}

#[cfg(any(unix, windows))]
fn decode_reader(
    format: &BlobFormatIdentity,
    reader: &mut impl Read,
    maximum: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    decode_reader_with_usize_limit(format, reader, maximum, usize::MAX as u64)
}

#[cfg(any(unix, windows))]
fn decode_reader_with_usize_limit(
    format: &BlobFormatIdentity,
    reader: &mut impl Read,
    maximum: u64,
    usize_limit: u64,
) -> Result<Vec<u8>, AtomicBlobStoreError> {
    let mut magic = [0; 8];
    read_section(reader, &mut magic, EnvelopeSection::Magic)?;
    if &magic != format.domain_tag() {
        return Err(AtomicBlobStoreError::InvalidEnvelopeDomain {
            expected: *format.domain_tag(),
            found: magic,
        });
    }

    let mut version = [0; 2];
    read_section(reader, &mut version, EnvelopeSection::Version)?;
    let version = u16::from_be_bytes(version);
    if version != format.envelope_version() {
        return Err(AtomicBlobStoreError::UnsupportedEnvelopeVersion { found: version });
    }

    let mut length = [0; 8];
    read_section(reader, &mut length, EnvelopeSection::PayloadLength)?;
    let declared = u64::from_be_bytes(length);
    if declared > maximum {
        return Err(AtomicBlobStoreError::BlobTooLarge {
            size: declared,
            maximum,
        });
    }
    if declared > usize_limit {
        return Err(AtomicBlobStoreError::InvalidPayloadLength { declared });
    }
    let payload_len = usize::try_from(declared)
        .ok()
        .filter(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN).is_some())
        .ok_or(AtomicBlobStoreError::InvalidPayloadLength { declared })?;

    let mut payload = vec![0; payload_len];
    read_section(reader, &mut payload, EnvelopeSection::Payload)?;
    let mut checksum = [0; 4];
    read_section(reader, &mut checksum, EnvelopeSection::Checksum)?;

    let mut trailing = [0; 1];
    match reader.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(AtomicBlobStoreError::TrailingData),
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::ReadEnvelope,
                source,
            });
        }
    }

    let mut actual = crc32c::crc32c(format.domain_tag());
    actual = crc32c::crc32c_append(actual, &format.envelope_version().to_be_bytes());
    actual = crc32c::crc32c_append(actual, &declared.to_be_bytes());
    actual = crc32c::crc32c_append(actual, &payload);
    let expected = u32::from_be_bytes(checksum);
    if expected != actual {
        return Err(AtomicBlobStoreError::ChecksumMismatch { expected, actual });
    }
    Ok(payload)
}

#[cfg(any(unix, windows))]
fn read_section(
    reader: &mut impl Read,
    bytes: &mut [u8],
    section: EnvelopeSection,
) -> Result<(), AtomicBlobStoreError> {
    match reader.read_exact(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(AtomicBlobStoreError::TruncatedEnvelope { section })
        }
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::ReadEnvelope,
            source,
        }),
    }
}

/// Benchmark-only access to the stable production envelope implementation.
///
/// This module is deliberately feature-gated so ordinary consumers do not
/// acquire an additional public surface. Its functions call the same encoder
/// and bounded reader used by [`AtomicBlobStore`].
#[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
#[doc(hidden)]
pub mod bench_instrumentation {
    use std::io::Read;

    use super::{AtomicBlobStoreError, CHECKSUM_LEN, HEADER_LEN, decode_reader, encode_envelope};

    pub const ENVELOPE_OVERHEAD: usize = HEADER_LEN + CHECKSUM_LEN;

    pub fn encode(
        format: &super::BlobFormatIdentity,
        payload: &[u8],
        maximum: u64,
    ) -> Result<Vec<u8>, AtomicBlobStoreError> {
        encode_envelope(format, payload, maximum)
    }

    pub fn decode(
        format: &super::BlobFormatIdentity,
        reader: &mut impl Read,
        maximum: u64,
    ) -> Result<Vec<u8>, AtomicBlobStoreError> {
        decode_reader(format, reader, maximum)
    }
}

#[cfg(any(unix, windows))]
fn ensure_namespace_available(config: &StoreConfig) -> Result<(), AtomicBlobStoreError> {
    match std::fs::metadata(&config.namespace) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(AtomicBlobStoreError::NamespacePathIsNotDirectory),
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::InspectNamespace,
            source,
        }),
    }
}

#[cfg(unix)]
fn save_blob(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(test)]
    let envelope = encode_envelope(&config.format, payload, config.maximum)?;
    #[cfg(not(test))]
    let (header, checksum) = envelope_parts(&config.format, payload, config.maximum)?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeAtomicOpen,
        StoreOperation::OpenAtomicWriter,
    )?;
    let mut options = atomic_write_file::OpenOptions::new();
    StdOpenOptionsExt::mode(&mut options, 0o600);
    AtomicOpenOptionsExt::preserve_mode(&mut options, false);
    AtomicOpenOptionsExt::preserve_owner(&mut options, false);
    let mut writer = options
        .open(path)
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source,
        })?;
    #[cfg(all(test, unix))]
    {
        let midpoint = envelope.len() / 2;
        writer
            .write_all(&envelope[..midpoint])
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
        hit_test_stage(
            config,
            TestStage::DuringWrite,
            StoreOperation::WriteEnvelope,
        )?;
        writer
            .write_all(&envelope[midpoint..])
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(not(test))]
    for section in [&header[..], payload, &checksum[..]] {
        writer
            .write_all(section)
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeCommit,
        StoreOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    if let Err(source) = config
        .hook
        .as_ref()
        .map_or(Ok(()), |hook| hook(TestStage::CommitError))
    {
        return Err(AtomicBlobStoreError::AtomicCommit { source });
    }
    writer
        .commit()
        .map_err(|source| AtomicBlobStoreError::AtomicCommit { source })?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterCommit,
        StoreOperation::WriteEnvelope,
    )?;
    Ok(())
}

#[cfg(unix)]
fn clear_blob(config: &StoreConfig, path: &Path) -> Result<(), AtomicBlobStoreError> {
    #[cfg(all(test, unix))]
    hit_test_stage(config, TestStage::BeforeRemove, StoreOperation::RemoveBlob)?;
    match std::fs::remove_file(path) {
        Ok(()) => {
            #[cfg(all(test, unix))]
            hit_test_stage(config, TestStage::AfterRemove, StoreOperation::RemoveBlob)?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::BeforeDirectorySync,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            sync_directory(
                &config.namespace,
                StoreOperation::OpenNamespaceDirectory,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::AfterDirectorySync,
                StoreOperation::SyncNamespaceDirectory,
            )?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => ensure_namespace_available(config),
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::RemoveBlob,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn inspect_blob(config: &StoreConfig, path: &Path) -> Result<BlobInspection, AtomicBlobStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(BlobInspection {
            state: BlobState::Present,
            size: Some(metadata.len()),
            modified: metadata.modified().ok(),
        }),
        Ok(_) => Err(AtomicBlobStoreError::UnexpectedFileType),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            Ok(BlobInspection {
                state: BlobState::Absent,
                size: None,
                modified: None,
            })
        }
        Err(source) => Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::InspectBlob,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn random_identifier() -> Result<String, AtomicBlobStoreError> {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|source| AtomicBlobStoreError::IdentifierGeneration { source })?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(result)
}

#[cfg(unix)]
fn quarantine_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, AtomicBlobStoreError> {
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config.namespace.join(format!(
            "{hash}{}.quarantine-v1.{identifier}",
            config.format.filename_suffix()
        ));
        match rustix::fs::renameat_with(
            rustix::fs::CWD,
            path,
            rustix::fs::CWD,
            &destination,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                let quarantine = QuarantineInfo {
                    identifier,
                    diagnostic_path: destination,
                };
                sync_quarantine_namespace(config, &quarantine)?;
                return Ok(quarantine);
            }
            Err(error) if error == rustix::io::Errno::NOENT => {
                ensure_namespace_available(config)?;
                return Err(AtomicBlobStoreError::QuarantineSourceMissing);
            }
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => {
                return Err(AtomicBlobStoreError::QuarantineCommit {
                    source: io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        }
    }
}

#[cfg(unix)]
fn sync_quarantine_namespace(
    config: &StoreConfig,
    quarantine: &QuarantineInfo,
) -> Result<(), AtomicBlobStoreError> {
    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::BeforeDirectorySync).map_err(|source| {
            AtomicBlobStoreError::QuarantineNamespaceSync {
                quarantine: quarantine.clone(),
                source,
            }
        })?;
    }

    let directory = std::fs::File::open(&config.namespace).map_err(|source| {
        AtomicBlobStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        }
    })?;
    directory
        .sync_all()
        .map_err(|source| AtomicBlobStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        })?;

    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::AfterDirectorySync).map_err(|source| {
            AtomicBlobStoreError::QuarantineNamespaceSync {
                quarantine: quarantine.clone(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(unix)]
#[allow(clippy::missing_const_for_fn)]
fn cleanup_stale_files(
    config: &StoreConfig,
    _minimum_age: Duration,
) -> Result<CleanupReport, AtomicBlobStoreError> {
    #[cfg(not(test))]
    let _ = config;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeCleanup,
        StoreOperation::EnumerateTemporaryFiles,
    )?;
    Err(AtomicBlobStoreError::CleanupUnsupported {
        platform: std::env::consts::OS,
    })
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    const SEPARATOR: u16 = b'\\' as u16;
    const ALTERNATE_SEPARATOR: u16 = b'/' as u16;
    const VERBATIM_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const DEVICE_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'.' as u16, SEPARATOR];
    const UNC_PREFIX: &[u16] = &[
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    let path: Vec<u16> = path.as_os_str().encode_wide().collect();
    let (prefix, remainder) =
        if path.starts_with(VERBATIM_PREFIX) || path.starts_with(DEVICE_PREFIX) {
            (&[][..], path.as_slice())
        } else if path.starts_with(&[SEPARATOR, SEPARATOR]) {
            (UNC_PREFIX, &path[2..])
        } else {
            (VERBATIM_PREFIX, path.as_slice())
        };

    let mut extended = Vec::with_capacity(prefix.len() + remainder.len() + 1);
    extended.extend_from_slice(prefix);
    extended.extend(remainder.iter().map(|unit| {
        if *unit == ALTERNATE_SEPARATOR {
            SEPARATOR
        } else {
            *unit
        }
    }));
    extended.push(0);
    extended
}

#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, flags: u32) -> Result<(), io::Error> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    // SAFETY: both pointers reference NUL-terminated wide strings for the call.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            flags,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn delete_file(path: &Path) -> Result<(), io::Error> {
    let path = wide_path(path);
    // SAFETY: the pointer references a NUL-terminated wide string for the call.
    if unsafe { windows_sys::Win32::Storage::FileSystem::DeleteFileW(path.as_ptr()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn create_windows_staging(
    path: &Path,
    header: &[u8],
    payload: &[u8],
    checksum: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FlushFileBuffers,
    };

    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated; null security attributes deliberately inherit the directory ACL.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::OpenAtomicWriter,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the successful CreateFileW call returned an owned handle.
    let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
    for section in [header, payload, checksum] {
        file.write_all(section)
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::WriteEnvelope,
                source,
            })?;
    }
    // SAFETY: the file still owns a live handle.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(AtomicBlobStoreError::Io {
            operation: StoreOperation::WriteEnvelope,
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn refresh_windows_clear_age(path: &Path) -> Result<(), io::Error> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{FILETIME, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH, FILE_WRITE_ATTRIBUTES,
        FlushFileBuffers, OPEN_EXISTING, SetFileTime,
    };

    const WINDOWS_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;
    const HUNDRED_NS_PER_SECOND: u64 = 10_000_000;

    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let ticks = elapsed
        .as_secs()
        .checked_mul(HUNDRED_NS_PER_SECOND)
        .and_then(|ticks| ticks.checked_add(u64::from(elapsed.subsec_nanos() / 100)))
        .and_then(|ticks| ticks.checked_add(WINDOWS_EPOCH_OFFSET_100NS))
        .ok_or_else(|| {
            io::Error::other("current time cannot be represented as a Windows file time")
        })?;
    let [b0, b1, b2, b3, b4, b5, b6, b7] = ticks.to_le_bytes();
    let last_write = FILETIME {
        dwLowDateTime: u32::from_le_bytes([b0, b1, b2, b3]),
        dwHighDateTime: u32::from_le_bytes([b4, b5, b6, b7]),
    };

    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated and the returned handle is checked below.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE | FILE_WRITE_ATTRIBUTES,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful CreateFileW call returned an owned handle. Keeping
    // it in a File ensures it is closed before the subsequent rename.
    let _file = unsafe { std::fs::File::from_raw_handle(handle) };
    // SAFETY: `handle` is live and `last_write` remains valid for the call.
    if unsafe {
        SetFileTime(
            handle,
            std::ptr::null(),
            std::ptr::null(),
            &raw const last_write,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is still live and was opened with write access.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn save_blob(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), AtomicBlobStoreError> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let (header, checksum) = envelope_parts(&config.format, payload, config.maximum)?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let staging = config.namespace.join(format!(
            "{hash}{}.tmp-v1.save.{identifier}",
            config.format.filename_suffix()
        ));
        match create_windows_staging(&staging, &header, payload, &checksum) {
            Ok(()) => {}
            Err(AtomicBlobStoreError::Io { source, .. })
                if source.kind() == io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
        let initial = move_file(&staging, path, MOVEFILE_WRITE_THROUGH);
        match initial {
            Ok(()) => return Ok(()),
            Err(error) if matches!(error.raw_os_error(), Some(code) if code.cast_unsigned() == ERROR_FILE_EXISTS || code.cast_unsigned() == ERROR_ALREADY_EXISTS) =>
            {
                return move_file(
                    &staging,
                    path,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
                .map_err(|source| AtomicBlobStoreError::AtomicCommit { source });
            }
            Err(source) => return Err(AtomicBlobStoreError::AtomicCommit { source }),
        }
    }
}

#[cfg(windows)]
fn clear_blob(config: &StoreConfig, path: &Path) -> Result<(), AtomicBlobStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    match refresh_windows_clear_age(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(());
        }
        Err(source) => {
            return Err(AtomicBlobStoreError::Io {
                operation: StoreOperation::RefreshClearStagingAge,
                source,
            });
        }
    }
    loop {
        let identifier = random_identifier()?;
        let staging = config.namespace.join(format!(
            "{hash}{}.tmp-v1.clear.{identifier}",
            config.format.filename_suffix()
        ));
        match move_file(path, &staging, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                return delete_file(&staging).map_err(|source| AtomicBlobStoreError::Io {
                    operation: StoreOperation::RemoveTemporaryFile,
                    source,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(AtomicBlobStoreError::Io {
                    operation: StoreOperation::RemoveBlob,
                    source,
                });
            }
        }
    }
}

#[cfg(windows)]
fn quarantine_blob(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, AtomicBlobStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(AtomicBlobStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config.namespace.join(format!(
            "{hash}{}.quarantine-v1.{identifier}",
            config.format.filename_suffix()
        ));
        match move_file(path, &destination, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                return Ok(QuarantineInfo {
                    identifier,
                    diagnostic_path: destination,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Err(AtomicBlobStoreError::QuarantineSourceMissing);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(AtomicBlobStoreError::QuarantineCommit { source }),
        }
    }
}

#[cfg(windows)]
fn cleanup_stale_files(
    config: &StoreConfig,
    minimum_age: Duration,
) -> Result<CleanupReport, AtomicBlobStoreError> {
    let now = SystemTime::now();
    let mut report = CleanupReport::default();
    let entries =
        std::fs::read_dir(&config.namespace).map_err(|source| AtomicBlobStoreError::Io {
            operation: StoreOperation::EnumerateTemporaryFiles,
            source,
        })?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                report.failures.push(CleanupFailure {
                    identifier: "<directory-entry>".into(),
                    source,
                });
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_owned_temporary_filename(&name, config.format.filename_suffix()) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                report.failures.push(CleanupFailure {
                    identifier: name,
                    source: io::Error::other("owned staging name is not a regular file"),
                });
                continue;
            }
            Err(source) => {
                report.failures.push(CleanupFailure {
                    identifier: name,
                    source,
                });
                continue;
            }
        };
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age >= minimum_age);
        if !old_enough {
            report.skipped.push(name);
            continue;
        }
        match delete_file(&entry.path()) {
            Ok(()) => report.removed.push(name),
            Err(source) => report.failures.push(CleanupFailure {
                identifier: name,
                source,
            }),
        }
    }
    Ok(report)
}

#[cfg(windows)]
fn is_owned_temporary_filename(name: &str, suffix: &str) -> bool {
    let marker = format!("{suffix}.tmp-v1.");
    let Some((hash, rest)) = name.split_once(&marker) else {
        return false;
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return false;
    }
    let Some((kind, identifier)) = rest.split_once('.') else {
        return false;
    };
    matches!(kind, "save" | "clear")
        && identifier.len() == 64
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(unix)]
fn sync_directory(
    path: &Path,
    open_operation: StoreOperation,
    sync_operation: StoreOperation,
) -> Result<(), AtomicBlobStoreError> {
    let directory = std::fs::File::open(path).map_err(|source| AtomicBlobStoreError::Io {
        operation: open_operation,
        source,
    })?;
    directory
        .sync_all()
        .map_err(|source| AtomicBlobStoreError::Io {
            operation: sync_operation,
            source,
        })
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
