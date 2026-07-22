//! Protocol-neutral, file-backed checkpoint storage for `rumqttc-next`.
//!
//! The store accepts opaque key and payload bytes. MQTT key encoding and session
//! decoding belong to the protocol adapters. See the crate README for the
//! on-disk format, interruption guarantees, and trust boundary.
//!
//! # Platform and filesystem scope
//!
//! This implementation supports Unix and Windows. Other targets compile, but
//! [`FileStore::open`] returns [`FileStoreError::UnsupportedPlatform`]. It is
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
//! application. Hash-derived filenames prevent canonical session key bytes from
//! directly constructing paths outside that root. The store does not defend
//! against another process or attacker that can modify the root, symlink or
//! directory replacement, Windows reparse points, or concurrent checkpoint
//! manipulation.
//!
//! Coordination is process-local and shared by clones of one store. It provides
//! same-key FIFO execution and cancellation-safe completion, but no
//! cross-process locking, distributed locking, leases, fencing,
//! compare-and-swap, or multi-writer coordination. Applications must ensure
//! only one process and one active session owner writes a key.
//!
//! # Data and recovery limitations
//!
//! CRC32C detects accidental corruption only. The store provides no encryption,
//! authentication, cryptographic integrity, or tamper resistance. Corruption
//! fails closed and is left untouched.
//!
//! Only the canonical `.session` path is authoritative. Exact legacy detection
//! never scans or migrates. Windows cleanup recognizes only store-owned staging
//! names; Unix never parses dependency-private temporary names.

#[cfg(any(unix, windows))]
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::future::Future;
use std::io;
#[cfg(any(unix, windows))]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

#[cfg(any(unix, windows))]
use std::sync::mpsc;

#[cfg(any(unix, windows))]
use tokio::sync::oneshot;

#[cfg(any(unix, windows))]
const MAGIC: &[u8; 8] = b"RUMQSESS";
#[cfg(any(unix, windows))]
const ENVELOPE_VERSION: u16 = 1;
const HEADER_LEN: usize = 18;
const CHECKSUM_LEN: usize = 4;

/// The default maximum canonical checkpoint payload size (64 MiB).
pub const DEFAULT_MAX_CHECKPOINT_SIZE: u64 = 64 * 1024 * 1024;

/// Observable state of a canonical checkpoint (or an exact legacy candidate).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointState {
    Absent,
    Present,
    LegacyDetected,
}

/// Metadata returned without opening or decoding checkpoint contents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointInspection {
    pub state: CheckpointState,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

/// Result of an exact canonical load plus legacy-filename check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyLoad {
    Absent,
    Present(Vec<u8>),
    LegacyDetected { diagnostic_path: PathBuf },
}

/// Location assigned to a quarantined checkpoint.
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

/// Configuration for a [`FileStore`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStoreOptions {
    /// Maximum canonical session payload size accepted by save and load.
    pub max_checkpoint_size: u64,
}

impl Default for FileStoreOptions {
    fn default() -> Self {
        Self {
            max_checkpoint_size: DEFAULT_MAX_CHECKPOINT_SIZE,
        }
    }
}

/// Filesystem operation associated with an ordinary I/O error.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileOperation {
    ResolveCurrentDirectory,
    NormalizeRoot,
    StartCoordinator,
    InspectRoot,
    InspectNamespace,
    CreateNamespace,
    OpenRootDirectory,
    SyncRootDirectory,
    OpenCheckpoint,
    ReadEnvelope,
    OpenAtomicWriter,
    WriteEnvelope,
    RemoveCheckpoint,
    OpenNamespaceDirectory,
    SyncNamespaceDirectory,
    InspectCheckpoint,
    InspectLegacyCheckpoint,
    QuarantineCheckpoint,
    GenerateIdentifier,
    EnumerateTemporaryFiles,
    RemoveTemporaryFile,
    RefreshClearStagingAge,
}

impl std::fmt::Display for FileOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ResolveCurrentDirectory => "resolve the current directory",
            Self::NormalizeRoot => "normalize the configured root path",
            Self::StartCoordinator => "start the session-store coordinator",
            Self::InspectRoot => "inspect configured root",
            Self::InspectNamespace => "inspect namespace directory",
            Self::CreateNamespace => "create namespace directory",
            Self::OpenRootDirectory => "open configured root directory",
            Self::SyncRootDirectory => "synchronize configured root directory",
            Self::OpenCheckpoint => "open checkpoint",
            Self::ReadEnvelope => "read checkpoint envelope",
            Self::OpenAtomicWriter => "open atomic checkpoint writer",
            Self::WriteEnvelope => "write checkpoint envelope",
            Self::RemoveCheckpoint => "remove checkpoint",
            Self::OpenNamespaceDirectory => "open namespace directory",
            Self::SyncNamespaceDirectory => "synchronize namespace directory",
            Self::InspectCheckpoint => "inspect checkpoint",
            Self::InspectLegacyCheckpoint => "inspect exact legacy checkpoint",
            Self::QuarantineCheckpoint => "quarantine checkpoint",
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

