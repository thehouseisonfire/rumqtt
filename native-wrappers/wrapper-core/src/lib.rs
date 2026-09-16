//! Owned, protocol-neutral support for native language wrappers.
//!
//! This crate is implementation infrastructure. It deliberately exposes Rust
//! values rather than a stable foreign-function ABI.

mod acknowledgement;
mod auth;
mod scram;
pub use auth::{
    AuthAction, AuthChallenge, AuthContext, AuthEvent, AuthExchange, AuthFailure, AuthOutcome,
    AuthProperties, AuthStage, Authenticator, AuthenticatorConfig,
};
pub use scram::ScramConfig;
mod backend;
mod command;
mod completion;
mod config;
mod connection;
mod error;
mod event;
mod handle;
mod operations;
mod protocol;
mod proxy;
mod redirect;
mod runtime;
pub mod rust_session_store;
mod session;
mod shutdown;
pub use redirect::{
    RedirectEvent, RedirectFailure, RedirectPolicy, RedirectReason, RedirectSource, SrvFailure,
    SrvFuture, SrvRecord, SrvResolver, SrvResolverConfig,
};
pub use session::{
    BrokerSessionResumePolicy, SessionCheckpoint, SessionStore, SessionStoreConfig,
    SessionStoreKey, StoreFailure, StoreFuture,
};
mod validation;
mod websocket;
mod will;
pub use proxy::{ProxyConfig, ProxyCredentials};
pub use websocket::WebSocketHeader;

pub use command::{
    Command, DisconnectProtocolOptions, PublishCommand, PublishProtocolOptions, SubscribeCommand,
    SubscribeProtocolOptions, Subscription, SubscriptionProtocolOptions, UnsubscribeCommand,
    UnsubscribeProtocolOptions, V5DisconnectOptions, V5OutgoingPublishProperties,
    V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions, V5UnsubscribeProperties,
};
pub use completion::{
    Admission, BrokerReason, Completion, CompletionHandle, CompletionWaitOutcome,
    PublishCompletion, SubscribeCompletion, SubscribeResult, UnsubscribeCompletion,
    UnsubscribeResult,
};
pub use config::{
    AckMode, BrokerTarget, ClientConfig, CommonConfig, IncomingPacketLimit, NetworkConfig,
    ProtocolConfig, SecretBytes, TlsBackend, TlsClientIdentity, TlsConfig, TlsRootPolicy,
    TopicAliasPolicy, TransportConfig, V4Config, V5Config, V5ConnectProperties,
};
pub use connection::{ConnectionHandle, ConnectionResult};
pub use error::{DeliveryStatus, Error, ErrorCode, ErrorContext, ErrorKind, Result};
pub use event::{
    AckToken, ConnAckDetails, ConnectionPhase, DiagnosticsSnapshot, IncomingPublish,
    OutgoingActivity, OutgoingEvent, V5ConnAckProperties, V5IncomingPublishProperties,
    WrapperEvent,
};
pub use handle::ClientHandle;
pub use protocol::{OperationId, ProtocolVersion, QoS};
pub use runtime::{EventConsumer, NativeClient, NativeClientCloser};
pub use shutdown::LifecycleState;
pub use will::{LastWillConfig, LastWillProtocolOptions, V5WillProperties};
