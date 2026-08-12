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
    Closed,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownIntent {
    Running,
    Graceful,
    Immediate,
    Terminal,
}

pub(crate) struct ShutdownDisposition {
    pub(crate) graceful: bool,
    pub(crate) completion: Option<(OperationId, crate::Completion)>,
}

pub(crate) struct ShutdownCoordinator {
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

    pub(crate) fn intent(&self) -> ShutdownIntent {
        match &*self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ShutdownRecord::Running => ShutdownIntent::Running,
            ShutdownRecord::Graceful { .. } => ShutdownIntent::Graceful,
            ShutdownRecord::Immediate { .. } => ShutdownIntent::Immediate,
            ShutdownRecord::Closed | ShutdownRecord::Failed => ShutdownIntent::Terminal,
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
        let previous = self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let ShutdownRecord::Graceful { operation_id, .. } = &previous {
            self.operations.complete(
                *operation_id,
                Err(Error::new(
                    ErrorKind::Shutdown,
                    "the requested graceful shutdown was escalated to immediate shutdown",
                )
                .with_delivery(DeliveryStatus::Ambiguous)),
            );
        }
        *self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ShutdownRecord::Immediate {
            operation_id: admission.map(|value| value.operation_id),
        };
        self.phase.store(2, Ordering::Release);
        _ = self.immediate_tx.send(());
    }

    pub(crate) fn disposition(&self) -> ShutdownDisposition {
        match self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            ShutdownRecord::Graceful { operation_id, .. } => ShutdownDisposition {
                graceful: true,
                completion: Some((operation_id, crate::Completion::GracefulShutdown)),
            },
            ShutdownRecord::Immediate { operation_id, .. } => ShutdownDisposition {
                graceful: false,
                completion: operation_id.map(|id| (id, crate::Completion::ImmediateShutdown)),
            },
            ShutdownRecord::Running | ShutdownRecord::Closed | ShutdownRecord::Failed => {
                ShutdownDisposition {
                    graceful: false,
                    completion: None,
                }
            }
        }
    }

    pub(crate) fn immediate_requested(&self) -> bool {
        self.phase.load(Ordering::Acquire) == 2
    }

    pub(crate) fn committed_kind(&self) -> Option<u8> {
        match &*self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            ShutdownRecord::Graceful { .. } => Some(1),
            ShutdownRecord::Immediate { .. } => Some(2),
            ShutdownRecord::Running | ShutdownRecord::Closed | ShutdownRecord::Failed => None,
        }
    }

    pub(crate) fn publish_terminal(&self, failed: bool) {
        let (lifecycle, record) = if failed {
            (LifecycleState::Failed, ShutdownRecord::Failed)
        } else {
            (LifecycleState::Closed, ShutdownRecord::Closed)
        };
        *self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = record;
        self.lifecycle.store(lifecycle as u8, Ordering::Release);
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

    #[test]
    fn terminal_transition_wakes_and_publishes_one_state() {
        let (operations, _) = OperationRegistry::new(1);
        let (immediate_tx, _) = flume::unbounded();
        let shutdown = ShutdownCoordinator::new(operations, immediate_tx);
        shutdown.transition_to_closing().unwrap();
        shutdown.publish_terminal(false);
        assert_eq!(shutdown.state(), LifecycleState::Closed);
        assert!(shutdown.transition_to_closing().is_err());
    }
}