/// Protocol-neutral failure from the file store.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum FileStoreError {
    #[error("I/O failure while attempting to {operation}: {source}")]
    Io {
        operation: FileOperation,
        #[source]
        source: io::Error,
    },
    #[error("atomic checkpoint commit failed: {source}")]
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
    #[error("namespace must be one non-empty normal path component")]
    InvalidNamespace,
    #[error("maximum checkpoint size {maximum} cannot be represented safely on this target")]
    InvalidMaximumCheckpointSize { maximum: u64 },
    #[error("invalid checkpoint envelope magic")]
    InvalidEnvelopeMagic,
    #[error("unsupported checkpoint envelope version {found}")]
    UnsupportedEnvelopeVersion { found: u16 },
    #[error("checkpoint envelope ended while reading {section}")]
    TruncatedEnvelope { section: EnvelopeSection },
    #[error("checkpoint payload size {size} exceeds configured maximum {maximum}")]
    CheckpointTooLarge { size: u64, maximum: u64 },
    #[error("declared checkpoint payload length {declared} cannot be represented safely")]
    InvalidPayloadLength { declared: u64 },
    #[error("checkpoint envelope contains trailing data")]
    TrailingData,
    #[error("checkpoint checksum mismatch: stored {expected:#010x}, calculated {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },
    #[error("file-backed session storage is unsupported on {platform}")]
    UnsupportedPlatform { platform: &'static str },
    #[error("session-store operation coordination failed")]
    CoordinationFailure,
    #[error("canonical checkpoint to quarantine does not exist")]
    QuarantineSourceMissing,
    #[error("checkpoint quarantine failed: {source}")]
    QuarantineCommit {
        #[source]
        source: io::Error,
    },
    #[error("checkpoint was moved to quarantine, but namespace synchronization failed: {source}")]
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
    #[error("checkpoint path has an unexpected file type")]
    UnexpectedFileType,
    #[error("store-wide maintenance coordination failed")]
    MaintenanceCoordinationFailure,
    #[error("legacy filename must be one non-empty normal path component")]
    InvalidLegacyFilename,
}

/// A boxed future returned by core store operations.
pub type FileStoreFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, FileStoreError>> + Send + 'static>>;

/// Protocol-neutral file-backed checkpoint store.
///
/// Clones share one FIFO coordinator whose lifetime is independent of any
/// caller-owned Tokio runtime. Submission occurs when an operation method is
/// called, before its returned future is polled.
#[derive(Clone)]
pub struct FileStore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FileStore")
            .field("root", &self.inner.config.root)
            .field("namespace", &self.inner.config.namespace)
            .field("max_checkpoint_size", &self.inner.config.maximum)
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
    root: PathBuf,
    namespace: PathBuf,
    maximum: u64,
    #[cfg(all(test, unix))]
    hook: Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoreConfig")
            .field("root", &self.root)
            .field("namespace", &self.namespace)
            .field("maximum", &self.maximum)
            .finish_non_exhaustive()
    }
}

