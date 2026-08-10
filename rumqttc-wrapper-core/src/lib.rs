//! Owned, protocol-neutral support for native language wrappers.
//!
//! This crate is implementation infrastructure. It deliberately exposes Rust
//! values rather than a stable foreign-function ABI.

mod command;
mod completion;
mod config;
mod driver;
mod error;
mod event;
mod protocol;
mod shutdown;

pub use command::{Command, PublishCommand, SubscribeCommand, Subscription};
pub use completion::{
    Admission, BrokerReason, Completion, CompletionHandle, PublishCompletion, SubscribeCompletion,
    SubscribeResult, UnsubscribeCompletion, UnsubscribeResult,
};
pub use config::{
    AckMode, ClientConfig, CommonConfig, ProtocolConfig, TlsConfig, TransportConfig, V5Config,
    V311Config,
};
pub use driver::{ClientHandle, EventConsumer, NativeClient};
pub use error::{DeliveryStatus, Error, ErrorKind, Result};
pub use event::{
    AckToken, ConnectionPhase, DiagnosticsSnapshot, IncomingPublish, OutgoingActivity,
    V5PublishProperties, WrapperEvent,
};
pub use protocol::{OperationId, ProtocolVersion, QoS};
pub use shutdown::LifecycleState;
