#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub(crate) enum Lifecycle {
    Open,
    Closing,
    Closed,
    ShutdownFailed,
    Failed,
}
