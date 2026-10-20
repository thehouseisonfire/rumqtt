//! Optional maintained file stores. Filesystem and codec work runs on owned workers.
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crate::{
    ProtocolVersion, SessionCheckpoint, SessionStore, SessionStoreConfig, SessionStoreKey,
    StoreFailure, StoreFuture,
};
use rumqttc_session_store_file::{self as disk, AtomicBlobStoreError as DiskError};

type NativeFuture<'a, T> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>,
            > + Send
            + 'a,
    >,
>;
type Job = Box<dyn FnOnce() + Send>;
fn lock<T>(v: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    v.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FileStoreFailure {
    Invalid = 1,
    InUse,
    Closed,
    Resources,
    Io,
    Corrupt,
    Version,
    Protocol,
    Oversized,
    Unsupported,
    Shutdown,
    Panic,
    Absent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStoreError {
    pub kind: FileStoreFailure,
    pub operation: u32,
    pub io_kind: u32,
    pub os_code: Option<i32>,
}
impl FileStoreError {
    fn new(kind: FileStoreFailure) -> Self {
        Self {
            kind,
            operation: 0,
            io_kind: 0,
            os_code: None,
        }
    }
    fn store(self) -> StoreFailure {
        match self.kind {
            FileStoreFailure::InUse => StoreFailure::InUse,
            FileStoreFailure::Corrupt => StoreFailure::Corrupt,
            FileStoreFailure::Version => StoreFailure::Version,
            FileStoreFailure::Protocol => StoreFailure::Protocol,
            FileStoreFailure::Oversized => StoreFailure::Oversized,
            FileStoreFailure::Panic => StoreFailure::Panic,
            FileStoreFailure::Closed => StoreFailure::Closed,
            FileStoreFailure::Resources => StoreFailure::Resources,
            FileStoreFailure::Unsupported => StoreFailure::Unsupported,
            FileStoreFailure::Shutdown => StoreFailure::Shutdown,
            _ => StoreFailure::File(self),
        }
    }
}
impl std::fmt::Display for FileStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "file session store {:?}", self.kind)
    }
}
impl std::error::Error for FileStoreError {}
type Result<T> = std::result::Result<T, FileStoreError>;
fn error(kind: FileStoreFailure) -> FileStoreError {
    FileStoreError::new(kind)
}
fn io_error(e: &std::io::Error, operation: u32) -> FileStoreError {
    FileStoreError {
        kind: FileStoreFailure::Io,
        operation,
        io_kind: match e.kind() {
            std::io::ErrorKind::NotFound => 1,
            std::io::ErrorKind::PermissionDenied => 2,
            std::io::ErrorKind::AlreadyExists => 3,
            std::io::ErrorKind::InvalidInput => 4,
            std::io::ErrorKind::Interrupted => 5,
            std::io::ErrorKind::WouldBlock => 6,
            std::io::ErrorKind::TimedOut => 7,
            _ => 8,
        },
        os_code: e.raw_os_error(),
    }
}
fn disk_operation(op: disk::StoreOperation) -> u32 {
    match op {
        disk::StoreOperation::ResolveCurrentDirectory => 1,
        disk::StoreOperation::NormalizeRoot => 2,
        disk::StoreOperation::StartInitialization => 3,
        disk::StoreOperation::StartCoordinator => 4,
        disk::StoreOperation::InspectRoot => 5,
        disk::StoreOperation::InspectNamespace => 6,
        disk::StoreOperation::CreateNamespace => 7,
        disk::StoreOperation::OpenRootDirectory => 8,
        disk::StoreOperation::SyncRootDirectory => 9,
        disk::StoreOperation::OpenBlob => 10,
        disk::StoreOperation::ReadEnvelope => 11,
        disk::StoreOperation::OpenAtomicWriter => 12,
        disk::StoreOperation::WriteEnvelope => 13,
        disk::StoreOperation::RemoveBlob => 14,
        disk::StoreOperation::OpenNamespaceDirectory => 15,
        disk::StoreOperation::SyncNamespaceDirectory => 16,
        disk::StoreOperation::InspectBlob => 17,
        disk::StoreOperation::QuarantineBlob => 18,
        disk::StoreOperation::GenerateIdentifier => 19,
        disk::StoreOperation::EnumerateTemporaryFiles => 20,
        disk::StoreOperation::RemoveTemporaryFile => 21,
        disk::StoreOperation::RefreshClearStagingAge => 22,
        _ => 0,
    }
}
fn disk_error(e: DiskError) -> FileStoreError {
    use DiskError::*;
    match e {
        Io { operation, source } => io_error(&source, disk_operation(operation)),
        AtomicCommit { source } => io_error(&source, 13),
        QuarantineCommit { source } | QuarantineNamespaceSync { source, .. } => {
            io_error(&source, 18)
        }
        InputIo { source } | OutputIo { source } => io_error(&source, 12),
        StoreClosed => error(FileStoreFailure::Closed),
        BlobTooLarge { .. } | InvalidPayloadLength { .. } => error(FileStoreFailure::Oversized),
        UnsupportedEnvelopeVersion { .. } => error(FileStoreFailure::Version),
        InvalidEnvelopeDomain { .. }
        | TruncatedEnvelope { .. }
        | TrailingData
        | ChecksumMismatch { .. } => error(FileStoreFailure::Corrupt),
        UnsupportedPlatform { .. } | CleanupUnsupported { .. } => {
            error(FileStoreFailure::Unsupported)
        }
        ShutdownFailure | EngineFailed | WorkerUnavailable => error(FileStoreFailure::Shutdown),
        Configuration(_)
        | InvalidCleanupAge
        | RootDoesNotExist
        | RootIsNotDirectory
        | NamespacePathIsNotDirectory => error(FileStoreFailure::Invalid),
        QuarantineSourceMissing => error(FileStoreFailure::Absent),
        _ => error(FileStoreFailure::Io),
    }
}
macro_rules! map_error {
    ($name:ident, $module:ident, $native:ident) => {
        fn $name(e: disk::$module::SessionFileStoreError) -> FileStoreError {
            use disk::$module::SessionFileStoreError::*;
            match e {
                FileStore(e) => disk_error(e),
                SessionDecode($native::SessionDecodeError::WrongProtocol { .. }) => {
                    error(FileStoreFailure::Protocol)
                }
                SessionDecode($native::SessionDecodeError::UnsupportedCodecVersion { .. }) => {
                    error(FileStoreFailure::Version)
                }
                SessionDecode($native::SessionDecodeError::LimitExceeded { .. })
                | SessionEncode(_) => error(FileStoreFailure::Oversized),
                SessionDecode(_) => error(FileStoreFailure::Corrupt),
                KeyEncode(_) => error(FileStoreFailure::Invalid),
                _ => error(FileStoreFailure::Io),
            }
        }
    };
}
map_error!(v4_error, v4, rumqttc_v4);
map_error!(v5_error, v5, rumqttc_v5);