impl FileStore {
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
    /// [`FileStoreError::UnsupportedPlatform`].
    ///
    /// # Errors
    ///
    /// Returns a validation, platform, filesystem, or coordination error when
    /// the store cannot be initialized with the requested guarantees.
    pub async fn open(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: FileStoreOptions,
    ) -> Result<Self, FileStoreError> {
        let root = root.into();
        let namespace = validate_namespace(namespace.as_ref())?;
        validate_maximum(options.max_checkpoint_size)?;

        #[cfg(not(any(unix, windows)))]
        {
            let _ = (root, namespace, options);
            return Err(FileStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }

        #[cfg(any(unix, windows))]
        {
            let maximum = options.max_checkpoint_size;
            let config =
                tokio::task::spawn_blocking(move || initialize_platform(root, namespace, maximum))
                    .await
                    .map_err(|_| FileStoreError::CoordinationFailure)??;
            Self::from_config(config)
        }
    }

    #[cfg(any(unix, windows))]
    fn from_config(config: StoreConfig) -> Result<Self, FileStoreError> {
        let config = Arc::new(config);
        let (submissions, receiver) = mpsc::channel();
        #[cfg(all(test, unix))]
        let registry_entries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let coordinator_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::StartCoordinator,
                source,
            })?;
        let scheduler_config = Arc::clone(&config);
        #[cfg(all(test, unix))]
        let scheduler_registry_entries = Arc::clone(&registry_entries);
        std::thread::Builder::new()
            .name("rumqttc-file-store-coordinator".into())
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
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::StartCoordinator,
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
        options: FileStoreOptions,
        hook: Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>,
    ) -> Result<Self, FileStoreError> {
        let root = root.into();
        let namespace = validate_namespace(namespace.as_ref())?;
        validate_maximum(options.max_checkpoint_size)?;
        let maximum = options.max_checkpoint_size;
        let mut config =
            tokio::task::spawn_blocking(move || initialize_platform(root, namespace, maximum))
                .await
                .map_err(|_| FileStoreError::CoordinationFailure)??;
        config.hook = Some(hook);
        Self::from_config(config)
    }

    /// Loads the canonical payload associated with `canonical_key`.
    ///
    /// Only a genuinely absent canonical checkpoint returns `Ok(None)`.
    #[must_use]
    pub fn load(&self, canonical_key: &[u8]) -> FileStoreFuture<Option<Vec<u8>>> {
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
    pub fn save(&self, canonical_key: &[u8], payload: Vec<u8>) -> FileStoreFuture<()> {
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

    /// Clears the canonical checkpoint for `canonical_key`.
    #[must_use]
    pub fn clear(&self, canonical_key: &[u8]) -> FileStoreFuture<()> {
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

    /// Inspects metadata without opening, decoding, or mutating the checkpoint.
    #[must_use]
    pub fn inspect(&self, canonical_key: &[u8]) -> FileStoreFuture<CheckpointInspection> {
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

    /// Loads the canonical checkpoint, checking exactly one legacy filename only
    /// when the canonical checkpoint is absent.
    #[must_use]
    pub fn load_with_legacy_filename(
        &self,
        canonical_key: &[u8],
        legacy_filename: impl AsRef<OsStr>,
    ) -> FileStoreFuture<LegacyLoad> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (canonical_key, legacy_filename);
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let legacy_filename = match validate_filename(legacy_filename.as_ref()) {
                Ok(filename) => filename,
                Err(error) => return Box::pin(async move { Err(error) }),
            };
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::LoadWithLegacy {
                    legacy_filename,
                    sender,
                },
                receiver,
            )
        }
    }

    /// Inspects canonical state and then exactly one legacy filename in one FIFO operation.
    #[must_use]
    pub fn inspect_with_legacy_filename(
        &self,
        canonical_key: &[u8],
        legacy_filename: impl AsRef<OsStr>,
    ) -> FileStoreFuture<CheckpointInspection> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (canonical_key, legacy_filename);
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let legacy_filename = match validate_filename(legacy_filename.as_ref()) {
                Ok(filename) => filename,
                Err(error) => return Box::pin(async move { Err(error) }),
            };
            let (sender, receiver) = oneshot::channel();
            submit(
                &self.inner.submissions,
                key_hash(canonical_key),
                Operation::InspectWithLegacy {
                    legacy_filename,
                    sender,
                },
                receiver,
            )
        }
    }

    /// Atomically moves a canonical checkpoint to a randomized diagnostic name.
    #[must_use]
    pub fn quarantine(&self, canonical_key: &[u8]) -> FileStoreFuture<QuarantineInfo> {
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
    pub fn cleanup_stale_temporary_files_before_use(
        &self,
        minimum_age: Duration,
    ) -> FileStoreFuture<CleanupReport> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = minimum_age;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            if minimum_age.is_zero() {
                return Box::pin(async { Err(FileStoreError::InvalidCleanupAge) });
            }
            let (sender, receiver) = oneshot::channel();
            if self
                .inner
                .submissions
                .send(CoordinatorEvent::Maintenance(MaintenanceSubmission {
                    minimum_age,
                    sender,
                    completion_sender: self.inner.submissions.clone(),
                }))
                .is_err()
            {
                return Box::pin(async { Err(FileStoreError::MaintenanceCoordinationFailure) });
            }
            Box::pin(async move {
                receiver
                    .await
                    .unwrap_or(Err(FileStoreError::MaintenanceCoordinationFailure))
            })
        }
    }

    /// Returns a diagnostic path for an opaque key.
    ///
    /// This path is not a stable storage API and callers must not read, write,
    /// rename, or delete it while the store is in use.
    #[must_use]
    pub fn checkpoint_path(&self, canonical_key: &[u8]) -> PathBuf {
        self.inner
            .config
            .namespace
            .join(checkpoint_filename(canonical_key))
    }
}

/// Returns the stable full-BLAKE3 checkpoint filename for canonical key bytes.
#[must_use]
pub fn checkpoint_filename(canonical_key: &[u8]) -> String {
    format!("{}.session", blake3::hash(canonical_key).to_hex())
}

#[cfg(any(unix, windows))]
fn key_hash(canonical_key: &[u8]) -> [u8; 32] {
    *blake3::hash(canonical_key).as_bytes()
}

fn validate_namespace(namespace: &OsStr) -> Result<PathBuf, FileStoreError> {
    let path = Path::new(namespace);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => {
            Ok(PathBuf::from(component))
        }
        _ => Err(FileStoreError::InvalidNamespace),
    }
}

