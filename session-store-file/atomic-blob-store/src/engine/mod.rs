use super::*;

#[cfg(any(unix, windows))]
mod event;
#[cfg(any(unix, windows))]
use event::*;
mod lifecycle;
pub(crate) use lifecycle::Lifecycle;
mod operation;
pub(crate) use operation::SaveStreamMessage;
#[cfg(any(unix, windows))]
use operation::*;
#[cfg(any(unix, windows))]
mod scheduler;
#[cfg(any(unix, windows))]
use scheduler::{fail_queued_after_coordinator_panic, run_scheduler};
mod stream;
pub(crate) use stream::{LoadStreamEndpoint, SaveStreamEndpoint};
#[cfg(any(unix, windows))]
mod workers;
#[cfg(any(unix, windows))]
use workers::*;

type PendingCompletion<T> = Box<
    dyn FnOnce(Option<Result<T, AtomicBlobStoreError>>) -> Result<T, AtomicBlobStoreError> + Send,
>;

pub(crate) struct Pending<T> {
    receiver: Receiver<Result<T, AtomicBlobStoreError>>,
    completion: PendingCompletion<T>,
}

impl<T: Send + 'static> Pending<T> {
    fn new(receiver: Receiver<Result<T, AtomicBlobStoreError>>) -> Self {
        Self {
            receiver,
            completion: Box::new(|result| {
                result.unwrap_or(Err(AtomicBlobStoreError::EngineFailed))
            }),
        }
    }

    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    fn with_disconnected(
        receiver: Receiver<Result<T, AtomicBlobStoreError>>,
        disconnected: impl FnOnce() -> Result<T, AtomicBlobStoreError> + Send + 'static,
    ) -> Self {
        Self {
            receiver,
            completion: Box::new(move |result| result.unwrap_or_else(disconnected)),
        }
    }

    #[cfg(any(unix, windows))]
    fn with_after(
        mut self,
        after: impl FnOnce(Result<T, AtomicBlobStoreError>) -> Result<T, AtomicBlobStoreError>
        + Send
        + 'static,
    ) -> Self {
        let completion = self.completion;
        self.completion = Box::new(move |result| after(completion(result)));
        self
    }

    fn resolved(result: Result<T, AtomicBlobStoreError>) -> Self {
        let (sender, receiver) = flume::bounded(1);
        let _ = sender.send(result);
        Self::new(receiver)
    }

    pub(crate) fn wait(self) -> Result<T, AtomicBlobStoreError> {
        (self.completion)(self.receiver.recv().ok())
    }

    #[allow(dead_code)]
    pub(crate) async fn wait_async(self) -> Result<T, AtomicBlobStoreError> {
        (self.completion)(self.receiver.recv_async().await.ok())
    }
}

#[derive(Clone)]
pub(crate) struct EngineHandle {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for EngineHandle {
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

pub(crate) struct Inner {
    pub(crate) config: Arc<StoreConfig>,
    #[cfg(any(unix, windows))]
    pub(crate) submissions: Sender<CoordinatorEvent>,
    pub(crate) lifecycle: Arc<Mutex<Lifecycle>>,
    #[cfg(any(unix, windows))]
    pub(crate) coordinator: Arc<Mutex<CoordinatorJoin>>,
    #[cfg(all(test, any(unix, windows)))]
    #[allow(dead_code)]
    pub(crate) registry_entries: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(any(unix, windows))]
pub(crate) struct CoordinatorJoin {
    handle: Option<std::thread::JoinHandle<()>>,
    outcome: Option<CoordinatorJoinOutcome>,
}

#[cfg(any(unix, windows))]
#[derive(Clone, Copy)]
enum CoordinatorJoinOutcome {
    Joined,
    Panicked,
}

#[cfg(any(unix, windows))]
impl CoordinatorJoin {
    fn new(handle: std::thread::JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
            outcome: None,
        }
    }

