use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use flume::{Receiver, Sender};
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::completion::CompletionCell;
use crate::{
    Admission, Completion, CompletionHandle, DeliveryStatus, DiagnosticsSnapshot, Error, ErrorKind,
    OperationId, Result, TerminalOutcome,
};

pub type CompletionFuture = Pin<Box<dyn Future<Output = TerminalOutcome> + Send + 'static>>;
pub type PendingFuture = Pin<Box<dyn Future<Output = (OperationId, TerminalOutcome)> + Send>>;

pub struct CompletionRegistration {
    operation_id: OperationId,
    registry: Weak<Inner>,
    future: CompletionFuture,
}

pub struct DiagnosticsRequest {
    operation_id: OperationId,
    registry: Weak<Inner>,
}

impl DiagnosticsRequest {
    pub(crate) fn resolve(self, snapshot: DiagnosticsSnapshot) {
        if let Some(inner) = self.registry.upgrade() {
            OperationRegistry { inner }
                .complete(self.operation_id, Ok(Completion::Diagnostics(snapshot)));
        }
    }
}

pub struct PendingSender {
    registry: OperationRegistry,
}

pub struct OperationReceivers {
    completions: Receiver<CompletionRegistration>,
    diagnostics: Receiver<DiagnosticsRequest>,
}

impl OperationReceivers {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Receiver<CompletionRegistration>,
        Receiver<DiagnosticsRequest>,
    ) {
        (self.completions, self.diagnostics)
    }
}

#[derive(Clone)]
pub struct OperationRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    next: AtomicU64,
    cells: Mutex<HashMap<OperationId, Arc<CompletionCell>>>,
    completion_tx: Sender<CompletionRegistration>,
    diagnostics_tx: Sender<DiagnosticsRequest>,
}

impl OperationRegistry {
    pub(crate) fn new(diagnostics_capacity: usize) -> (Self, OperationReceivers) {
        let (completion_tx, completions) = flume::unbounded();
        let (diagnostics_tx, diagnostics) = flume::bounded(diagnostics_capacity);
        (
            Self {
                inner: Arc::new(Inner {
                    next: AtomicU64::new(1),
                    cells: Mutex::new(HashMap::new()),
                    completion_tx,
                    diagnostics_tx,
                }),
            },
            OperationReceivers {
                completions,
                diagnostics,
            },
        )
    }

    fn next_id(&self) -> Result<OperationId> {
        #[allow(deprecated)]
        let value = self
            .inner
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1).filter(|next| *next != 0)
            })
            .map_err(|_| Error::new(ErrorKind::Internal, "operation identifier space exhausted"))?;
        Ok(OperationId(
            NonZeroU64::new(value).expect("operation IDs start at one"),
        ))
    }

    pub(crate) fn allocate(&self) -> Result<Admission> {
        let operation_id = self.next_id()?;
        let cell = CompletionCell::new(operation_id);
        self.inner
            .cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(operation_id, Arc::clone(&cell));
        Ok(Admission {
            operation_id,
            completion: CompletionHandle::new(cell),
        })
    }

    pub(crate) fn register(&self, future: CompletionFuture) -> Result<Admission> {
        let admission = self.allocate()?;
        let operation_id = admission.operation_id;
        self.inner
            .completion_tx
            .send(CompletionRegistration {
                operation_id,
                // The registry owns the sender, so queued work must not own it back.
                registry: Arc::downgrade(&self.inner),
                future,
            })
            .map_err(|_| {
                let error = Error::new(ErrorKind::Shutdown, "driver stopped during admission")
                    .with_delivery(DeliveryStatus::Ambiguous);
                self.complete(operation_id, Err(error.clone()));
                error
            })?;
        Ok(admission)
    }

    pub(crate) fn register_diagnostics(&self) -> Result<Admission> {
        let admission = self.allocate()?;
        let operation_id = admission.operation_id;
        self.inner
            .diagnostics_tx
            .try_send(DiagnosticsRequest {
                operation_id,
                registry: Arc::downgrade(&self.inner),
            })
            .map_err(|error| {
                let error = match error {
                    flume::TrySendError::Full(_) => Error::new(
                        ErrorKind::Backpressure,
                        "diagnostics request channel is full",
                    ),
                    flume::TrySendError::Disconnected(_) => {
                        Error::new(ErrorKind::Shutdown, "driver is not running")
                    }
                }
                .with_delivery(DeliveryStatus::NotAdmitted);
                self.complete(operation_id, Err(error.clone()));
                error
            })?;
        Ok(admission)
    }

    pub(crate) fn complete(&self, operation_id: OperationId, result: Result<Completion>) {
        self.complete_outcome(operation_id, result.into());
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(name = "mqtt.wrapper.complete", skip_all, fields(operation_id = ?operation_id, success = outcome.result().is_ok())))]
    pub(crate) fn complete_outcome(&self, operation_id: OperationId, outcome: TerminalOutcome) {
        let cell = self
            .inner
            .cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&operation_id);
        if let Some(cell) = cell {
            cell.complete_outcome(outcome.map_error(|error| error.with_operation(operation_id)));
        }
    }

    pub(crate) fn fail_all(&self, error: &Error) {
        let cells = std::mem::take(
            &mut *self
                .inner
                .cells
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (operation_id, cell) in cells {
            cell.complete(Err(error.clone().with_operation(operation_id)));
        }
    }

    pub(crate) fn cancel(&self, operation_id: OperationId) {
        self.inner
            .cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&operation_id);
    }
}

