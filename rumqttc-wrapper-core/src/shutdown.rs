#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LifecycleState {
    Running = 0,
    Closing = 1,
    Closed = 2,
    Failed = 3,
}

impl LifecycleState {
    pub(crate) const fn from_u8(value: u8) -> Self {
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
pub(crate) enum ShutdownRecord {
    Running,
    Graceful {
        operation_id: OperationId,
        cell: Arc<CompletionCell>,
        timeout: Option<Duration>,
    },
    Immediate {
        operation_id: Option<OperationId>,
        cell: Option<Arc<CompletionCell>>,
        escalated: bool,
    },
    Closed,
    Failed,
}
use std::sync::Arc;
use std::time::Duration;

use crate::OperationId;
use crate::completion::CompletionCell;
