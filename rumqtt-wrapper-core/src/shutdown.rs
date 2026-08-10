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
