//! Protocol-neutral, file-backed checkpoint storage for `rumqttc-next`.
//!
//! The store accepts opaque key and payload bytes. MQTT key encoding and session
//! decoding belong to the protocol adapters. See the crate README for the
//! on-disk format, interruption guarantees, and trust boundary.
//!
//! # Platform and filesystem scope
//!
//! This implementation supports Unix only. Non-Unix targets compile, but
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
//! Only the canonical `.session` path is authoritative. Dependency-owned
//! temporary files may remain after abrupt interruption and are always ignored.
//! This crate performs no stale-temporary-file discovery, promotion, migration,
//! quarantine, or cleanup. Ordinary drop-time cleanup remains dependency-owned
//! and is not part of the store's correctness contract.

#[cfg(unix)]
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::future::Future;
use std::io;
#[cfg(unix)]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

#[cfg(unix)]
use std::sync::mpsc;

#[cfg(unix)]
use tokio::sync::oneshot;

#[cfg(unix)]
const MAGIC: &[u8; 8] = b"RUMQSESS";
#[cfg(unix)]
const ENVELOPE_VERSION: u16 = 1;
const HEADER_LEN: usize = 18;
const CHECKSUM_LEN: usize = 4;

/// The default maximum canonical checkpoint payload size (64 MiB).
pub const DEFAULT_MAX_CHECKPOINT_SIZE: u64 = 64 * 1024 * 1024;

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
}

impl std::fmt::Display for FileOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ResolveCurrentDirectory => "resolve the current directory",
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
    #[cfg(unix)]
    submissions: mpsc::Sender<CoordinatorEvent>,
    #[cfg(all(test, unix))]
    registry_entries: Arc<std::sync::atomic::AtomicUsize>,
}

struct StoreConfig {
    root: PathBuf,
    namespace: PathBuf,
    maximum: u64,
    #[cfg(test)]
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
    /// On non-Unix targets this method returns
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

        #[cfg(not(unix))]
        {
            let _ = (root, namespace, options);
            return Err(FileStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }

        #[cfg(unix)]
        {
            let maximum = options.max_checkpoint_size;
            let config =
                tokio::task::spawn_blocking(move || initialize_unix(root, namespace, maximum))
                    .await
                    .map_err(|_| FileStoreError::CoordinationFailure)??;
            Self::from_config(config)
        }
    }

    #[cfg(unix)]
    fn from_config(config: StoreConfig) -> Result<Self, FileStoreError> {
        let config = Arc::new(config);
        let (submissions, receiver) = mpsc::channel();
        #[cfg(test)]
        let registry_entries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let coordinator_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|source| FileStoreError::Io {
                operation: FileOperation::StartCoordinator,
                source,
            })?;
        let scheduler_config = Arc::clone(&config);
        #[cfg(test)]
        let scheduler_registry_entries = Arc::clone(&registry_entries);
        std::thread::Builder::new()
            .name("rumqttc-file-store-coordinator".into())
            .spawn(move || {
                let blocking_executor = coordinator_runtime.handle().clone();
                run_scheduler(
                    &scheduler_config,
                    &receiver,
                    &blocking_executor,
                    #[cfg(test)]
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
                #[cfg(test)]
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
            tokio::task::spawn_blocking(move || initialize_unix(root, namespace, maximum))
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
        #[cfg(not(unix))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(unix)]
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
        #[cfg(not(unix))]
        {
            let _ = (canonical_key, payload);
            return unsupported_future();
        }
        #[cfg(unix)]
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
        #[cfg(not(unix))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(unix)]
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

    /// Returns the canonical checkpoint path for an opaque key.
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

#[cfg(unix)]
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

fn validate_maximum(maximum: u64) -> Result<(), FileStoreError> {
    let envelope_capacity = usize::try_from(maximum)
        .ok()
        .and_then(|size| size.checked_add(HEADER_LEN + CHECKSUM_LEN));
    if envelope_capacity.is_none() {
        return Err(FileStoreError::InvalidMaximumCheckpointSize { maximum });
    }
    Ok(())
}

#[cfg(not(unix))]
fn unsupported_future<T: Send + 'static>() -> FileStoreFuture<T> {
    Box::pin(async {
        Err(FileStoreError::UnsupportedPlatform {
            platform: std::env::consts::OS,
        })
    })
}

#[cfg(unix)]
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

#[cfg(unix)]
struct Submission {
    key_hash: [u8; 32],
    operation: Operation,
    completion_sender: mpsc::Sender<CoordinatorEvent>,
}

#[cfg(unix)]
struct QueuedOperation {
    operation: Operation,
    completion_sender: mpsc::Sender<CoordinatorEvent>,
}

#[cfg(unix)]
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
}

#[cfg(unix)]
enum BlockingResult {
    Load(Result<Option<Vec<u8>>, FileStoreError>),
    Save(Result<(), FileStoreError>),
    Clear(Result<(), FileStoreError>),
}

#[cfg(unix)]
struct Completion {
    key_hash: [u8; 32],
    outcome: Option<(Operation, BlockingResult)>,
}

#[cfg(unix)]
enum CoordinatorEvent {
    Submission(Submission),
    Completion(Completion),
}

#[cfg(unix)]
fn run_scheduler(
    config: &Arc<StoreConfig>,
    receiver: &mpsc::Receiver<CoordinatorEvent>,
    blocking_executor: &tokio::runtime::Handle,
    #[cfg(test)] registry_entries: &std::sync::atomic::AtomicUsize,
) {
    let mut queues: HashMap<[u8; 32], VecDeque<QueuedOperation>> = HashMap::new();
    let mut active = HashSet::new();

    while let Ok(event) = receiver.recv() {
        match event {
            CoordinatorEvent::Submission(submission) => {
                let key_hash = submission.key_hash;
                queues
                    .entry(key_hash)
                    .or_default()
                    .push_back(QueuedOperation {
                        operation: submission.operation,
                        completion_sender: submission.completion_sender,
                    });
                #[cfg(test)]
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
                #[cfg(test)]
                registry_entries.store(queues.len(), std::sync::atomic::Ordering::SeqCst);
                if let Some((operation, result)) = outcome {
                    deliver(operation, result);
                }
            }
        }
    }
}

#[cfg(unix)]
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

#[cfg(unix)]
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
            #[cfg(unix)]
            let result = save_checkpoint(config, path, &payload);
            #[cfg(not(unix))]
            let result = Err(FileStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
            (
                Operation::Save { payload, sender },
                BlockingResult::Save(result),
            )
        }
        Operation::Clear { sender } => {
            #[cfg(unix)]
            let result = clear_checkpoint(config, path);
            #[cfg(not(unix))]
            let result = Err(FileStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
            (Operation::Clear { sender }, BlockingResult::Clear(result))
        }
    }
}

#[cfg(unix)]
fn deliver(operation: Operation, result: BlockingResult) {
    match (operation, result) {
        (Operation::Load { sender }, BlockingResult::Load(result)) => {
            let _send_result = sender.send(result);
        }
        (Operation::Save { sender, .. }, BlockingResult::Save(result))
        | (Operation::Clear { sender }, BlockingResult::Clear(result)) => {
            let _send_result = sender.send(result);
        }
        _ => unreachable!("operation and result kinds must match"),
    }
}

#[cfg(unix)]
fn initialize_unix(
    mut root: PathBuf,
    namespace_component: PathBuf,
    maximum: u64,
) -> Result<StoreConfig, FileStoreError> {
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
        Ok(()) => sync_directory(
            &root,
            FileOperation::OpenRootDirectory,
            FileOperation::SyncRootDirectory,
        )?,
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
        #[cfg(test)]
        hook: None,
    })
}