pub fn accept_registration(
    registration: CompletionRegistration,
    pending: &FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    let CompletionRegistration {
        operation_id,
        registry,
        future,
    } = registration;
    let Some(inner) = registry.upgrade() else {
        return;
    };
    let registry = OperationRegistry { inner };
    senders.insert(operation_id, PendingSender { registry });
    pending.push(Box::pin(async move { (operation_id, future.await) }));
}

pub fn resolve_pending(
    (operation_id, outcome): (OperationId, TerminalOutcome),
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    if let Some(sender) = senders.remove(&operation_id) {
        sender.registry.complete_outcome(operation_id, outcome);
    }
}

pub async fn drain_pending(
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    while let Some(result) = pending.next().await {
        resolve_pending(result, senders);
    }
}

pub fn complete_queued_diagnostics(
    diagnostics: &Receiver<DiagnosticsRequest>,
    snapshot: &DiagnosticsSnapshot,
) {
    while let Ok(request) = diagnostics.try_recv() {
        request.resolve(snapshot.clone());
    }
}

pub fn fail_pending(senders: &mut HashMap<OperationId, PendingSender>, error: &Error) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (operation_id, sender) in pending {
        sender.registry.complete(
            operation_id,
            Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)),
        );
    }
}

pub fn fail_unfinished(senders: &mut HashMap<OperationId, PendingSender>) {
    let error = Error::new(
        ErrorKind::Shutdown,
        "driver closed before the operation reported a terminal MQTT result",
    )
    .with_delivery(DeliveryStatus::Ambiguous);
    fail_pending(senders, &error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_is_resolved_exactly_once() {
        let (registry, _) = OperationRegistry::new(1);
        let admission = registry.allocate().unwrap();
        registry.complete(admission.operation_id, Ok(Completion::Acknowledged));
        registry.complete(
            admission.operation_id,
            Err(Error::new(ErrorKind::Internal, "duplicate completion")),
        );
        assert_eq!(
            admission.completion.wait().unwrap(),
            Completion::Acknowledged
        );
    }

    #[test]
    fn queued_completion_does_not_keep_its_registry_or_future_alive_after_teardown() {
        struct DropGuard(Arc<AtomicU64>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let (registry, receivers) = OperationRegistry::new(1);
        let weak = Arc::downgrade(&registry.inner);
        let dropped = Arc::new(AtomicU64::new(0));
        let guard = DropGuard(Arc::clone(&dropped));
        let admission = registry
            .register(Box::pin(async move {
                let _guard = guard;
                std::future::pending().await
            }))
            .unwrap();
        drop(receivers);
        drop(registry);
        assert!(weak.upgrade().is_none());
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        drop(admission);
    }

    #[test]
    fn queued_diagnostics_does_not_keep_its_registry_alive_after_teardown() {
        let (registry, receivers) = OperationRegistry::new(1);
        let weak = Arc::downgrade(&registry.inner);
        let admission = registry.register_diagnostics().unwrap();
        drop(receivers);
        drop(registry);
        assert!(weak.upgrade().is_none());
        drop(admission);
    }
}