#[cfg(any(unix, windows))]
fn validate_filename(filename: &OsStr) -> Result<PathBuf, FileStoreError> {
    let path = Path::new(filename);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => Ok(path.into()),
        _ => Err(FileStoreError::InvalidLegacyFilename),
    }
}

fn validate_maximum(maximum: u64) -> Result<(), FileStoreError> {
    let envelope_capacity = usize::try_from(maximum)
        .ok()
        .and_then(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN));
    if envelope_capacity.is_none() {
        return Err(FileStoreError::InvalidMaximumCheckpointSize { maximum });
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn unsupported_future<T: Send + 'static>() -> FileStoreFuture<T> {
    Box::pin(async {
        Err(FileStoreError::UnsupportedPlatform {
            platform: std::env::consts::OS,
        })
    })
}

#[cfg(any(unix, windows))]
fn submit<T: Send + 'static>(
    submissions: &mpsc::Sender<CoordinatorEvent>,
    key_hash: [u8; 32],
    operation: Operation,
    receiver: oneshot::Receiver<Result<T, FileStoreError>>,
) -> FileStoreFuture<T> {
    let submission = Submission {
        key_hash,
        operation,
        completion_sender: submissions.clone(),
    };
    if submissions
        .send(CoordinatorEvent::Submission(submission))
        .is_err()
    {
        return Box::pin(async { Err(FileStoreError::CoordinationFailure) });
    }
    Box::pin(async move {
        receiver
            .await
            .unwrap_or(Err(FileStoreError::CoordinationFailure))
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
        sender: oneshot::Sender<Result<Option<Vec<u8>>, FileStoreError>>,
    },
    Save {
        payload: Vec<u8>,
        sender: oneshot::Sender<Result<(), FileStoreError>>,
    },
    Clear {
        sender: oneshot::Sender<Result<(), FileStoreError>>,
    },
    Inspect {
        sender: oneshot::Sender<Result<CheckpointInspection, FileStoreError>>,
    },
    LoadWithLegacy {
        legacy_filename: PathBuf,
        sender: oneshot::Sender<Result<LegacyLoad, FileStoreError>>,
    },
    InspectWithLegacy {
        legacy_filename: PathBuf,
        sender: oneshot::Sender<Result<CheckpointInspection, FileStoreError>>,
    },
    Quarantine {
        sender: oneshot::Sender<Result<QuarantineInfo, FileStoreError>>,
    },
}

#[cfg(any(unix, windows))]
enum BlockingResult {
    Load(Result<Option<Vec<u8>>, FileStoreError>),
    Save(Result<(), FileStoreError>),
    Clear(Result<(), FileStoreError>),
    Inspect(Result<CheckpointInspection, FileStoreError>),
    LoadWithLegacy(Result<LegacyLoad, FileStoreError>),
    InspectWithLegacy(Result<CheckpointInspection, FileStoreError>),
    Quarantine(Result<QuarantineInfo, FileStoreError>),
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
    minimum_age: Duration,
    sender: oneshot::Sender<Result<CleanupReport, FileStoreError>>,
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
    oneshot::Sender<Result<CleanupReport, FileStoreError>>,
    Result<CleanupReport, FileStoreError>,
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
            cleanup_stale_files(&config, minimum_age)
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
    if active.contains(&key_hash) {
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
            "{}.session",
            blake3::Hash::from_bytes(key_hash).to_hex()
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
fn run_owned_operation(
    config: &StoreConfig,
    path: &Path,
    operation: Operation,
) -> (Operation, BlockingResult) {
    match operation {
        Operation::Load { sender } => {
            let result = load_checkpoint(config, path);
            (Operation::Load { sender }, BlockingResult::Load(result))
        }
        Operation::Save { payload, sender } => {
            #[cfg(any(unix, windows))]
            let result = save_checkpoint(config, path, &payload);
            #[cfg(not(any(unix, windows)))]
            let result = Err(FileStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
            (
                Operation::Save { payload, sender },
                BlockingResult::Save(result),
            )
        }
        Operation::Clear { sender } => {
            let result = clear_checkpoint(config, path);
            (Operation::Clear { sender }, BlockingResult::Clear(result))
        }
        Operation::Inspect { sender } => (
            Operation::Inspect { sender },
            BlockingResult::Inspect(inspect_checkpoint(config, path)),
        ),
        Operation::LoadWithLegacy {
            legacy_filename,
            sender,
        } => {
            let result = load_with_legacy(config, path, &legacy_filename);
            (
                Operation::LoadWithLegacy {
                    legacy_filename,
                    sender,
                },
                BlockingResult::LoadWithLegacy(result),
            )
        }
        Operation::InspectWithLegacy {
            legacy_filename,
            sender,
        } => {
            let result = inspect_with_legacy(config, path, &legacy_filename);
            (
                Operation::InspectWithLegacy {
                    legacy_filename,
                    sender,
                },
                BlockingResult::InspectWithLegacy(result),
            )
        }
        Operation::Quarantine { sender } => (
            Operation::Quarantine { sender },
            BlockingResult::Quarantine(quarantine_checkpoint(config, path)),
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
        (Operation::Inspect { sender }, BlockingResult::Inspect(result))
        | (
            Operation::InspectWithLegacy { sender, .. },
            BlockingResult::InspectWithLegacy(result),
        ) => {
            let _send_result = sender.send(result);
        }
        (Operation::LoadWithLegacy { sender, .. }, BlockingResult::LoadWithLegacy(result)) => {
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
    maximum: u64,
) -> Result<StoreConfig, FileStoreError> {
    #[cfg(windows)]
    {
        root = std::path::absolute(&root).map_err(|source| FileStoreError::Io {
            operation: FileOperation::NormalizeRoot,
            source,
        })?;
    }

    #[cfg(unix)]
    if root.is_relative() {
        let current_directory = std::env::current_dir().map_err(|source| FileStoreError::Io {
            operation: FileOperation::ResolveCurrentDirectory,
            source,
        })?;
        root = current_directory.join(root);
    }

    let metadata = match std::fs::metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(FileStoreError::RootDoesNotExist);
        }
        Err(source) => {
            return Err(FileStoreError::Io {
                operation: FileOperation::InspectRoot,
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(FileStoreError::RootIsNotDirectory);
    }

    let namespace = root.join(namespace_component);
    match std::fs::create_dir(&namespace) {
        Ok(()) => {
            #[cfg(unix)]
            sync_directory(
                &root,
                FileOperation::OpenRootDirectory,
                FileOperation::SyncRootDirectory,
            )?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::metadata(&namespace).map_err(|source| FileStoreError::Io {
                operation: FileOperation::InspectNamespace,
                source,
            })?;
            if !metadata.is_dir() {
                return Err(FileStoreError::NamespacePathIsNotDirectory);
            }
        }
        Err(source) => {
            return Err(FileStoreError::Io {
                operation: FileOperation::CreateNamespace,
                source,
            });
        }
    }

    Ok(StoreConfig {
        root,
        namespace,
        maximum,
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
    operation: FileOperation,
) -> Result<(), FileStoreError> {
    let Some(hook) = &config.hook else {
        return Ok(());
    };
    hook(stage).map_err(|source| FileStoreError::Io { operation, source })
}

#[cfg(any(unix, windows))]
#[cfg_attr(not(any(test, feature = "bench-instrumentation")), allow(dead_code))]
fn encode_envelope(payload: &[u8], maximum: u64) -> Result<Vec<u8>, FileStoreError> {
    let (header, checksum) = envelope_parts(payload, maximum)?;
    let mut envelope = Vec::with_capacity(header.len() + payload.len() + checksum.len());
    envelope.extend_from_slice(&header);
    envelope.extend_from_slice(payload);
    envelope.extend_from_slice(&checksum);
    Ok(envelope)
}

#[cfg(any(unix, windows))]
fn envelope_parts(
    payload: &[u8],
    maximum: u64,
) -> Result<([u8; HEADER_LEN], [u8; CHECKSUM_LEN]), FileStoreError> {
    let size = u64::try_from(payload.len())
        .map_err(|_| FileStoreError::InvalidPayloadLength { declared: u64::MAX })?;
    if size > maximum {
        return Err(FileStoreError::CheckpointTooLarge { size, maximum });
    }
    payload
        .len()
        .checked_add(HEADER_LEN + CHECKSUM_LEN)
        .ok_or(FileStoreError::InvalidPayloadLength { declared: size })?;
    let mut header = [0; HEADER_LEN];
    header[..MAGIC.len()].copy_from_slice(MAGIC);
    header[MAGIC.len()..MAGIC.len() + 2].copy_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    header[MAGIC.len() + 2..].copy_from_slice(&size.to_be_bytes());
    let checksum = crc32c::crc32c_append(crc32c::crc32c(&header), payload).to_be_bytes();
    Ok((header, checksum))
}

#[cfg(any(unix, windows))]
fn load_checkpoint(config: &StoreConfig, path: &Path) -> Result<Option<Vec<u8>>, FileStoreError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(None);
        }
        Err(source) => {
            return Err(FileStoreError::Io {
                operation: FileOperation::OpenCheckpoint,
                source,
            });
        }
    };
    decode_reader(&mut file, config.maximum).map(Some)
}

#[cfg(any(unix, windows))]
fn decode_reader(reader: &mut impl Read, maximum: u64) -> Result<Vec<u8>, FileStoreError> {
    decode_reader_with_usize_limit(reader, maximum, usize::MAX as u64)
}

#[cfg(any(unix, windows))]
fn decode_reader_with_usize_limit(
    reader: &mut impl Read,
    maximum: u64,
    usize_limit: u64,
) -> Result<Vec<u8>, FileStoreError> {
    let mut magic = [0; 8];
    read_section(reader, &mut magic, EnvelopeSection::Magic)?;
    if &magic != MAGIC {
        return Err(FileStoreError::InvalidEnvelopeMagic);
    }

    let mut version = [0; 2];
    read_section(reader, &mut version, EnvelopeSection::Version)?;
    let version = u16::from_be_bytes(version);
    if version != ENVELOPE_VERSION {
        return Err(FileStoreError::UnsupportedEnvelopeVersion { found: version });
    }

    let mut length = [0; 8];
    read_section(reader, &mut length, EnvelopeSection::PayloadLength)?;
    let declared = u64::from_be_bytes(length);
    if declared > maximum {
        return Err(FileStoreError::CheckpointTooLarge {
            size: declared,
            maximum,
        });
    }
    if declared > usize_limit {
        return Err(FileStoreError::InvalidPayloadLength { declared });
    }
    let payload_len = usize::try_from(declared)
        .ok()
        .filter(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN).is_some())
        .ok_or(FileStoreError::InvalidPayloadLength { declared })?;

    let mut payload = vec![0; payload_len];
    read_section(reader, &mut payload, EnvelopeSection::Payload)?;
    let mut checksum = [0; 4];
    read_section(reader, &mut checksum, EnvelopeSection::Checksum)?;

    let mut trailing = [0; 1];
    match reader.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err(FileStoreError::TrailingData),
        Err(source) => {
            return Err(FileStoreError::Io {
                operation: FileOperation::ReadEnvelope,
                source,
            });
        }
    }

    let mut actual = crc32c::crc32c(MAGIC);
    actual = crc32c::crc32c_append(actual, &ENVELOPE_VERSION.to_be_bytes());
    actual = crc32c::crc32c_append(actual, &declared.to_be_bytes());
    actual = crc32c::crc32c_append(actual, &payload);
    let expected = u32::from_be_bytes(checksum);
    if expected != actual {
        return Err(FileStoreError::ChecksumMismatch { expected, actual });
    }
    Ok(payload)
}

#[cfg(any(unix, windows))]
fn read_section(
    reader: &mut impl Read,
    bytes: &mut [u8],
    section: EnvelopeSection,
) -> Result<(), FileStoreError> {
    match reader.read_exact(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(FileStoreError::TruncatedEnvelope { section })
        }
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::ReadEnvelope,
            source,
        }),
    }
}

/// Benchmark-only access to the stable production envelope implementation.
///
/// This module is deliberately feature-gated so ordinary consumers do not
/// acquire an additional public surface. Its functions call the same encoder
/// and bounded reader used by [`FileStore`].
#[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
#[doc(hidden)]
pub mod bench_instrumentation {
    use std::io::Read;

    use super::{CHECKSUM_LEN, FileStoreError, HEADER_LEN, decode_reader, encode_envelope};

    pub const ENVELOPE_OVERHEAD: usize = HEADER_LEN + CHECKSUM_LEN;

    pub fn encode(payload: &[u8], maximum: u64) -> Result<Vec<u8>, FileStoreError> {
        encode_envelope(payload, maximum)
    }

    pub fn decode(reader: &mut impl Read, maximum: u64) -> Result<Vec<u8>, FileStoreError> {
        decode_reader(reader, maximum)
    }
}

#[cfg(any(unix, windows))]
fn ensure_namespace_available(config: &StoreConfig) -> Result<(), FileStoreError> {
    match std::fs::metadata(&config.namespace) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(FileStoreError::NamespacePathIsNotDirectory),
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::InspectNamespace,
            source,
        }),
    }
}

