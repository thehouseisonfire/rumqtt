use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use flume::Sender;
use tokio::sync::{Notify, futures::Notified};

use crate::operations::OperationRegistry;
use crate::{Admission, DeliveryStatus, Error, ErrorKind, OperationId, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecycleState {
    Running = 0,
    Closing = 1,
    Closed = 2,
    Failed = 3,
}

impl LifecycleState {
    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Closing,
            2 => Self::Closed,
            _ => Self::Failed,
        }
    }
}

/// One coherent snapshot of a committed shutdown transaction.
#[derive(Clone)]
enum ShutdownRecord {
    Running,
    Graceful { operation_id: OperationId },
    Immediate { operation_id: Option<OperationId> },
    Closed { outcome: ClosedOutcome },
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmediateAdmission {
    StartClosing,
    EscalateGraceful,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollErrorAction {
    Reconnect,
    Fail,
    CompleteImmediateClose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosedOutcome {
    Graceful,
    Immediate,
}

pub struct ShutdownCoordinator {
    lifecycle: AtomicU8,
    phase: AtomicU8,
    record: Mutex<ShutdownRecord>,
    operations: OperationRegistry,
    immediate_tx: Sender<()>,
    progress: Notify,
}

impl ShutdownCoordinator {
    pub(crate) fn new(operations: OperationRegistry, immediate_tx: Sender<()>) -> Arc<Self> {
        Arc::new(Self {
            lifecycle: AtomicU8::new(LifecycleState::Running as u8),
            phase: AtomicU8::new(0),
            record: Mutex::new(ShutdownRecord::Running),
            operations,
            immediate_tx,
            progress: Notify::new(),
        })
    }

    pub(crate) fn state(&self) -> LifecycleState {
        LifecycleState::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    pub(crate) fn require_running(&self) -> Result<()> {
        if self.state() == LifecycleState::Running {
            Ok(())
        } else {
            Err(
                Error::new(ErrorKind::Shutdown, "client is closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted),
            )
        }
    }

    pub(crate) fn transition_to_closing(&self) -> Result<()> {
        self.lifecycle
            .compare_exchange(
                LifecycleState::Running as u8,
                LifecycleState::Closing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| self.progress.notify_waiters())
            .map_err(|_| {
                Error::new(ErrorKind::Shutdown, "client is already closing or closed")
                    .with_delivery(DeliveryStatus::NotAdmitted)
            })
    }

    pub(crate) fn restore_running(&self) {
        _ = self.lifecycle.compare_exchange(
            LifecycleState::Closing as u8,
            LifecycleState::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn graceful_admission_allowed(&self) -> bool {
        matches!(
            &*self
                .record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ShutdownRecord::Running
        )
    }

    pub(crate) fn immediate_admission(&self) -> Option<ImmediateAdmission> {
        match &*self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ShutdownRecord::Running => Some(ImmediateAdmission::StartClosing),
            ShutdownRecord::Graceful { .. } => Some(ImmediateAdmission::EscalateGraceful),
            ShutdownRecord::Immediate { .. }
            | ShutdownRecord::Closed { .. }
            | ShutdownRecord::Failed => None,
        }
    }

    pub(crate) fn commit_graceful(&self, admission: &Admission) {
        *self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ShutdownRecord::Graceful {
            operation_id: admission.operation_id,
        };
        self.phase.store(1, Ordering::Release);
    }

    pub(crate) fn commit_immediate(&self, admission: Option<&Admission>) {
        let mut record = self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::mem::replace(
            &mut *record,
            ShutdownRecord::Immediate {
                operation_id: admission.map(|value| value.operation_id),
            },
        );
        drop(record);
        if let ShutdownRecord::Graceful { operation_id } = previous {
            self.operations.complete(
                operation_id,
                Err(Error::new(
                    ErrorKind::Shutdown,
                    "the requested graceful shutdown was escalated to immediate shutdown",
                )
                .with_delivery(DeliveryStatus::Ambiguous)),
            );
        }
        self.phase.store(2, Ordering::Release);
        _ = self.immediate_tx.send(());
    }

    pub(crate) fn timeout_graceful(&self, error: Error) -> bool {
        let operation_id = {
            let mut record = self
                .record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ShutdownRecord::Graceful { operation_id } = *record else {
                return false;
            };
            *record = ShutdownRecord::Immediate { operation_id: None };
            operation_id
        };
        self.operations.complete(operation_id, Err(error));
        self.phase.store(2, Ordering::Release);
        self.progress.notify_waiters();
        _ = self.immediate_tx.send(());
        true
    }

    pub(crate) fn poll_error_action(&self) -> PollErrorAction {
        match &*self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ShutdownRecord::Running => PollErrorAction::Reconnect,
            ShutdownRecord::Graceful { .. } => PollErrorAction::Fail,
            ShutdownRecord::Immediate { .. } => PollErrorAction::CompleteImmediateClose,
            ShutdownRecord::Closed { .. } | ShutdownRecord::Failed => PollErrorAction::Fail,
        }
    }

    pub(crate) fn should_drain_admitted_work(&self) -> bool {
        matches!(
            &*self
                .record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ShutdownRecord::Graceful { .. }
        )
    }

    pub(crate) fn immediate_requested(&self) -> bool {
        self.phase.load(Ordering::Acquire) == 2
    }

    pub(crate) fn reconcile_closed(&self) -> ClosedOutcome {
        let (outcome, operation) = {
            let mut record = self
                .record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (outcome, operation) = match &*record {
                ShutdownRecord::Graceful { operation_id } => (
                    ClosedOutcome::Graceful,
                    Some((*operation_id, crate::Completion::GracefulShutdown)),
                ),
                ShutdownRecord::Immediate { operation_id } => (
                    ClosedOutcome::Immediate,
                    operation_id.map(|id| (id, crate::Completion::ImmediateShutdown)),
                ),
                ShutdownRecord::Running => (ClosedOutcome::Immediate, None),
                ShutdownRecord::Closed { outcome } => return *outcome,
                ShutdownRecord::Failed => return ClosedOutcome::Immediate,
            };
            *record = ShutdownRecord::Closed { outcome };
            (outcome, operation)
        };
        if let Some((operation_id, completion)) = operation {
            self.operations.complete(operation_id, Ok(completion));
        }
        self.lifecycle
            .store(LifecycleState::Closed as u8, Ordering::Release);
        self.progress.notify_waiters();
        outcome
    }

    pub(crate) fn reconcile_failed(&self, error: Error) {
        let operation_id = {
            let mut record = self
                .record
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let operation_id = match &*record {
                ShutdownRecord::Graceful { operation_id } => Some(*operation_id),
                ShutdownRecord::Immediate { operation_id } => *operation_id,
                ShutdownRecord::Running => None,
                ShutdownRecord::Closed { .. } | ShutdownRecord::Failed => return,
            };
            *record = ShutdownRecord::Failed;
            operation_id
        };
        if let Some(operation_id) = operation_id {
            self.operations.complete(operation_id, Err(error));
        }
        self.lifecycle
            .store(LifecycleState::Failed as u8, Ordering::Release);
        self.progress.notify_waiters();
    }

    pub(crate) fn notify_progress(&self) {
        self.progress.notify_waiters();
    }

    pub(crate) fn notified(&self) -> Notified<'_> {
        self.progress.notified()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinator() -> (Arc<ShutdownCoordinator>, OperationRegistry) {
        let (operations, _) = OperationRegistry::new(1);
        let (immediate_tx, _) = flume::unbounded();
        (
            ShutdownCoordinator::new(operations.clone(), immediate_tx),
            operations,
        )
    }

    #[tokio::test]
    async fn graceful_reconciliation_resolves_once_and_notifies_waiters() {
        let (shutdown, operations) = coordinator();
        let admission = operations.allocate().unwrap();
        shutdown.transition_to_closing().unwrap();
        shutdown.commit_graceful(&admission);
        assert!(shutdown.should_drain_admitted_work());
        assert_eq!(shutdown.poll_error_action(), PollErrorAction::Fail);

        let progress = shutdown.notified();
        tokio::pin!(progress);
        progress.as_mut().enable();
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Graceful);
        tokio::time::timeout(std::time::Duration::ZERO, progress)
            .await
            .unwrap();

        assert_eq!(
            admission.completion.wait().unwrap(),
            crate::Completion::GracefulShutdown
        );
        assert_eq!(shutdown.state(), LifecycleState::Closed);
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Graceful);
        assert_eq!(
            admission.completion.wait().unwrap(),
            crate::Completion::GracefulShutdown
        );
        assert!(shutdown.transition_to_closing().is_err());
    }

    #[test]
    fn immediate_reconciliation_resolves_the_committed_operation() {
        let (shutdown, operations) = coordinator();
        let admission = operations.allocate().unwrap();
        shutdown.transition_to_closing().unwrap();
        shutdown.commit_immediate(Some(&admission));

        assert!(shutdown.immediate_requested());
        assert!(!shutdown.should_drain_admitted_work());
        assert_eq!(
            shutdown.poll_error_action(),
            PollErrorAction::CompleteImmediateClose
        );
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Immediate);
        assert_eq!(
            admission.completion.wait().unwrap(),
            crate::Completion::ImmediateShutdown
        );
    }

    #[test]
    fn graceful_timeout_fails_the_operation_and_reconciles_as_immediate() {
        let (shutdown, operations) = coordinator();
        let admission = operations.allocate().unwrap();
        shutdown.transition_to_closing().unwrap();
        shutdown.commit_graceful(&admission);
        let error = Error::new(ErrorKind::Timeout, "graceful shutdown timed out")
            .with_delivery(DeliveryStatus::Ambiguous);

        assert!(shutdown.timeout_graceful(error));
        assert!(shutdown.immediate_requested());
        assert!(!shutdown.should_drain_admitted_work());
        assert_eq!(
            admission.completion.wait().unwrap_err().kind(),
            ErrorKind::Timeout
        );
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Immediate);
        assert_eq!(shutdown.state(), LifecycleState::Closed);
    }

    #[test]
    fn immediate_escalation_supersedes_graceful_reconciliation() {
        let (shutdown, operations) = coordinator();
        let graceful = operations.allocate().unwrap();
        shutdown.transition_to_closing().unwrap();
        shutdown.commit_graceful(&graceful);
        let immediate = operations.allocate().unwrap();

        shutdown.commit_immediate(Some(&immediate));

        let graceful_error = graceful.completion.wait().unwrap_err();
        assert_eq!(graceful_error.kind(), ErrorKind::Shutdown);
        assert_eq!(graceful_error.delivery_status(), DeliveryStatus::Ambiguous);
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Immediate);
        assert_eq!(
            immediate.completion.wait().unwrap(),
            crate::Completion::ImmediateShutdown
        );
    }

    #[test]
    fn failed_reconciliation_fails_the_shutdown_operation_once() {
        let (shutdown, operations) = coordinator();
        let admission = operations.allocate().unwrap();
        shutdown.transition_to_closing().unwrap();
        shutdown.commit_graceful(&admission);
        let error = Error::new(ErrorKind::Network, "connection failed")
            .with_delivery(DeliveryStatus::Ambiguous);

        shutdown.reconcile_failed(error);
        shutdown.reconcile_failed(Error::new(ErrorKind::Internal, "duplicate failure"));

        let result = admission.completion.wait().unwrap_err();
        assert_eq!(result.kind(), ErrorKind::Network);
        assert_eq!(shutdown.state(), LifecycleState::Failed);
        assert_eq!(shutdown.reconcile_closed(), ClosedOutcome::Immediate);
        assert_eq!(shutdown.state(), LifecycleState::Failed);
    }
}