    fn finish(
        &mut self,
        result: Result<(), AtomicBlobStoreError>,
    ) -> Result<(), AtomicBlobStoreError> {
        let outcome = match self.outcome {
            Some(outcome) => outcome,
            None => {
                let handle = self
                    .handle
                    .take()
                    .expect("an unfinished coordinator has a join handle");
                let outcome = if handle.join().is_ok() {
                    CoordinatorJoinOutcome::Joined
                } else {
                    CoordinatorJoinOutcome::Panicked
                };
                self.outcome = Some(outcome);
                outcome
            }
        };
        match outcome {
            CoordinatorJoinOutcome::Joined => result,
            CoordinatorJoinOutcome::Panicked => Err(AtomicBlobStoreError::ShutdownFailure),
        }
    }
}

pub(crate) struct StoreConfig {
    pub(crate) namespace: PathBuf,
    pub(crate) format: BlobFormatIdentity,
    pub(crate) maximum: u64,
    pub(crate) max_concurrent_operations: usize,
    #[cfg(all(test, any(unix, windows)))]
    pub(crate) hook: Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
    #[cfg(feature = "bench-instrumentation")]
    pub(crate) benchmark_events:
        Option<std::sync::mpsc::Sender<crate::bench_instrumentation::BenchmarkEvent>>,
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

impl EngineHandle {
    fn ensure_open(&self) -> Result<(), AtomicBlobStoreError> {
        match *self
            .inner
            .lifecycle
            .lock()
            .expect("lifecycle lock poisoned")
        {
            Lifecycle::Open => Ok(()),
            Lifecycle::Closing | Lifecycle::Closed | Lifecycle::ShutdownFailed => {
                Err(AtomicBlobStoreError::StoreClosed)
            }
            Lifecycle::Failed => Err(AtomicBlobStoreError::EngineFailed),
        }
    }

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
    pub(crate) fn open(
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
            let config =
                initialize_platform(root, namespace, format, maximum, max_concurrent_operations)?;
            Self::from_config(config)
        }
    }

