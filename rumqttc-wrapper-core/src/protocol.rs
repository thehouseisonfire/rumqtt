use std::num::NonZeroU64;

/// MQTT protocol version selected for a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolVersion {
    V311,
    V5,
}

/// Protocol-neutral quality of service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum QoS {
    #[default]
    AtMostOnce = 0,
    AtLeastOnce = 1,
    ExactlyOnce = 2,
}

/// Wrapper operation identity. It is independent from MQTT packet identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(pub(crate) NonZeroU64);

impl OperationId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}
