//! Operator recovery uses native protocol transitions and retained, operation-local observation.
pub use rumqttc_core::session_recovery::{RecoveryPhase, RecoverySnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RecoveryFailure {
    Unavailable = 1,
    InProgress = 2,
    Interrupted = 3,
    Transition = 4,
    Persistence = 5,
    Establishment = 6,
}
