use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender};
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::completion::CompletionCell;
use crate::{
    Admission, Completion, CompletionHandle, DeliveryStatus, DiagnosticsSnapshot, Error, ErrorKind,
    OperationId, Result,
};

pub(crate) type CompletionFuture =
    Pin<Box<dyn Future<Output = Result<Completion>> + Send + 'static>>;
pub(crate) type PendingFuture =
    Pin<Box<dyn Future<Output = (OperationId, Result<Completion>)> + Send>>;

pub(crate) struct CompletionRegistration {
    pub(crate) operation_id: OperationId,
    pub(crate) registry: OperationRegistry,
    pub(crate) future: CompletionFuture,
}

pub(crate) struct DiagnosticsRequest {
    pub(crate) operation_id: OperationId,
    pub(crate) registry: OperationRegistry,
}

pub(crate) struct PendingSender {
    pub(crate) registry: OperationRegistry,
}

pub(crate) struct OperationReceivers {
    pub(crate) completions: Receiver<CompletionRegistration>,
    pub(crate) diagnostics: Receiver<DiagnosticsRequest>,
}

#[derive(Clone)]
pub(crate) struct OperationRegistry {
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
                registry: self.clone(),
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
                registry: self.clone(),
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
        if let Some(cell) = self
            .inner
            .cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&operation_id)
        {
            cell.complete(result);
        }
    }

    pub(crate) fn fail_all(&self, error: Error) {
        let cells = std::mem::take(
            &mut *self
                .inner
                .cells
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for cell in cells.into_values() {
            cell.complete(Err(error.clone()));
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

pub(crate) fn accept_registration(
    registration: CompletionRegistration,
    pending: &FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    let CompletionRegistration {
        operation_id,
        registry,
        future,
    } = registration;
    senders.insert(operation_id, PendingSender { registry });
    pending.push(Box::pin(async move { (operation_id, future.await) }));
}

pub(crate) fn resolve_pending(
    (operation_id, result): (OperationId, Result<Completion>),
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    if let Some(sender) = senders.remove(&operation_id) {
        sender.registry.complete(operation_id, result);
    }
}

pub(crate) async fn drain_pending(
    pending: &mut FuturesUnordered<PendingFuture>,
    senders: &mut HashMap<OperationId, PendingSender>,
) {
    while let Some(result) = pending.next().await {
        resolve_pending(result, senders);
    }
}

pub(crate) fn complete_queued_diagnostics(
    diagnostics: &Receiver<DiagnosticsRequest>,
    snapshot: &DiagnosticsSnapshot,
) {
    while let Ok(request) = diagnostics.try_recv() {
        request.registry.complete(
            request.operation_id,
            Ok(Completion::Diagnostics(snapshot.clone())),
        );
    }
}

pub(crate) fn fail_pending(senders: &mut HashMap<OperationId, PendingSender>, error: &Error) {
    let mut pending: Vec<_> = senders.drain().collect();
    pending.sort_unstable_by_key(|(operation_id, _)| *operation_id);
    for (operation_id, sender) in pending {
        sender.registry.complete(
            operation_id,
            Err(error.clone().with_delivery(DeliveryStatus::Ambiguous)),
        );
    }
}

pub(crate) fn fail_unfinished(senders: &mut HashMap<OperationId, PendingSender>) {
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
}
