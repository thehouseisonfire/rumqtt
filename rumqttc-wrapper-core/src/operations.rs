use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::completion::CompletionCell;
use crate::{Completion, Error, OperationId, Result};

#[derive(Clone, Default)]
pub(crate) struct OperationRegistry {
    cells: Arc<Mutex<HashMap<OperationId, Arc<CompletionCell>>>>,
}

impl OperationRegistry {
    pub(crate) fn insert(&self, cell: Arc<CompletionCell>) {
        self.cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cell.operation_id(), cell);
    }

    pub(crate) fn complete(&self, operation_id: OperationId, result: Result<Completion>) {
        if let Some(cell) = self
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
                .cells
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for cell in cells.into_values() {
            cell.complete(Err(error.clone()));
        }
    }

    pub(crate) fn cancel(&self, operation_id: OperationId) {
        self.cells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&operation_id);
    }
}
