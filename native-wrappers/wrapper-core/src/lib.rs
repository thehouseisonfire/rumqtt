//! Owned, protocol-neutral support for native language wrappers.
//!
//! This crate is implementation infrastructure. It deliberately exposes Rust
//! values rather than a stable foreign-function ABI.

mod acknowledgement;
pub use acknowledgement::{AcknowledgementProtocolOptions, V5AcknowledgementOptions};
mod auth;
mod scram;
pub use auth::{
    AsyncAuthChallenge, AsyncAuthenticator, AsyncAuthenticatorConfig, AuthAction, AuthChallenge,
    AuthContext, AuthEvent, AuthExchange, AuthFailure, AuthFuture, AuthOutcome, AuthProperties,
    AuthStage, Authenticator, AuthenticatorConfig,
};
pub use scram::ScramConfig;
mod backend;
mod command;
mod completion;
mod config;
mod connection;
mod error;
mod event;
mod execution;
pub use execution::{ExecutionContext, ExecutionOptions, ExecutionState, blocking_wait_allowed};
mod handle;
mod operations;
mod ordered;
pub use ordered::{OrderedDisconnectFailure, OrderedShutdownDiagnostics, OrderedShutdownPhase};
mod protocol;
mod publish;
pub use publish::{
    PublishAdmissionPolicy, PublishBudgetLimits, PublishBudgetSnapshot, PublishFailure,
};
mod proxy;
mod redirect;
mod runtime;
pub mod rust_session_store;
mod session;
mod shutdown;
mod transport;
pub use redirect::{
    MAX_REDIRECT_REFERENCES, MAX_REDIRECT_REQUEST_BYTES, MAX_REDIRECT_RESPONSE_BYTES,
    RedirectAuthority, RedirectAuthorityConfig, RedirectClientId, RedirectDecisionFailure,
    RedirectEvent, RedirectFailure, RedirectPolicy, RedirectReason, RedirectReference,
    RedirectRequest, RedirectResponse, RedirectScheme, RedirectSession, RedirectSource,
    RedirectTargetConfig, SrvFailure, SrvFuture, SrvRecord, SrvResolver, SrvResolverConfig,
};
pub use session::{
    BrokerSessionResumePolicy, SessionCheckpoint, SessionPresentMismatchPolicy, SessionStore,
    SessionStoreConfig, SessionStoreKey, StoreFailure, StoreFuture,
};
pub use transport::{
    IoFuture as TransportIoFuture, NetworkHandling, OwnedIo as TransportIo, TransportConnection,
    TransportConnector, TransportConnectorConfig, TransportFailure, TransportFuture, TransportMode,
    TransportRequest,
};
mod validation;
mod websocket;
mod will;
pub use proxy::{ProxyConfig, ProxyCredentials};
pub use websocket::{
    MAX_WEBSOCKET_BYTES, MAX_WEBSOCKET_EDITS, MAX_WEBSOCKET_HEADERS, MAX_WEBSOCKET_PATH,
    WebSocketHandshake, WebSocketHandshakeConfig, WebSocketHandshakeFailure,
    WebSocketHandshakeFuture, WebSocketHandshakeRequest, WebSocketHandshakeResponse,
    WebSocketHeader, WebSocketRequestHeader,
};

pub use command::{
    Command, DisconnectProtocolOptions, PublishCommand, PublishProtocolOptions, SubscribeCommand,
    SubscribeProtocolOptions, Subscription, SubscriptionProtocolOptions, UnsubscribeCommand,
    UnsubscribeProtocolOptions, V5DisconnectOptions, V5OutgoingPublishProperties,
    V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions, V5UnsubscribeProperties,
};
pub use completion::{
    AcknowledgementKind, AcknowledgementProperties, Admission, BrokerAcknowledgement, BrokerReason,
    Completion, CompletionHandle, CompletionWaitOutcome, PublishCompletion, SubscribeCompletion,
    SubscribeResult, TerminalOutcome, UnsubscribeCompletion, UnsubscribeResult,
};
mod tls_advanced;
pub use tls_advanced::*;
mod tls;
pub use tls::{MAX_TLS_PINS, TlsCapabilities, TlsPin, TlsPinTarget, TlsVersionPolicy};

pub use config::{
    AckMode, BrokerTarget, ClientConfig, CommonConfig, IncomingPacketLimit, NetworkConfig,
    ProtocolConfig, SecretBytes, TlsBackend, TlsClientIdentity, TlsConfig, TlsRootPolicy,
    TopicAliasPolicy, TransportConfig, V4Config, V5Config, V5ConnectProperties,
};
pub use connection::{ConnectionHandle, ConnectionResult};
pub use error::{DeliveryStatus, Error, ErrorCode, ErrorContext, ErrorKind, Result};
pub use event::{
    AckToken, ConnAckDetails, ConnAckDiagnostic, ConnAckSessionDiagnostics, ConnectionPhase,
    DiagnosticsSnapshot, IncomingPublish, OutgoingActivity, OutgoingEvent, V5ConnAckProperties,
    V5IncomingPublishProperties, WrapperEvent,
};
pub use handle::ClientHandle;
pub use protocol::{OperationId, ProtocolVersion, QoS};
pub use runtime::{EventConsumer, NativeClient, NativeClientCloser};
pub use shutdown::LifecycleState;
pub use will::{LastWillConfig, LastWillProtocolOptions, V5WillProperties};
