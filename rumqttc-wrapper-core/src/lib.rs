//! Owned, protocol-neutral support for native language wrappers.
//!
//! This crate is implementation infrastructure. It deliberately exposes Rust
//! values rather than a stable foreign-function ABI.

mod acknowledgement;
mod adapter;
mod command;
mod completion;
mod config;
mod error;
mod event;
mod handle;
mod operations;
mod protocol;
mod runtime;
mod shutdown;

pub use command::{
    Command, PublishCommand, PublishProtocolOptions, SubscribeCommand, SubscribeProtocolOptions,
    Subscription, SubscriptionProtocolOptions, UnsubscribeCommand, UnsubscribeProtocolOptions,
    V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions, V5UnsubscribeProperties,
};
pub use completion::{
    Admission, BrokerReason, Completion, CompletionHandle, CompletionWaitOutcome,
    PublishCompletion, SubscribeCompletion, SubscribeResult, UnsubscribeCompletion,
    UnsubscribeResult,
};
pub use config::{
    AckMode, ClientConfig, CommonConfig, ProtocolConfig, TlsConfig, TransportConfig, V5Config,
    V311Config,
};
pub use error::{DeliveryStatus, Error, ErrorKind, Result};
pub use event::{
    AckToken, ConnectionPhase, DiagnosticsSnapshot, IncomingPublish, OutgoingActivity,
    V5PublishProperties, WrapperEvent,
};
pub use handle::ClientHandle;
pub use protocol::{OperationId, ProtocolVersion, QoS};
pub use runtime::{EventConsumer, NativeClient, NativeClientCloser};
pub use shutdown::LifecycleState;
