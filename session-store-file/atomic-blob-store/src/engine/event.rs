use super::*;

#[cfg(any(unix, windows))]
pub(crate) struct Submission {
    pub(crate) key_hash: [u8; 32],
    pub(crate) operation: Operation,
    pub(crate) completion_sender: Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
pub(crate) struct QueuedOperation {
    pub(crate) operation: Operation,
    pub(crate) completion_sender: Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
pub(crate) struct Completion {
    pub(crate) key_hash: [u8; 32],
    pub(crate) outcome: Option<(Operation, BlockingResult)>,
}

#[cfg(any(unix, windows))]
pub(crate) enum CoordinatorEvent {
    Submission(Submission),
    Completion(Completion),
    Maintenance(MaintenanceSubmission),
    MaintenanceCompletion(MaintenanceCompletion),
    Flush(Sender<Result<(), AtomicBlobStoreError>>),
    Close(CloseSubmission),
}

#[cfg(any(unix, windows))]
pub(crate) struct MaintenanceSubmission {
    pub(crate) minimum_age: Option<Duration>,
    pub(crate) sender: Sender<Result<CleanupReport, AtomicBlobStoreError>>,
    pub(crate) completion_sender: Sender<CoordinatorEvent>,
}

#[cfg(any(unix, windows))]
pub(crate) struct CloseSubmission {
    pub(crate) sender: Sender<Result<(), AtomicBlobStoreError>>,
}

#[cfg(any(unix, windows))]
pub(crate) struct MaintenanceCompletion {
    pub(crate) outcome: Option<MaintenanceOutcome>,
}

#[cfg(any(unix, windows))]
pub(crate) enum PendingEvent {
    Submission(Submission),
    Maintenance(MaintenanceSubmission),
    Flush(Sender<Result<(), AtomicBlobStoreError>>),
    Close(CloseSubmission),
}

#[cfg(any(unix, windows))]
pub(crate) type MaintenanceOutcome = (
    Sender<Result<CleanupReport, AtomicBlobStoreError>>,
    Result<CleanupReport, AtomicBlobStoreError>,
);