    #[cfg(any(unix, windows))]
    fn from_config(config: StoreConfig) -> Result<Self, AtomicBlobStoreError> {
        let config = Arc::new(config);
        let (submissions, receiver) = flume::unbounded();
        let lifecycle = Arc::new(Mutex::new(Lifecycle::Open));
        #[cfg(all(test, any(unix, windows)))]
        let registry_entries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let scheduler_config = Arc::clone(&config);
        let scheduler_lifecycle = Arc::clone(&lifecycle);
        let worker_pool = WorkerPool::new(
            config.max_concurrent_operations,
            #[cfg(all(test, any(unix, windows)))]
            config.hook.clone(),
        )?;
        #[cfg(all(test, any(unix, windows)))]
        let scheduler_registry_entries = Arc::clone(&registry_entries);
        let coordinator = std::thread::Builder::new()
            .name("atomic-blob-store-coordinator".into())
            .spawn(move || {
                #[cfg(all(test, any(unix, windows)))]
                struct CoordinatorExitNotification(
                    Option<Arc<dyn Fn(TestStage) -> io::Result<()> + Send + Sync>>,
                );
                #[cfg(all(test, any(unix, windows)))]
                impl Drop for CoordinatorExitNotification {
                    fn drop(&mut self) {
                        if let Some(hook) = &self.0 {
                            let _ = hook(TestStage::CoordinatorStopped);
                        }
                    }
                }
                #[cfg(all(test, any(unix, windows)))]
                let _exit_notification = CoordinatorExitNotification(scheduler_config.hook.clone());
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_scheduler(
                        &scheduler_config,
                        &receiver,
                        &scheduler_lifecycle,
                        worker_pool,
                        #[cfg(all(test, any(unix, windows)))]
                        &scheduler_registry_entries,
                    );
                }));
                if outcome.is_err() {
                    *scheduler_lifecycle.lock().expect("lifecycle lock poisoned") =
                        Lifecycle::Failed;
                    fail_queued_after_coordinator_panic(&receiver);
                }
            })
            .map_err(|source| AtomicBlobStoreError::Io {
                operation: StoreOperation::StartCoordinator,
                source,
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                submissions,
                lifecycle,
                coordinator: Arc::new(Mutex::new(CoordinatorJoin::new(coordinator))),
                #[cfg(all(test, any(unix, windows)))]
                registry_entries,
            }),
        })
    }

    #[cfg(all(test, any(unix, windows)))]
    #[allow(dead_code)]
    pub(crate) fn open_with_test_hook(
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
            initialize_platform(root, namespace, format, maximum, max_concurrent_operations)?;
        config.hook = Some(hook);
        Self::from_config(config)
    }

    #[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
    pub(crate) fn open_with_benchmark_events(
        root: impl Into<PathBuf>,
        namespace: impl AsRef<OsStr>,
        options: AtomicBlobStoreOptions,
        events: std::sync::mpsc::Sender<crate::bench_instrumentation::BenchmarkEvent>,
    ) -> Result<Self, AtomicBlobStoreError> {
        let root = root.into();
        let namespace = validate_namespace(namespace.as_ref())?;
        validate_maximum(options.max_blob_size)?;
        let maximum = options.max_blob_size;
        let format = options.format;
        let max_concurrent_operations = options.max_concurrent_operations.get();
        let mut config =
            initialize_platform(root, namespace, format, maximum, max_concurrent_operations)?;
        config.benchmark_events = Some(events);
        Self::from_config(config)
    }

    /// Allocates and loads the complete canonical payload for `canonical_key`.
    ///
    /// Only a genuinely absent canonical blob returns `Ok(None)`.
    #[must_use]
    pub(crate) fn load(&self, canonical_key: &[u8]) -> Pending<Option<Vec<u8>>> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            submit(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::Load { sender },
                receiver,
            )
        }
    }

    /// Saves an already allocated complete payload for `canonical_key`.
    #[must_use]
    pub(crate) fn save(&self, canonical_key: &[u8], payload: Vec<u8>) -> Pending<()> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (canonical_key, payload);
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            submit(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::Save { payload, sender },
                receiver,
            )
        }
    }

    pub(crate) fn start_save_stream(
        &self,
        canonical_key: &[u8],
        declared_len: u64,
    ) -> Result<SaveStreamEndpoint, AtomicBlobStoreError> {
        self.ensure_open()?;
        if declared_len > self.inner.config.maximum {
            return Err(AtomicBlobStoreError::BlobTooLarge {
                size: declared_len,
                maximum: self.inner.config.maximum,
            });
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }

        #[cfg(any(unix, windows))]
        {
            let (chunks_sender, chunks_receiver) = flume::bounded(STREAM_CHANNEL_CAPACITY);
            let (sender, receiver) = flume::bounded(1);
            submit_operation(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::SaveStream {
                    declared_len,
                    chunks: chunks_receiver,
                    sender,
                },
            )?;
            Ok(SaveStreamEndpoint {
                chunks: chunks_sender,
                result: Pending::new(receiver),
            })
        }
    }

    pub(crate) fn start_load_stream(
        &self,
        canonical_key: &[u8],
    ) -> Result<LoadStreamEndpoint, AtomicBlobStoreError> {
        self.ensure_open()?;
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return Err(AtomicBlobStoreError::UnsupportedPlatform {
                platform: std::env::consts::OS,
            });
        }

        #[cfg(any(unix, windows))]
        {
            let (chunks_sender, chunks_receiver) = flume::bounded(STREAM_CHANNEL_CAPACITY);
            let (acknowledgement_sender, acknowledgement_receiver) = flume::bounded(1);
            let (sender, receiver) = flume::bounded(1);
            submit_operation(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::LoadStream {
                    chunks: Some(chunks_sender),
                    acknowledgement: Some(acknowledgement_receiver),
                    sender,
                },
            )?;
            Ok(LoadStreamEndpoint {
                chunks: chunks_receiver,
                acknowledgement: acknowledgement_sender,
                result: Pending::new(receiver),
            })
        }
    }

    /// Clears the canonical blob for `canonical_key`.
    #[must_use]
    pub(crate) fn clear(&self, canonical_key: &[u8]) -> Pending<()> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            submit(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::Clear { sender },
                receiver,
            )
        }
    }

    /// Inspects metadata without opening, decoding, or mutating the blob.
    #[must_use]
    pub(crate) fn inspect(&self, canonical_key: &[u8]) -> Pending<BlobInspection> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            submit(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::Inspect { sender },
                receiver,
            )
        }
    }

    /// Atomically moves a canonical blob to a randomized diagnostic name.
    #[must_use]
    pub(crate) fn quarantine(&self, canonical_key: &[u8]) -> Pending<QuarantineInfo> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = canonical_key;
            unsupported_future()
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            submit(
                &self.inner.submissions,
                &self.inner.lifecycle,
                key_hash(canonical_key),
                Operation::Quarantine { sender },
                receiver,
            )
        }
    }

    /// Removes stale store-owned Windows staging files behind a store-wide FIFO barrier.
    #[must_use]
    pub(crate) fn cleanup_stale_temporary_files(
        &self,
        minimum_age: Duration,
    ) -> Pending<CleanupReport> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = minimum_age;
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            if minimum_age.is_zero() {
                return Pending::resolved(Err(AtomicBlobStoreError::InvalidCleanupAge));
            }
            let (sender, receiver) = flume::bounded(1);
            let state = self
                .inner
                .lifecycle
                .lock()
                .expect("lifecycle lock poisoned");
            match *state {
                Lifecycle::Open => {}
                Lifecycle::Failed => {
                    return Pending::resolved(Err(AtomicBlobStoreError::EngineFailed));
                }
                Lifecycle::Closing | Lifecycle::Closed | Lifecycle::ShutdownFailed => {
                    return Pending::resolved(Err(AtomicBlobStoreError::StoreClosed));
                }
            }
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
                return Pending::resolved(Err(
                    AtomicBlobStoreError::MaintenanceCoordinationFailure,
                ));
            }
            Pending::new(receiver)
        }
    }

    /// Waits until every operation submitted before this call has completed.
    ///
    /// Later submissions are not part of this barrier. Dropping the returned
    /// future discards only its result; the barrier remains ordered.
    #[must_use]
    pub(crate) fn flush(&self) -> Pending<()> {
        if let Err(error) = self.ensure_open() {
            return Pending::resolved(Err(error));
        }
        #[cfg(not(any(unix, windows)))]
        {
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            let state = self
                .inner
                .lifecycle
                .lock()
                .expect("lifecycle lock poisoned");
            match *state {
                Lifecycle::Open => {}
                Lifecycle::Failed => {
                    return Pending::resolved(Err(AtomicBlobStoreError::EngineFailed));
                }
                Lifecycle::Closing | Lifecycle::Closed | Lifecycle::ShutdownFailed => {
                    return Pending::resolved(Err(AtomicBlobStoreError::StoreClosed));
                }
            }
            if self
                .inner
                .submissions
                .send(CoordinatorEvent::Flush(sender))
                .is_err()
            {
                return Pending::resolved(Err(
                    AtomicBlobStoreError::MaintenanceCoordinationFailure,
                ));
            }
            Pending::new(receiver)
        }
    }

    #[must_use]
    pub(crate) fn close(&self) -> Pending<()> {
        let state = *self
            .inner
            .lifecycle
            .lock()
            .expect("lifecycle lock poisoned");
        match state {
            Lifecycle::Closed => return self.closed_pending(Ok(())),
            Lifecycle::ShutdownFailed | Lifecycle::Failed => {
                let error = AtomicBlobStoreError::ShutdownFailure;
                return self.closed_pending(Err(error));
            }
            Lifecycle::Open | Lifecycle::Closing => {}
        }
        #[cfg(not(any(unix, windows)))]
        {
            return unsupported_future();
        }
        #[cfg(any(unix, windows))]
        {
            let (sender, receiver) = flume::bounded(1);
            let state = self
                .inner
                .lifecycle
                .lock()
                .expect("lifecycle lock poisoned");
            match *state {
                Lifecycle::Closed => return self.closed_pending(Ok(())),
                Lifecycle::ShutdownFailed | Lifecycle::Failed => {
                    return self.closed_pending(Err(AtomicBlobStoreError::ShutdownFailure));
                }
                Lifecycle::Open | Lifecycle::Closing => {}
            }
            if self
                .inner
                .submissions
                .send(CoordinatorEvent::Close(CloseSubmission { sender }))
                .is_err()
            {
                let state = *self
                    .inner
                    .lifecycle
                    .lock()
                    .expect("lifecycle lock poisoned");
                return Pending::resolved(match state {
                    Lifecycle::Closed => Ok(()),
                    Lifecycle::ShutdownFailed | Lifecycle::Failed => {
                        Err(AtomicBlobStoreError::ShutdownFailure)
                    }
                    Lifecycle::Open | Lifecycle::Closing => Err(AtomicBlobStoreError::EngineFailed),
                });
            }
            let lifecycle = Arc::clone(&self.inner.lifecycle);
            let pending = Pending::with_disconnected(receiver, move || {
                match *lifecycle.lock().expect("lifecycle lock poisoned") {
                    Lifecycle::Closed => Ok(()),
                    Lifecycle::ShutdownFailed | Lifecycle::Failed => {
                        Err(AtomicBlobStoreError::ShutdownFailure)
                    }
                    Lifecycle::Open | Lifecycle::Closing => Err(AtomicBlobStoreError::EngineFailed),
                }
            });
            let coordinator = Arc::clone(&self.inner.coordinator);
            pending.with_after(move |result| {
                coordinator
                    .lock()
                    .expect("coordinator handle lock poisoned")
                    .finish(result)
            })
        }
    }

    #[cfg(any(unix, windows))]
    fn closed_pending(&self, result: Result<(), AtomicBlobStoreError>) -> Pending<()> {
        let coordinator = Arc::clone(&self.inner.coordinator);
        Pending::resolved(result).with_after(move |result| {
            coordinator
                .lock()
                .expect("coordinator handle lock poisoned")
                .finish(result)
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn closed_pending(&self, result: Result<(), AtomicBlobStoreError>) -> Pending<()> {
        Pending::resolved(result)
    }

    /// Returns a diagnostic path for an opaque key.
    ///
    /// This path is not a stable storage API and callers must not read, write,
    /// rename, or delete it while the store is in use.
    #[must_use]
    pub(crate) fn blob_path(&self, canonical_key: &[u8]) -> PathBuf {
        self.inner
            .config
            .namespace
            .join(blob_filename(&self.inner.config.format, canonical_key))
    }
}

pub(crate) fn validate_maximum(maximum: u64) -> Result<(), AtomicBlobStoreError> {
    let overhead =
        u64::try_from(HEADER_LEN + CHECKSUM_LEN).expect("the fixed envelope overhead fits in u64");
    if maximum.checked_add(overhead).is_none() {
        return Err(AtomicBlobStoreConfigError::InvalidMaximumBlobSize { maximum }.into());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn unsupported_future<T: Send + 'static>() -> Pending<T> {
    Pending::resolved(Err(AtomicBlobStoreError::UnsupportedPlatform {
        platform: std::env::consts::OS,
    }))
}

#[cfg(any(unix, windows))]
pub(crate) fn submit<T: Send + 'static>(
    submissions: &Sender<CoordinatorEvent>,
    lifecycle: &Mutex<Lifecycle>,
    key_hash: [u8; 32],
    operation: Operation,
    receiver: Receiver<Result<T, AtomicBlobStoreError>>,
) -> Pending<T> {
    let state = lifecycle.lock().expect("lifecycle lock poisoned");
    match *state {
        Lifecycle::Open => {}
        Lifecycle::Failed => {
            return Pending::resolved(Err(AtomicBlobStoreError::EngineFailed));
        }
        Lifecycle::Closing | Lifecycle::Closed | Lifecycle::ShutdownFailed => {
            return Pending::resolved(Err(AtomicBlobStoreError::StoreClosed));
        }
    }
    let submission = Submission {
        key_hash,
        operation,
        completion_sender: submissions.clone(),
    };
    if submissions
        .send(CoordinatorEvent::Submission(submission))
        .is_err()
    {
        return Pending::resolved(Err(AtomicBlobStoreError::EngineFailed));
    }
    Pending::new(receiver)
}

#[cfg(any(unix, windows))]
pub(crate) fn submit_operation(
    submissions: &Sender<CoordinatorEvent>,
    lifecycle: &Mutex<Lifecycle>,
    key_hash: [u8; 32],
    operation: Operation,
) -> Result<(), AtomicBlobStoreError> {
    let state = lifecycle.lock().expect("lifecycle lock poisoned");
    match *state {
        Lifecycle::Open => {}
        Lifecycle::Failed => return Err(AtomicBlobStoreError::EngineFailed),
        Lifecycle::Closing | Lifecycle::Closed | Lifecycle::ShutdownFailed => {
            return Err(AtomicBlobStoreError::StoreClosed);
        }
    }
    submissions
        .send(CoordinatorEvent::Submission(Submission {
            key_hash,
            operation,
            completion_sender: submissions.clone(),
        }))
        .map_err(|_| AtomicBlobStoreError::EngineFailed)
}

#[cfg(all(test, any(unix, windows)))]
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TestStage {
    CoordinatorEvent,
    FlushCompleted,
    CoordinatorStopping,
    WorkerStart,
    WorkerDispatch,
    WorkerExit,
    WorkerStopped,
    CoordinatorStopped,
    OperationStarted,
    MaintenanceStarted,
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
    BeforeCleanupMetadata,
    BeforeCleanupRemove,
    BeforeQuarantineRename,
    AfterQuarantineRename,
}

#[cfg(all(test, any(unix, windows)))]
pub(crate) fn hit_test_stage(
    config: &StoreConfig,
    stage: TestStage,
    operation: StoreOperation,
) -> Result<(), AtomicBlobStoreError> {
    let Some(hook) = &config.hook else {
        return Ok(());
    };
    hook(stage).map_err(|source| AtomicBlobStoreError::Io { operation, source })
}

#[cfg(all(feature = "bench-instrumentation", any(unix, windows)))]
pub(crate) fn emit_benchmark_event(
    config: &StoreConfig,
    event: crate::bench_instrumentation::BenchmarkEvent,
) {
    if let Some(events) = &config.benchmark_events {
        let _ = events.send(event);
    }
}
