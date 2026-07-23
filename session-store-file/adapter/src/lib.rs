//! File-backed session stores for the MQTT 3.1.1 and MQTT 5 rumqttc clients.
//!
//! Protocol support is additive and opt-in: enable `v4`, `v5`, or both. Each
//! adapter retains its protocol-specific on-disk namespace and key tag.

mod shared;

use std::num::NonZeroUsize;
use std::time::SystemTime;

pub use atomic_blob_store::{CleanupReport, QuarantineInfo};
pub use shared::KeyEncodeError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointState {
    Absent,
    Present,
    LegacyDetected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointInspection {
    pub state: CheckpointState,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStoreOptions {
    pub max_checkpoint_size: u64,
    pub max_concurrent_operations: NonZeroUsize,
}

impl Default for FileStoreOptions {
    fn default() -> Self {
        Self {
            max_checkpoint_size: atomic_blob_store::DEFAULT_MAX_BLOB_SIZE,
            max_concurrent_operations: NonZeroUsize::new(
                atomic_blob_store::DEFAULT_MAX_CONCURRENT_OPERATIONS,
            )
            .expect("the core default is nonzero"),
        }
    }
}

#[cfg(feature = "v4")]
pub mod v4;
#[cfg(feature = "v5")]
pub mod v5;