#[cfg(unix)]
fn save_checkpoint(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), FileStoreError> {
    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt as StdOpenOptionsExt;

    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        FileOperation::WriteEnvelope,
    )?;
    #[cfg(test)]
    let envelope = encode_envelope(payload, config.maximum)?;
    #[cfg(not(test))]
    let (header, checksum) = envelope_parts(payload, config.maximum)?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        FileOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeAtomicOpen,
        FileOperation::OpenAtomicWriter,
    )?;
    let mut options = atomic_write_file::OpenOptions::new();
    StdOpenOptionsExt::mode(&mut options, 0o600);
    AtomicOpenOptionsExt::preserve_mode(&mut options, false);
    AtomicOpenOptionsExt::preserve_owner(&mut options, false);
    let mut writer = options.open(path).map_err(|source| FileStoreError::Io {
        operation: FileOperation::OpenAtomicWriter,
        source,
    })?;
    #[cfg(all(test, unix))]
    {
        let midpoint = envelope.len() / 2;
        writer
            .write_all(&envelope[..midpoint])
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::WriteEnvelope,
                source,
            })?;
        hit_test_stage(config, TestStage::DuringWrite, FileOperation::WriteEnvelope)?;
        writer
            .write_all(&envelope[midpoint..])
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(not(test))]
    for section in [&header[..], payload, &checksum[..]] {
        writer
            .write_all(section)
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::WriteEnvelope,
                source,
            })?;
    }
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeCommit,
        FileOperation::WriteEnvelope,
    )?;
    #[cfg(all(test, unix))]
    if let Err(source) = config
        .hook
        .as_ref()
        .map_or(Ok(()), |hook| hook(TestStage::CommitError))
    {
        return Err(FileStoreError::AtomicCommit { source });
    }
    writer
        .commit()
        .map_err(|source| FileStoreError::AtomicCommit { source })?;
    #[cfg(all(test, unix))]
    hit_test_stage(config, TestStage::AfterCommit, FileOperation::WriteEnvelope)?;
    Ok(())
}

