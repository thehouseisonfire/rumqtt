use std::sync::{LockResult, Mutex, MutexGuard};

/// Serializes request admission with connection-scoped invalidation and shutdown commitment.
#[derive(Default)]
pub(crate) struct AdmissionGate(Mutex<()>);

impl AdmissionGate {
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, ()>> {
        self.0.lock()
    }
}