#[cfg(test)]
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
}

#[cfg(test)]
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

#[cfg(unix)]
fn encode_envelope(payload: &[u8], maximum: u64) -> Result<Vec<u8>, FileStoreError> {
    let size = u64::try_from(payload.len())
        .map_err(|_| FileStoreError::InvalidPayloadLength { declared: u64::MAX })?;
    if size > maximum {
        return Err(FileStoreError::CheckpointTooLarge { size, maximum });
    }
    let capacity = payload
        .len()
        .checked_add(HEADER_LEN + CHECKSUM_LEN)
        .ok_or(FileStoreError::InvalidPayloadLength { declared: size })?;
    let mut envelope = Vec::with_capacity(capacity);
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&ENVELOPE_VERSION.to_be_bytes());
    envelope.extend_from_slice(&size.to_be_bytes());
    envelope.extend_from_slice(payload);
    envelope.extend_from_slice(&crc32c::crc32c(&envelope).to_be_bytes());
    Ok(envelope)
}

#[cfg(unix)]
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

#[cfg(unix)]
fn decode_reader(reader: &mut impl Read, maximum: u64) -> Result<Vec<u8>, FileStoreError> {
    decode_reader_with_usize_limit(reader, maximum, usize::MAX as u64)
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeEnvelope,
        FileOperation::WriteEnvelope,
    )?;
    let envelope = encode_envelope(payload, config.maximum)?;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::AfterEnvelope,
        FileOperation::WriteEnvelope,
    )?;
    #[cfg(test)]
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
    #[cfg(test)]
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
    writer
        .write_all(&envelope)
        .map_err(|source| FileStoreError::Io {
            operation: FileOperation::WriteEnvelope,
            source,
        })?;
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeCommit,
        FileOperation::WriteEnvelope,
    )?;
    #[cfg(test)]
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
    #[cfg(test)]
    hit_test_stage(config, TestStage::AfterCommit, FileOperation::WriteEnvelope)?;
    Ok(())
}

#[cfg(unix)]
fn clear_checkpoint(config: &StoreConfig, path: &Path) -> Result<(), FileStoreError> {
    #[cfg(test)]
    hit_test_stage(
        config,
        TestStage::BeforeRemove,
        FileOperation::RemoveCheckpoint,
    )?;
    match std::fs::remove_file(path) {
        Ok(()) => {
            #[cfg(test)]
            hit_test_stage(
                config,
                TestStage::AfterRemove,
                FileOperation::RemoveCheckpoint,
            )?;
            #[cfg(test)]
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
            #[cfg(test)]
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

#[cfg(all(test, unix))]
mod tests;