#[cfg(unix)]
fn clear_checkpoint(config: &StoreConfig, path: &Path) -> Result<(), FileStoreError> {
    #[cfg(all(test, unix))]
    hit_test_stage(
        config,
        TestStage::BeforeRemove,
        FileOperation::RemoveCheckpoint,
    )?;
    match std::fs::remove_file(path) {
        Ok(()) => {
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::AfterRemove,
                FileOperation::RemoveCheckpoint,
            )?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::BeforeDirectorySync,
                FileOperation::SyncNamespaceDirectory,
            )?;
            sync_directory(
                &config.namespace,
                FileOperation::OpenNamespaceDirectory,
                FileOperation::SyncNamespaceDirectory,
            )?;
            #[cfg(all(test, unix))]
            hit_test_stage(
                config,
                TestStage::AfterDirectorySync,
                FileOperation::SyncNamespaceDirectory,
            )?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => ensure_namespace_available(config),
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::RemoveCheckpoint,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn inspect_checkpoint(
    config: &StoreConfig,
    path: &Path,
) -> Result<CheckpointInspection, FileStoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(CheckpointInspection {
            state: CheckpointState::Present,
            size: Some(metadata.len()),
            modified: metadata.modified().ok(),
        }),
        Ok(_) => Err(FileStoreError::UnexpectedFileType),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            Ok(CheckpointInspection {
                state: CheckpointState::Absent,
                size: None,
                modified: None,
            })
        }
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::InspectCheckpoint,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn load_with_legacy(
    config: &StoreConfig,
    canonical_path: &Path,
    legacy_filename: &Path,
) -> Result<LegacyLoad, FileStoreError> {
    if let Some(payload) = load_checkpoint(config, canonical_path)? {
        return Ok(LegacyLoad::Present(payload));
    }
    let legacy_path = config.root.join(legacy_filename);
    match std::fs::symlink_metadata(&legacy_path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(LegacyLoad::LegacyDetected {
            diagnostic_path: legacy_path,
        }),
        Ok(_) => Err(FileStoreError::UnexpectedFileType),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidFilename
            ) =>
        {
            Ok(LegacyLoad::Absent)
        }
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::InspectLegacyCheckpoint,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn inspect_with_legacy(
    config: &StoreConfig,
    canonical_path: &Path,
    legacy_filename: &Path,
) -> Result<CheckpointInspection, FileStoreError> {
    let canonical = inspect_checkpoint(config, canonical_path)?;
    if canonical.state == CheckpointState::Present {
        return Ok(canonical);
    }
    match std::fs::symlink_metadata(config.root.join(legacy_filename)) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(CheckpointInspection {
            state: CheckpointState::LegacyDetected,
            size: Some(metadata.len()),
            modified: metadata.modified().ok(),
        }),
        Ok(_) => Err(FileStoreError::UnexpectedFileType),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidFilename
            ) =>
        {
            Ok(canonical)
        }
        Err(source) => Err(FileStoreError::Io {
            operation: FileOperation::InspectLegacyCheckpoint,
            source,
        }),
    }
}

