//! File-backed session stores for the MQTT 3.1.1 and MQTT 5 rumqttc clients.
//!
//! Protocol support is additive and opt-in: enable `v4`, `v5`, or both. Each
//! adapter retains its protocol-specific on-disk namespace and key tag.

mod shared;

pub use rumqttc_session_store_file_core_next::{
    CheckpointInspection, CheckpointState, CleanupReport, FileStoreOptions, QuarantineInfo,
};
pub use shared::KeyEncodeError;

#[cfg(feature = "v4")]
pub mod v4;
#[cfg(feature = "v5")]
pub mod v5;
