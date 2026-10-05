#[cfg(feature = "ordered-shutdown")]
use std::future::Future;
#[cfg(feature = "ordered-shutdown")]
use std::pin::Pin;
use std::time::{Duration, Instant};

/// Terminal native fence failure; delivery may remain ambiguous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum OrderedDisconnectFailure {
    Timeout = 1,
    Transport = 2,
    Protocol = 3,
    Persistence = 4,
    Publish = 5,
    SupersededByImmediate = 6,
    Superseded = 7,
    ReceiverTerminated = 8,
    SessionReset = 9,
    Redirected = 10,
    ReplayUnavailable = 11,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum OrderedShutdownPhase {
    Open = 0,
    AdmittedDrain = 1,
    Approaching = 2,
    Draining = 3,
    Flushing = 4,
    Completed = 5,
    TimedOut = 6,
    Failed = 7,
}

/// Cached native observation. The count excludes channels and in-flight work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedShutdownDiagnostics {
    pub phase: OrderedShutdownPhase,
    pub fence_sequence: Option<u64>,
    pub remaining_at_capture: Option<Duration>,
    pub local_queued_publishes: Option<usize>,
    pub captured_at: Instant,
}

#[cfg(feature = "ordered-shutdown")]
pub type OrderedNotice = Pin<Box<dyn Future<Output = crate::Result<()>> + Send>>;

#[cfg_attr(not(feature = "ordered-shutdown"), derive(Clone, Copy))]
pub struct OrderedAdmission {
    pub sequence: u64,
    pub deadline: Option<Instant>,
    #[cfg(feature = "ordered-shutdown")]
    pub notice: OrderedNotice,
}