#[derive(Clone, Copy, Debug)]
pub struct FileStoreContextOptions {
    pub worker_threads: usize,
    pub store_capacity: usize,
    pub operation_capacity: usize,
    pub observation_capacity: usize,
    pub byte_capacity: usize,
}
impl Default for FileStoreContextOptions {
    fn default() -> Self {
        Self {
            worker_threads: 2,
            store_capacity: 64,
            operation_capacity: 256,
            observation_capacity: 256,
            byte_capacity: 256 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct FileStoreOptions {
    pub max_checkpoint_size: usize,
    pub max_concurrent_operations: usize,
    pub max_report_entries: usize,
}
impl Default for FileStoreOptions {
    fn default() -> Self {
        Self {
            max_checkpoint_size: 16 * 1024 * 1024 - 8,
            max_concurrent_operations: 4,
            max_report_entries: 4096,
        }
    }
}
#[derive(Default)]
struct Counts {
    closing: bool,
    operations: usize,
    observations: usize,
    bytes: usize,
    stores: Vec<Arc<StoreResources>>,
}
struct Shared {
    options: FileStoreContextOptions,
    counts: Mutex<Counts>,
}
struct Context {
    submissions: Mutex<()>,
    shared: Arc<Shared>,
    failure: Arc<Mutex<Option<FileStoreError>>>,
    sender: Mutex<Option<flume::Sender<Job>>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct FileStoreContext(Arc<Context>);
impl std::fmt::Debug for FileStoreContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStoreContext").finish_non_exhaustive()
    }
}
struct Admission {
    shared: Arc<Shared>,
}
impl Drop for Admission {
    fn drop(&mut self) {
        let mut c = lock(&self.shared.counts);
        c.operations -= 1;
    }
}
struct Observation {
    shared: Arc<Shared>,
}
struct ByteReservation {
    shared: Arc<Shared>,
    bytes: Mutex<usize>,
}
impl ByteReservation {
    fn grow(&self, bytes: usize) -> Result<()> {
        let mut c = lock(&self.shared.counts);
        let mut current = lock(&self.bytes);
        if bytes > self.shared.options.byte_capacity.saturating_sub(c.bytes) {
            return Err(error(FileStoreFailure::Resources));
        }
        c.bytes += bytes;
        *current += bytes;
        Ok(())
    }
}
impl Drop for ByteReservation {
    fn drop(&mut self) {
        let bytes = *self
            .bytes
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lock(&self.shared.counts).bytes -= bytes;
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        let mut c = lock(&self.shared.counts);
        c.observations -= 1;
    }
}
struct BudgetedValue<T> {
    value: T,
    _bytes: Arc<ByteReservation>,
}
/// Shared operation result that retains its byte reservation until its final owner is dropped.
///
/// Dereferencing borrows the result without detaching it from its reservation.
pub struct FileStoreValue<T>(Arc<BudgetedValue<T>>);
impl<T> Clone for FileStoreValue<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> std::ops::Deref for FileStoreValue<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0.value
    }
}
impl<T: std::fmt::Debug> std::fmt::Debug for FileStoreValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.value.fmt(f)
    }
}
struct Outcome<T> {
    value: Mutex<Option<Result<FileStoreValue<T>>>>,
    changed: Condvar,
    notify: tokio::sync::Notify,
    _reservation: Observation,
    _bytes: Arc<ByteReservation>,
}
pub struct FileStoreOperation<T>(Arc<Outcome<T>>);
impl<T> Clone for FileStoreOperation<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> FileStoreOperation<T> {
    pub fn poll(&self) -> Option<Result<FileStoreValue<T>>> {
        lock(&self.0.value).clone()
    }
    pub fn wait(&self, timeout: Duration) -> crate::Result<Option<Result<FileStoreValue<T>>>> {
        crate::execution::check_wait(timeout)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| crate::Error::configuration("invalid file observation timeout"))?;
        let mut value = lock(&self.0.value);
        while value.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            value = self
                .0
                .changed
                .wait_timeout(value, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        Ok(value.clone())
    }
}
impl FileStoreContext {
    pub fn new(options: FileStoreContextOptions) -> crate::Result<Self> {
        crate::execution::check_wait(Duration::from_secs(1))?;
        if [
            options.worker_threads,
            options.store_capacity,
            options.operation_capacity,
            options.observation_capacity,
            options.byte_capacity,
        ]
        .contains(&0)
        {
            return Err(crate::Error::configuration(
                "file store context limits must be nonzero",
            ));
        }
        let (sender, receiver) = flume::unbounded::<Job>();
        let context = Self(Arc::new(Context {
            submissions: Mutex::new(()),
            failure: Arc::new(Mutex::new(None)),
            shared: Arc::new(Shared {
                options,
                counts: Mutex::new(Counts::default()),
            }),
            sender: Mutex::new(Some(sender)),
            threads: Mutex::new(Vec::new()),
        }));
        for _ in 0..options.worker_threads {
            let receiver = receiver.clone();
            match thread::Builder::new()
                .name("rumqtt-file-worker".into())
                .spawn(move || {
                    crate::execution::with_managed_thread(|| {
                        while let Ok(job) = receiver.recv() {
                            job();
                        }
                    });
                }) {
                Ok(thread) => lock(&context.0.threads).push(thread),
                Err(_) => {
                    lock(&context.0.sender).take();
                    for thread in lock(&context.0.threads).drain(..) {
                        let _ = thread.join();
                    }
                    return Err(crate::Error::configuration(
                        "could not start file store worker",
                    ));
                }
            }
        }
        Ok(context)
    }
    fn operation<T: Send + Sync + 'static>(
        &self,
        bytes: usize,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<FileStoreOperation<T>> {
        self.operation_budgeted(bytes, move |_| work())
    }
    fn operation_budgeted<T: Send + Sync + 'static>(
        &self,
        bytes: usize,
        work: impl FnOnce(Arc<ByteReservation>) -> Result<T> + Send + 'static,
    ) -> Result<FileStoreOperation<T>> {
        let mut c = lock(&self.0.shared.counts);
        if c.closing {
            return Err(error(FileStoreFailure::Closed));
        }
        if c.operations >= self.0.shared.options.operation_capacity
            || c.observations >= self.0.shared.options.observation_capacity
            || bytes > self.0.shared.options.byte_capacity.saturating_sub(c.bytes)
        {
            return Err(error(FileStoreFailure::Resources));
        }
        c.operations += 1;
        c.observations += 1;
        c.bytes += bytes;
        let admission = Admission {
            shared: self.0.shared.clone(),
        };
        let budget = Arc::new(ByteReservation {
            shared: self.0.shared.clone(),
            bytes: Mutex::new(bytes),
        });
        let state = Arc::new(Outcome {
            value: Mutex::new(None),
            changed: Condvar::new(),
            notify: tokio::sync::Notify::new(),
            _reservation: Observation {
                shared: self.0.shared.clone(),
            },
            _bytes: budget.clone(),
        });
        let output = state.clone();
        // Submission and shutdown are serialized by counts. No foreign code runs under it.
        let job = Box::new(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(budget.clone())))
                    .unwrap_or_else(|payload| {
                        std::mem::forget(payload);
                        Err(error(FileStoreFailure::Panic))
                    });
            drop(admission);
            *lock(&output.value) = Some(result.map(|value| {
                FileStoreValue(Arc::new(BudgetedValue {
                    value,
                    _bytes: budget,
                }))
            }));
            output.changed.notify_all();
            output.notify.notify_waiters();
        });
        let result = lock(&self.0.sender)
            .as_ref()
            .ok_or_else(|| error(FileStoreFailure::Closed))?
            .send(job);
        drop(c);
        result.map_err(|_| error(FileStoreFailure::Shutdown))?;
        Ok(FileStoreOperation(state))
    }
    pub fn open(
        &self,
        protocol: ProtocolVersion,
        root: PathBuf,
        options: FileStoreOptions,
    ) -> Result<FileStoreOperation<FileStore>> {
        let _publication = lock(&self.0.submissions);
        if !root.is_absolute()
            || options.max_checkpoint_size == 0
            || options.max_checkpoint_size > 256 * 1024 * 1024 - 8
            || options.max_concurrent_operations == 0
            || options.max_report_entries == 0
        {
            return Err(error(FileStoreFailure::Invalid));
        }
        let resources = Arc::new(StoreResources {
            shared: Arc::downgrade(&self.0.shared),
            protocol,
            options,
            state: Mutex::new(StoreState::default()),
            changed: Condvar::new(),
            native: Mutex::new(None),
            bridge: OnceLock::new(),
            directory: OnceLock::new(),
        });
        let owner = Arc::new(StoreOwner {
            context: self.clone(),
            resources: resources.clone(),
            close_observer: Mutex::new(None),
            wrapped: Mutex::new(Weak::<FileSession>::new()),
        });
        {
            let mut c = lock(&self.0.shared.counts);
            if c.closing {
                return Err(error(FileStoreFailure::Closed));
            }
            if c.stores.len() >= self.0.shared.options.store_capacity {
                return Err(error(FileStoreFailure::Resources));
            }
            c.stores.push(resources);
        }
        let root_bytes = root.as_os_str().len().saturating_mul(4);
        lock(&owner.state).pending += 1;
        let initializing = WorkLease {
            owner: owner.clone(),
            key: None,
            maintenance: false,
            exclusive: false,
        };
        self.operation(root_bytes, move || {
            let _initializing = initializing;
            let opened = (|| {
                let root = std::fs::canonicalize(root).map_err(|e| io_error(&e, 2))?;
                let identity = root_identity(&root, protocol)?;
                if !lock(ROOTS.get_or_init(Default::default)).insert(identity.clone()) {
                    return Err(error(FileStoreFailure::InUse));
                }
                lock(&owner.state).root = Some(identity);
                let _ = owner.directory.set(root.clone());
                let native = NativeStore::open(protocol, root, options)?;
                let bridge = match &native {
                    NativeStore::V4(store) => crate::rust_session_store::from_v4(Arc::new(
                        WorkerV4(store.clone(), options.max_checkpoint_size.saturating_mul(4)),
                    )),
                    NativeStore::V5(store) => crate::rust_session_store::from_v5(Arc::new(
                        WorkerV5(store.clone(), options.max_checkpoint_size.saturating_mul(4)),
                    )),
                };
                let _ = owner.bridge.set(bridge);
                *lock(&owner.native) = Some(native);
                {
                    let mut s = lock(&owner.state);
                    if s.phase == 0 {
                        s.phase = 1;
                    }
                }
                Ok(FileStore(owner.clone()))
            })();
            if opened.is_err() {
                let mut s = lock(&owner.state);
                s.phase = 4;
                release_root(&mut s);
            }
            owner.changed.notify_all();
            opened
        })
    }
    pub fn request_shutdown(&self) -> Result<()> {
        let _publication = lock(&self.0.submissions);
        let mut c = lock(&self.0.shared.counts);
        if c.closing {
            return Ok(());
        }
        let stores = c.stores.clone();
        if stores.iter().any(|s| !lock(&s.state).clients.is_empty()) {
            return Err(error(FileStoreFailure::InUse));
        }
        c.closing = true;
        let sender = lock(&self.0.sender).take();
        drop(c);
        if let Some(sender) = sender {
            for store in stores {
                let failure = self.0.failure.clone();
                let _ = sender.send(Box::new(move || {
                    if let Err(e) = store.close_blocking() {
                        *lock(&failure) = Some(e);
                    }
                }));
            }
        }
        Ok(())
    }
    pub fn try_join(&self) -> Result<bool> {
        if !lock(&self.0.shared.counts).closing {
            return Err(error(FileStoreFailure::Invalid));
        }
        let mut threads = lock(&self.0.threads);
        if threads.iter().any(|t| !t.is_finished()) {
            return Ok(false);
        }
        let mut failed = false;
        for thread in threads.drain(..) {
            failed |= thread.join().is_err();
        }
        if failed {
            *lock(&self.0.failure) = Some(error(FileStoreFailure::Shutdown));
        }
        if let Some(e) = *lock(&self.0.failure) {
            Err(e)
        } else {
            Ok(true)
        }
    }
    pub fn join(&self, timeout: Duration) -> crate::Result<bool> {
        crate::execution::check_wait(timeout)?;
        let start = Instant::now();
        loop {
            if self
                .try_join()
                .map_err(|e| crate::Error::store(e.store()))?
            {
                return Ok(true);
            }
            if start.elapsed() >= timeout {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(1).min(timeout.saturating_sub(start.elapsed())));
        }
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        lock(&self.sender).take();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RootIdentity {
    path: PathBuf,
    protocol: ProtocolVersion,
    #[cfg(any(unix, windows))]
    device: u64,
    #[cfg(any(unix, windows))]
    inode: u64,
}
// Unix identities compare aliases by device/inode, rather than their spelling.
fn root_identity(path: &std::path::Path, protocol: ProtocolVersion) -> Result<RootIdentity> {
    let metadata = std::fs::metadata(path).map_err(|e| io_error(&e, 2))?;
    if !metadata.is_dir() {
        return Err(error(FileStoreFailure::Invalid));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(RootIdentity {
            path: PathBuf::new(),
            protocol,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            GetFileInformationByHandle,
        };
        let root = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(|e| io_error(&e, 2))?;
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        if unsafe { GetFileInformationByHandle(root.as_raw_handle().cast(), info.as_mut_ptr()) }
            == 0
        {
            return Err(io_error(&std::io::Error::last_os_error(), 2));
        }
        let info = unsafe { info.assume_init() };
        Ok(RootIdentity {
            path: PathBuf::new(),
            protocol,
            device: u64::from(info.dwVolumeSerialNumber),
            inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(RootIdentity {
            path: path.to_owned(),
            protocol,
        })
    }
}
static ROOTS: OnceLock<Mutex<HashSet<RootIdentity>>> = OnceLock::new();
#[derive(Default)]
struct StoreState {
    closing_worker: bool,
    terminal_error: Option<FileStoreError>,
    phase: u32,
    root: Option<RootIdentity>,
    clients: HashSet<SessionStoreKey>,
    work: HashMap<SessionStoreKey, usize>,
    exclusive: HashSet<SessionStoreKey>,
    pending: usize,
    maintenance: bool,
}
fn release_root(state: &mut StoreState) {
    if let Some(root) = state.root.take() {
        lock(ROOTS.get_or_init(Default::default)).remove(&root);
    }
}
struct StoreResources {
    shared: Weak<Shared>,
    protocol: ProtocolVersion,
    options: FileStoreOptions,
    state: Mutex<StoreState>,
    changed: Condvar,
    native: Mutex<Option<NativeStore>>,
    bridge: OnceLock<Arc<dyn SessionStore>>,
    directory: OnceLock<PathBuf>,
}
struct StoreOwner {
    context: FileStoreContext,
    resources: Arc<StoreResources>,
    close_observer: Mutex<Option<FileStoreOperation<FileStoreResult>>>,
    wrapped: Mutex<Weak<FileSession>>,
}
impl std::ops::Deref for StoreOwner {
    type Target = StoreResources;
    fn deref(&self) -> &StoreResources {
        &self.resources
    }
}
#[derive(Clone)]
pub struct FileStore(Arc<StoreOwner>);
impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("protocol", &self.0.protocol)
            .finish_non_exhaustive()
    }
}
pub struct FileStoreClientLease {
    owner: Arc<StoreOwner>,
    key: SessionStoreKey,
}
impl std::fmt::Debug for FileStoreClientLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStoreClientLease")
            .finish_non_exhaustive()
    }
}
impl Drop for FileStoreClientLease {
    fn drop(&mut self) {
        lock(&self.owner.state).clients.remove(&self.key);
        self.owner.changed.notify_all();
    }
}
struct WorkLease {
    owner: Arc<StoreOwner>,
    key: Option<SessionStoreKey>,
    maintenance: bool,
    exclusive: bool,
}
impl Drop for WorkLease {
    fn drop(&mut self) {
        let mut s = lock(&self.owner.state);
        s.pending -= 1;
        if let Some(key) = &self.key {
            let count = s.work.get_mut(key).expect("accepted key work");
            *count -= 1;
            if *count == 0 {
                s.work.remove(key);
            }
            if self.exclusive {
                s.exclusive.remove(key);
            }
        }
        if self.maintenance {
            s.maintenance = false;
        }
        self.owner.changed.notify_all();
    }
}
impl FileStore {
    pub fn protocol(&self) -> ProtocolVersion {
        self.0.protocol
    }
    pub fn configuration(
        &self,
        scope: String,
        timeout: Duration,
        limit: usize,
    ) -> crate::Result<SessionStoreConfig> {
        if limit > self.0.options.max_checkpoint_size + 8 {
            return Err(crate::Error::configuration(
                "attachment checkpoint bound exceeds file store bound",
            ));
        }
        {
            let context = lock(&self.0.context.0.shared.counts);
            if context.closing || lock(&self.0.state).phase != 1 {
                return Err(crate::Error::store(StoreFailure::Closed));
            }
        }
        let mut weak = lock(&self.0.wrapped);
        let store = weak.upgrade().unwrap_or_else(|| {
            let store = Arc::new(FileSession(self.clone()));
            *weak = Arc::downgrade(&store);
            store
        });
        let mut config = SessionStoreConfig::new(store, scope);
        config.timeout = timeout;
        config.max_checkpoint_size = limit;
        config.validate()?;
        Ok(config)
    }
    pub(crate) fn acquire_client(
        &self,
        key: &SessionStoreKey,
    ) -> crate::Result<FileStoreClientLease> {
        let c = lock(&self.0.context.0.shared.counts);
        let mut s = lock(&self.0.state);
        if key.protocol != self.protocol() {
            return Err(crate::Error::store(StoreFailure::Protocol));
        }
        if c.closing || s.phase != 1 {
            return Err(crate::Error::store(StoreFailure::Closed));
        }
        if s.maintenance || s.work.contains_key(key) || !s.clients.insert(key.clone()) {
            return Err(crate::Error::store(StoreFailure::InUse));
        }
        Ok(FileStoreClientLease {
            owner: self.0.clone(),
            key: key.clone(),
        })
    }
    fn reserve(
        &self,
        key: Option<SessionStoreKey>,
        exclusive: bool,
        maintenance: bool,
    ) -> Result<WorkLease> {
        let mut s = lock(&self.0.state);
        if key.as_ref().is_some_and(|k| k.protocol != self.protocol()) {
            return Err(error(FileStoreFailure::Protocol));
        }
        if s.phase != 1 {
            return Err(error(FileStoreFailure::Closed));
        }
        if s.maintenance
            || key.as_ref().is_some_and(|k| s.exclusive.contains(k))
            || (maintenance && (!s.clients.is_empty() || s.pending != 0))
            || (exclusive
                && key
                    .as_ref()
                    .is_some_and(|k| s.clients.contains(k) || s.work.contains_key(k)))
        {
            return Err(error(FileStoreFailure::InUse));
        }
        s.pending += 1;
        if let Some(key) = &key {
            *s.work.entry(key.clone()).or_default() += 1;
            if exclusive {
                s.exclusive.insert(key.clone());
            }
        }
        if maintenance {
            s.maintenance = true;
        }
        Ok(WorkLease {
            owner: self.0.clone(),
            key,
            maintenance,
            exclusive,
        })
    }
    fn key(&self, scope: String, client_id: String) -> Result<SessionStoreKey> {
        if scope.is_empty()
            || scope.contains('\0')
            || client_id.is_empty()
            || client_id.contains('\0')
        {
            return Err(error(FileStoreFailure::Invalid));
        }
        Ok(SessionStoreKey {
            protocol: self.protocol(),
            scope,
            client_id,
        })
    }
    pub fn inspect(
        &self,
        scope: String,
        id: String,
    ) -> Result<FileStoreOperation<FileStoreResult>> {
        self.admin(self.key(scope, id)?, 1)
    }
    pub fn quarantine(
        &self,
        scope: String,
        id: String,
    ) -> Result<FileStoreOperation<FileStoreResult>> {
        self.admin(self.key(scope, id)?, 2)
    }
    pub fn clear(&self, scope: String, id: String) -> Result<FileStoreOperation<FileStoreResult>> {
        self.admin(self.key(scope, id)?, 3)
    }
    fn submit<T: Send + Sync + 'static>(
        &self,
        key: Option<SessionStoreKey>,
        exclusive: bool,
        maintenance: bool,
        bytes: usize,
        work: impl FnOnce(Arc<StoreOwner>) -> Result<T> + Send + 'static,
    ) -> Result<FileStoreOperation<T>> {
        self.submit_budgeted(key, exclusive, maintenance, bytes, move |owner, _| {
            work(owner)
        })
    }
    fn submit_budgeted<T: Send + Sync + 'static>(
        &self,
        key: Option<SessionStoreKey>,
        exclusive: bool,
        maintenance: bool,
        bytes: usize,
        work: impl FnOnce(Arc<StoreOwner>, Arc<ByteReservation>) -> Result<T> + Send + 'static,
    ) -> Result<FileStoreOperation<T>> {
        let _publication = lock(&self.0.context.0.submissions);
        // Peak key storage: closure, lease, work map, native key, encoded key,
        // and (for exclusive operations) the exclusive set.
        let key_bytes = key.as_ref().map_or(0, |k| {
            k.scope
                .len()
                .saturating_add(k.client_id.len())
                .saturating_mul(if exclusive { 6 } else { 5 })
                // The adapter encoding adds a version, protocol and two lengths.
                .saturating_add(10)
        });
        let path_bytes = self
            .0
            .directory
            .get()
            .map_or(0, |p| p.as_os_str().len().saturating_mul(4))
            // Namespace, encoded filename and quarantine identifier/path storage.
            .saturating_add(1024);
        let bytes = bytes.saturating_add(key_bytes).saturating_add(path_bytes);
        let lease = self.reserve(key, exclusive, maintenance)?;
        let owner = self.0.clone();
        self.0.context.operation_budgeted(bytes, move |budget| {
            let result = work(owner, budget);
            drop(lease);
            result
        })
    }
    fn admin(
        &self,
        key: SessionStoreKey,
        kind: u32,
    ) -> Result<FileStoreOperation<FileStoreResult>> {
        self.submit(Some(key.clone()), kind != 1, false, 0, move |owner| {
            let native = lock(&owner.native)
                .clone()
                .ok_or_else(|| error(FileStoreFailure::Closed))?;
            native.admin(&key, kind)
        })
    }
    pub fn cleanup(&self, age: Duration) -> Result<FileStoreOperation<FileStoreResult>> {
        if age.is_zero() {
            return Err(error(FileStoreFailure::Invalid));
        }
        self.submit(
            None,
            true,
            true,
            self.0.options.max_report_entries.saturating_mul(1024),
            move |owner| {
                #[cfg(windows)]
                {
                    let directory = owner
                        .directory
                        .get()
                        .ok_or_else(|| error(FileStoreFailure::Closed))?
                        .join(if owner.protocol == ProtocolVersion::V4 {
                            "v4"
                        } else {
                            "v5"
                        });
                    // The shared maintenance authority excludes all supported writers.
                    // Bound the engine's report before it enumerates/mutates the namespace.
                    let mut count = 0usize;
                    for entry in std::fs::read_dir(directory).map_err(|e| io_error(&e, 20))? {
                        entry.map_err(|e| io_error(&e, 20))?;
                        count += 1;
                        if count > owner.options.max_report_entries {
                            return Err(error(FileStoreFailure::Resources));
                        }
                    }
                }
                let native = lock(&owner.native)
                    .clone()
                    .ok_or_else(|| error(FileStoreFailure::Closed))?;
                native.cleanup(age)
            },
        )
    }
    pub fn close(&self) -> Result<FileStoreOperation<FileStoreResult>> {
        let _publication = lock(&self.0.context.0.submissions);
        if let Some(op) = lock(&self.0.close_observer).as_ref() {
            return Ok(op.clone());
        }
        {
            let c = lock(&self.0.context.0.shared.counts);
            let mut s = lock(&self.0.state);
            if c.closing {
                return Err(error(FileStoreFailure::Closed));
            }
            if !s.clients.is_empty() {
                return Err(error(FileStoreFailure::InUse));
            }
            if s.phase == 0 {
                return Err(error(FileStoreFailure::Invalid));
            }
            s.phase = 2;
        }
        let owner = self.0.clone();
        let op = self.0.context.operation(0, move || {
            if let Err(e) = owner.close_blocking() {
                *lock(&owner.context.0.failure) = Some(e);
                return Err(e);
            }
            Ok(FileStoreResult::Closed)
        });
        if op.is_err() {
            let mut s = lock(&self.0.state);
            if s.phase == 2 {
                s.phase = 1;
            }
        }
        if let Ok(op) = &op {
            *lock(&self.0.close_observer) = Some(op.clone());
        }
        op
    }
}
impl StoreResources {
    fn close_blocking(&self) -> Result<()> {
        let mut s = lock(&self.state);
        loop {
            if let Some(e) = s.terminal_error {
                return Err(e);
            }
            if s.phase == 3 {
                return Ok(());
            }
            if !s.closing_worker {
                break;
            }
            s = self
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        s.closing_worker = true;
        s.phase = 2;
        while s.pending != 0 {
            s = self
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(s);
        let result = if let Some(native) = lock(&self.native).take() {
            native.close()
        } else {
            Ok(())
        };
        let mut s = lock(&self.state);
        s.closing_worker = false;
        s.terminal_error = result.err();
        s.phase = if result.is_ok() { 3 } else { 4 };
        if result.is_ok() {
            release_root(&mut s);
        }
        self.changed.notify_all();
        drop(s);
        if result.is_ok()
            && let Some(shared) = self.shared.upgrade()
        {
            lock(&shared.counts)
                .stores
                .retain(|s| !std::ptr::eq(Arc::as_ptr(s), self));
        }
        result
    }
}
impl Drop for StoreOwner {
    fn drop(&mut self) {
        // The context registry independently owns every resource through joined
        // teardown. Shutdown cannot miss a last owner whose destructor is racing.
        let s = lock(&self.state);
        if s.phase == 3 {
            return;
        }
        if s.phase == 0 && s.pending == 0 {
            // Admission failed before initialization was accepted. No worker
            // can retire a nonexistent initialization reservation.
            drop(s);
            lock(&self.context.0.shared.counts)
                .stores
                .retain(|r| !Arc::ptr_eq(r, &self.resources));
            return;
        }
        drop(s);
        let resources = self.resources.clone();
        let failure = self.context.0.failure.clone();
        let job: Job = Box::new(move || {
            if let Err(e) = resources.close_blocking() {
                *lock(&failure) = Some(e);
            }
        });
        if let Some(sender) = lock(&self.context.0.sender).as_ref() {
            let _ = sender.send(job);
        }
        // If shutdown took the sender, it already enqueued these resources.
    }
}
#[derive(Debug)]
pub enum FileStoreResult {
    Inspection {
        present: bool,
        size: Option<u64>,
        modified: Option<SystemTime>,
    },
    Quarantined {
        identifier: String,
        path: PathBuf,
    },
    Cleared,
    Cleanup {
        removed: Vec<String>,
        skipped: Vec<String>,
        failures: Vec<(String, FileStoreError)>,
    },
    Closed,
}
#[derive(Clone)]
enum NativeStore {
    V4(disk::v4::BlockingSessionFileStore),
    V5(disk::v5::BlockingSessionFileStore),
}
impl NativeStore {
    fn open(protocol: ProtocolVersion, root: PathBuf, options: FileStoreOptions) -> Result<Self> {
        let options = disk::FileStoreOptions {
            max_checkpoint_size: options.max_checkpoint_size as u64,
            max_concurrent_operations: NonZeroUsize::new(options.max_concurrent_operations)
                .ok_or_else(|| error(FileStoreFailure::Invalid))?,
        };
        match protocol {
            ProtocolVersion::V4 => {
                disk::v4::BlockingSessionFileStore::open_with_options(root, options)
                    .map(Self::V4)
                    .map_err(v4_error)
            }
            ProtocolVersion::V5 => {
                disk::v5::BlockingSessionFileStore::open_with_options(root, options)
                    .map(Self::V5)
                    .map_err(v5_error)
            }
        }
    }
    fn close(self) -> Result<()> {
        match self {
            Self::V4(s) => s.close().map_err(v4_error),
            Self::V5(s) => s.close().map_err(v5_error),
        }
    }
    fn admin(&self, key: &SessionStoreKey, kind: u32) -> Result<FileStoreResult> {
        macro_rules! run {
            ($store:expr, $native:ident, $map:ident) => {{
                let key = $native::SessionStoreKey::new(key.scope.clone(), key.client_id.clone());
                match kind {
                    1 => {
                        let info = $store.inspect(&key).map_err($map)?;
                        Ok(FileStoreResult::Inspection {
                            present: info.state == disk::CheckpointState::Present,
                            size: info.size,
                            modified: info.modified,
                        })
                    }
                    2 => {
                        let info = $store.quarantine(&key).map_err($map)?;
                        Ok(FileStoreResult::Quarantined {
                            identifier: info.identifier,
                            path: info.diagnostic_path,
                        })
                    }
                    _ => {
                        $store.operator_clear(&key).map_err($map)?;
                        Ok(FileStoreResult::Cleared)
                    }
                }
            }};
        }
        match self {
            Self::V4(s) => run!(s, rumqttc_v4, v4_error),
            Self::V5(s) => run!(s, rumqttc_v5, v5_error),
        }
    }
    fn cleanup(&self, age: Duration) -> Result<FileStoreResult> {
        let report = match self {
            Self::V4(s) => s
                .cleanup_stale_temporary_files_before_use(age)
                .map_err(v4_error)?,
            Self::V5(s) => s
                .cleanup_stale_temporary_files_before_use(age)
                .map_err(v5_error)?,
        };
        Ok(FileStoreResult::Cleanup {
            removed: report.removed,
            skipped: report.skipped,
            failures: report
                .failures
                .into_iter()
                .map(|f| (f.identifier, io_error(&f.source, 0)))
                .collect(),
        })
    }
}
macro_rules! worker_native {
    ($worker:ident, $disk:ident, $native:ident, $map:ident) => {
        #[derive(Debug)]
        struct $worker(disk::$disk::BlockingSessionFileStore, usize);
        impl $native::SessionStore for $worker {
            fn load<'a>(
                &'a self,
                key: &'a $native::SessionStoreKey,
            ) -> NativeFuture<'a, Option<$native::PersistedSession>> {
                Box::pin(async move {
                    self.0
                        .load(key)
                        .map_err(|e| Box::new($map(e).store()) as $native::SessionStoreError)
                })
            }
            fn load_with_limit<'a>(
                &'a self,
                key: &'a $native::SessionStoreKey,
                limit: usize,
            ) -> NativeFuture<'a, Option<$native::PersistedSession>> {
                Box::pin(async move {
                    self.0
                        .load_with_limits(key, limit, self.1)
                        .map_err(|e| Box::new($map(e).store()) as $native::SessionStoreError)
                })
            }
            fn save<'a>(
                &'a self,
                key: &'a $native::SessionStoreKey,
                session: &'a $native::PersistedSession,
            ) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    self.0
                        .save(key, session)
                        .map_err(|e| Box::new($map(e).store()) as $native::SessionStoreError)
                })
            }
            fn clear<'a>(&'a self, key: &'a $native::SessionStoreKey) -> NativeFuture<'a, ()> {
                Box::pin(async move {
                    self.0
                        .operator_clear(key)
                        .map_err(|e| Box::new($map(e).store()) as $native::SessionStoreError)
                })
            }
        }
    };
}
worker_native!(WorkerV4, v4, rumqttc_v4, v4_error);
worker_native!(WorkerV5, v5, rumqttc_v5, v5_error);
fn from_store(e: StoreFailure) -> FileStoreError {
    if let StoreFailure::File(detail) = e {
        return detail;
    }
    error(match e {
        StoreFailure::Corrupt => FileStoreFailure::Corrupt,
        StoreFailure::Version => FileStoreFailure::Version,
        StoreFailure::Protocol | StoreFailure::KeyMismatch => FileStoreFailure::Protocol,
        StoreFailure::Oversized => FileStoreFailure::Oversized,
        StoreFailure::Closed => FileStoreFailure::Closed,
        StoreFailure::Resources | StoreFailure::PublishBudgetExceeded => {
            FileStoreFailure::Resources
        }
        StoreFailure::Unsupported => FileStoreFailure::Unsupported,
        StoreFailure::InUse => FileStoreFailure::InUse,
        StoreFailure::Panic => FileStoreFailure::Panic,
        StoreFailure::Shutdown => FileStoreFailure::Shutdown,
        _ => FileStoreFailure::Io,
    })
}
pub(crate) fn decode_budget(limit: usize, serialized: usize) -> usize {
    limit
        .saturating_mul(4)
        .min(serialized.saturating_mul(512).saturating_add(1024))
}
impl FileStore {
    pub(crate) async fn codec<T: Send + Sync + 'static>(
        &self,
        key: SessionStoreKey,
        bytes: usize,
        work: impl FnOnce() -> std::result::Result<T, StoreFailure> + Send + 'static,
    ) -> std::result::Result<T, StoreFailure> {
        let op = self
            .submit(Some(key), false, false, bytes, move |_| Ok(work()))
            .map_err(FileStoreError::store)?;
        wait_owned(op).await.map_err(FileStoreError::store)?
    }
}
struct FileSession(FileStore);
impl SessionStore for FileSession {
    fn file_store(&self) -> Option<FileStore> {
        Some(self.0.clone())
    }
    fn load(&self, key: SessionStoreKey) -> StoreFuture<Option<SessionCheckpoint>> {
        self.load_with_limit(key, self.0.0.options.max_checkpoint_size + 8)
    }
    fn load_with_limit(
        &self,
        key: SessionStoreKey,
        limit: usize,
    ) -> StoreFuture<Option<SessionCheckpoint>> {
        let store = self.0.clone();
        Box::pin(async move {
            let op = store
                .submit_budgeted(Some(key.clone()), false, false, 0, move |owner, budget| {
                    let native = lock(&owner.native)
                        .clone()
                        .ok_or_else(|| error(FileStoreFailure::Closed))?;
                    let metadata = native.admin(&key, 1)?;
                    let FileStoreResult::Inspection { present, size, .. } = metadata else {
                        unreachable!()
                    };
                    if !present {
                        return Ok(None);
                    }
                    let physical =
                        usize::try_from(size.ok_or_else(|| error(FileStoreFailure::Io))?)
                            .map_err(|_| error(FileStoreFailure::Oversized))?;
                    let bound = physical
                        .min(owner.options.max_checkpoint_size)
                        .min(limit.saturating_sub(8));
                    let memory = decode_budget(owner.options.max_checkpoint_size, bound);
                    budget.grow(bound.saturating_mul(4).saturating_add(memory))?;
                    let result = futures_executor::block_on(
                        owner
                            .bridge
                            .get()
                            .ok_or_else(|| error(FileStoreFailure::Closed))?
                            .load_with_limit(key, bound.saturating_add(8)),
                    )
                    .map_err(from_store)?;
                    Ok(result)
                })
                .map_err(FileStoreError::store)?;
            wait_owned(op).await.map_err(FileStoreError::store)
        })
    }
    fn save(&self, key: SessionStoreKey, checkpoint: SessionCheckpoint) -> StoreFuture<()> {
        if checkpoint.0.len() > self.0.0.options.max_checkpoint_size + 8 {
            return Box::pin(async { Err(StoreFailure::Oversized) });
        }
        let store = self.0.clone();
        Box::pin(async move {
            let op = store
                .submit(
                    Some(key.clone()),
                    false,
                    false,
                    checkpoint
                        .0
                        .len()
                        .saturating_mul(4)
                        .saturating_add(decode_budget(
                            store.0.options.max_checkpoint_size + 8,
                            checkpoint.0.len(),
                        )),
                    move |owner| {
                        if checkpoint.0.len() > owner.options.max_checkpoint_size + 8 {
                            return Err(error(FileStoreFailure::Oversized));
                        }
                        futures_executor::block_on(
                            owner
                                .bridge
                                .get()
                                .ok_or_else(|| error(FileStoreFailure::Closed))?
                                .save_with_limit(
                                    key,
                                    checkpoint,
                                    owner.options.max_checkpoint_size + 8,
                                ),
                        )
                        .map_err(from_store)
                    },
                )
                .map_err(FileStoreError::store)?;
            wait_owned(op).await.map_err(FileStoreError::store)
        })
    }
    fn save_with_limit(
        &self,
        key: SessionStoreKey,
        checkpoint: SessionCheckpoint,
        limit: usize,
    ) -> StoreFuture<()> {
        if checkpoint.0.len() > limit {
            return Box::pin(async { Err(StoreFailure::Oversized) });
        }
        self.save(key, checkpoint)
    }
    fn clear(&self, key: SessionStoreKey) -> StoreFuture<()> {
        let store = self.0.clone();
        Box::pin(async move {
            let op = store
                .submit(Some(key.clone()), false, false, 0, move |owner| {
                    futures_executor::block_on(
                        owner
                            .bridge
                            .get()
                            .ok_or_else(|| error(FileStoreFailure::Closed))?
                            .clear(key),
                    )
                    .map_err(from_store)
                })
                .map_err(FileStoreError::store)?;
            wait_owned(op).await.map_err(FileStoreError::store)
        })
    }
}
async fn wait_owned<T: Send + Sync + 'static>(op: FileStoreOperation<T>) -> Result<T> {
    loop {
        let notified = op.0.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(result) = op.poll() {
            lock(&op.0.value).take();
            return result.and_then(|v| {
                Arc::try_unwrap(v.0)
                    .map(|v| v.value)
                    .map_err(|_| error(FileStoreFailure::Panic))
            });
        }
        notified.await;
    }
}

#[cfg(test)]
#[path = "file_store/tests.rs"]
mod tests;
