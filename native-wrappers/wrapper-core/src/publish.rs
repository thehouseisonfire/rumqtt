//! MQTT 5 publish admission and process-local resource policy.

/// Where negotiated broker capabilities are checked. Fixed at client construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PublishAdmissionPolicy {
    #[default]
    RequireNegotiatedCapabilities,
    EventLoopValidated,
}

/// Finite retained publish limits, independent of request-channel capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishBudgetLimits {
    pub max_outstanding: usize,
    pub max_bytes: usize,
}

impl Default for PublishBudgetLimits {
    fn default() -> Self {
        Self {
            max_outstanding: 1024,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Coherent live usage; observers and queue placement do not own these counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishBudgetSnapshot {
    pub outstanding: usize,
    pub retained_bytes: usize,
    pub limits: PublishBudgetLimits,
    pub recovery_pending: bool,
}

/// Stable local publish failure details. Broker ACK reasons remain separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PublishFailure {
    CapabilitiesPending = 1,
    RecoveryPending = 2,
    RequestChannelFull = 3,
    CountExhausted = 4,
    BytesExhausted = 5,
    TooLarge = 6,
    RetainUnavailable = 7,
    MaximumQos = 8,
    TopicAliasZero = 9,
    TopicAliasMaximum = 10,
    TopicAliasUnmapped = 11,
    TopicAliasReplayUnavailable = 12,
    SessionReset = 13,
    Redirected = 14,
    BrokerOnlySessionResume = 15,
    Qos0NotFlushed = 16,
    Persistence = 17,
    ReceiverTerminated = 18,
    RestoreBudgetExceeded = 19,
}