#[cfg(any(unix, windows))]
fn random_identifier() -> Result<String, FileStoreError> {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|source| FileStoreError::IdentifierGeneration { source })?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(result)
}

#[cfg(unix)]
fn quarantine_checkpoint(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, FileStoreError> {
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(FileStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config
            .namespace
            .join(format!("{hash}.session.quarantine-v1.{identifier}"));
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
                return Err(FileStoreError::QuarantineSourceMissing);
            }
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => {
                return Err(FileStoreError::QuarantineCommit {
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
) -> Result<(), FileStoreError> {
    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::BeforeDirectorySync).map_err(|source| {
            FileStoreError::QuarantineNamespaceSync {
                quarantine: quarantine.clone(),
                source,
            }
        })?;
    }

    let directory = std::fs::File::open(&config.namespace).map_err(|source| {
        FileStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        }
    })?;
    directory
        .sync_all()
        .map_err(|source| FileStoreError::QuarantineNamespaceSync {
            quarantine: quarantine.clone(),
            source,
        })?;

    #[cfg(test)]
    if let Some(hook) = &config.hook {
        hook(TestStage::AfterDirectorySync).map_err(|source| {
            FileStoreError::QuarantineNamespaceSync {
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
) -> Result<CleanupReport, FileStoreError> {
    #[cfg(not(test))]
    let _ = config;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeCleanup,
        FileOperation::EnumerateTemporaryFiles,
    )?;
    Err(FileStoreError::CleanupUnsupported {
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
) -> Result<(), FileStoreError> {
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
        return Err(FileStoreError::Io {
            operation: FileOperation::OpenAtomicWriter,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the successful CreateFileW call returned an owned handle.
    let mut file = unsafe { std::fs::File::from_raw_handle(handle) };
    for section in [header, payload, checksum] {
        file.write_all(section)
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::WriteEnvelope,
                source,
            })?;
    }
    // SAFETY: the file still owns a live handle.
    if unsafe { FlushFileBuffers(handle) } == 0 {
        return Err(FileStoreError::Io {
            operation: FileOperation::WriteEnvelope,
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
fn save_checkpoint(
    config: &StoreConfig,
    path: &Path,
    payload: &[u8],
) -> Result<(), FileStoreError> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let (header, checksum) = envelope_parts(payload, config.maximum)?;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(FileStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let staging = config
            .namespace
            .join(format!("{hash}.session.tmp-v1.save.{identifier}"));
        match create_windows_staging(&staging, &header, payload, &checksum) {
            Ok(()) => {}
            Err(FileStoreError::Io { source, .. })
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
                .map_err(|source| FileStoreError::AtomicCommit { source });
            }
            Err(source) => return Err(FileStoreError::AtomicCommit { source }),
        }
    }
}

#[cfg(windows)]
fn clear_checkpoint(config: &StoreConfig, path: &Path) -> Result<(), FileStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(FileStoreError::UnexpectedFileType)?;
    match refresh_windows_clear_age(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            ensure_namespace_available(config)?;
            return Ok(());
        }
        Err(source) => {
            return Err(FileStoreError::Io {
                operation: FileOperation::RefreshClearStagingAge,
                source,
            });
        }
    }
    loop {
        let identifier = random_identifier()?;
        let staging = config
            .namespace
            .join(format!("{hash}.session.tmp-v1.clear.{identifier}"));
        match move_file(path, &staging, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                return delete_file(&staging).map_err(|source| FileStoreError::Io {
                    operation: FileOperation::RemoveTemporaryFile,
                    source,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(FileStoreError::Io {
                    operation: FileOperation::RemoveCheckpoint,
                    source,
                });
            }
        }
    }
}

#[cfg(windows)]
fn quarantine_checkpoint(
    config: &StoreConfig,
    path: &Path,
) -> Result<QuarantineInfo, FileStoreError> {
    use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
    let hash = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or(FileStoreError::UnexpectedFileType)?;
    loop {
        let identifier = random_identifier()?;
        let destination = config
            .namespace
            .join(format!("{hash}.session.quarantine-v1.{identifier}"));
        match move_file(path, &destination, MOVEFILE_WRITE_THROUGH) {
            Ok(()) => {
                return Ok(QuarantineInfo {
                    identifier,
                    diagnostic_path: destination,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure_namespace_available(config)?;
                return Err(FileStoreError::QuarantineSourceMissing);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(FileStoreError::QuarantineCommit { source }),
        }
    }
}

#[cfg(windows)]
fn cleanup_stale_files(
    config: &StoreConfig,
    minimum_age: Duration,
) -> Result<CleanupReport, FileStoreError> {
    let now = SystemTime::now();
    let mut report = CleanupReport::default();
    let entries = std::fs::read_dir(&config.namespace).map_err(|source| FileStoreError::Io {
        operation: FileOperation::EnumerateTemporaryFiles,
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
        if !is_owned_temporary_filename(&name) {
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
fn is_owned_temporary_filename(name: &str) -> bool {
    let Some((hash, rest)) = name.split_once(".session.tmp-v1.") else {
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
    open_operation: FileOperation,
    sync_operation: FileOperation,
) -> Result<(), FileStoreError> {
    let directory = std::fs::File::open(path).map_err(|source| FileStoreError::Io {
        operation: open_operation,
        source,
    })?;
    directory.sync_all().map_err(|source| FileStoreError::Io {
        operation: sync_operation,
        source,
    })
}

#[cfg(all(test, any(unix, windows)))]
mod tests;
