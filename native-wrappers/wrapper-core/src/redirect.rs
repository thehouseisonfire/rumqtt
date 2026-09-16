use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{BrokerTarget, TransportConfig};

/// Redirects are opt-in. Each followed target starts an isolated clean session
/// with a fresh client identifier and cleared CONNECT authentication, store,
/// proxy credentials, and WebSocket headers. TLS credentials come only from the
/// explicit redirect transport. Server Reference supplies the WebSocket path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectPolicy {
    #[default]
    Reject,
    Follow {
        max_attempts: usize,
        transport: TransportConfig,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectSource {
    ConnAck,
    Disconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectReason {
    UseAnotherServer,
    ServerMoved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectFailure {
    Callback(SrvFailure),
    Disabled,
    Rejected,
    InvalidReference,
    UnsupportedTarget,
    Loop,
    AttemptLimit,
    Dns,
    Timeout,
    Transport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectEvent {
    pub source: RedirectSource,
    pub reason: RedirectReason,
    pub server_reference: Option<String>,
    /// Selected endpoint. An accepted SRV redirect initially has no endpoint;
    /// a second event supplies it immediately before the redirected `Connected`.
    pub target: Option<BrokerTarget>,
    pub failure: Option<RedirectFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SrvFailure {
    #[error("SRV query failed")]
    Query,
    #[error("SRV callback panicked")]
    Panic,
}

pub type SrvFuture =
    Pin<Box<dyn Future<Output = Result<Vec<SrvRecord>, SrvFailure>> + Send + 'static>>;

/// Owned async SRV resolution. Calls are serialized per client and may overlap
/// across clients. Invocation and polling run on the driver thread without
/// lifecycle locks; neither may block. Reentrant `ClientHandle` admission is
/// allowed, but waiting for its completion stalls the driver. The backend's
/// total connection deadline bounds resolution. Cancellation drops the future;
/// detached work must own its inputs and ignore late completion. Panics become
/// typed redirect failures. Owners are released on driver teardown, including
/// failed start, close, abandonment, and failure (except host-owned clones).
/// Destructors must not block or panic.
pub trait SrvResolver: Send + Sync + 'static {
    fn resolve(&self, owner: String) -> SrvFuture;
}

#[derive(Clone)]
pub struct SrvResolverConfig(pub Arc<dyn SrvResolver>);
impl std::fmt::Debug for SrvResolverConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SrvResolverConfig(..)")
    }
}
impl PartialEq for SrvResolverConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for SrvResolverConfig {}
