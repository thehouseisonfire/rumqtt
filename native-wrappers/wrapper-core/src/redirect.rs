use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{BrokerTarget, TransportConfig};

/// Redirects are opt-in.
///
/// The fixed follow policy starts an isolated clean session
/// with a fresh client identifier and cleared CONNECT authentication, store,
/// proxy credentials, and WebSocket headers. TLS credentials come only from the
/// explicit redirect transport. Server Reference supplies the WebSocket path.
///
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectPolicy {
    #[default]
    Reject,
    Application(RedirectAuthorityConfig),
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
    Policy(RedirectDecisionFailure),
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
    pub selected_reference: Option<String>,
    pub source: RedirectSource,
    pub reason: RedirectReason,
    pub server_reference: Option<String>,
    /// Selected endpoint. An accepted SRV redirect initially has no endpoint;
    /// a second event supplies it immediately before the redirected `Connected`.
    pub target: Option<BrokerTarget>,
    pub failure: Option<RedirectFailure>,
    pub followed: bool,
    pub attempts: usize,
    pub attempt_limit: Option<usize>,
    pub visited_endpoints: usize,
    pub srv_candidate_index: Option<usize>,
    pub srv_candidate_count: Option<usize>,
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

/// Owned async SRV resolution.
///
/// Calls are serialized per client and may overlap
/// across clients. Invocation and polling run on the driver thread without
/// lifecycle locks; neither may block. Reentrant `ClientHandle` admission is
/// allowed, but waiting for its completion stalls the driver. The backend's
/// SRV lookup timeout bounds resolution. Cancellation drops the future;
/// detached work must own its inputs and ignore late completion. Panics become
/// typed redirect failures. Owners are released on driver teardown, including
/// failed start, close, abandonment, and failure (except host-owned clones).
///
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

/// Maximum references in an owned policy snapshot. Excess is rejected, never truncated.
pub const MAX_REDIRECT_REFERENCES: usize = 256;
pub const MAX_REDIRECT_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REDIRECT_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectScheme {
    Mqtt,
    Mqtts,
    Ws,
    Wss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectReference {
    pub raw: String,
    pub scheme: Option<RedirectScheme>,
    pub host: String,
    pub port: Option<u16>,
    pub websocket_resource: Option<String>,
    pub srv_owner: Option<String>,
}

/// Owned credential-free snapshot. Cloning/retaining does not extend its decision lifetime.
#[derive(Clone, Debug)]
pub struct RedirectRequest {
    pub source: RedirectSource,
    pub reason: RedirectReason,
    pub attempt: usize,
    pub client_id: String,
    pub store_scope: String,
    pub references: Vec<RedirectReference>,
    pub deadline: std::time::Instant,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectClientId {
    #[default]
    Fresh,
    Reuse,
    Replace(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RedirectSession {
    #[default]
    Isolated,
    Reuse {
        store_scope: String,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct RedirectTargetConfig {
    pub transport: TransportConfig,
    pub client_id: RedirectClientId,
    pub session: RedirectSession,
    pub username: Option<String>,
    pub password: Option<crate::SecretBytes>,
    pub reuse_authentication_authority: bool,
    pub reuse_network_credentials: bool,
}
impl RedirectTargetConfig {
    #[must_use]
    pub const fn new(transport: TransportConfig) -> Self {
        Self {
            transport,
            client_id: RedirectClientId::Fresh,
            session: RedirectSession::Isolated,
            username: None,
            password: None,
            reuse_authentication_authority: false,
            reuse_network_credentials: false,
        }
    }
    /// Validate copied target fields and storage bounds.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid MQTT fields or `ResourceLimit` for oversized inputs.
    pub fn validate(&self) -> Result<(), RedirectDecisionFailure> {
        let id = match &self.client_id {
            RedirectClientId::Replace(id) => id.as_str(),
            _ => "",
        };
        let scope = match &self.session {
            RedirectSession::Reuse { store_scope } => store_scope.as_str(),
            RedirectSession::Isolated => "",
        };
        let username = self.username.as_deref().unwrap_or("");
        let password = self.password.as_ref().map_or(0, |p| p.expose().len());
        if id.len() + scope.len() + username.len() + password > MAX_REDIRECT_RESPONSE_BYTES {
            return Err(RedirectDecisionFailure::ResourceLimit);
        }
        if [id, scope, username]
            .iter()
            .any(|s| s.len() > usize::from(u16::MAX) || s.contains('\0'))
            || password > usize::from(u16::MAX)
        {
            return Err(RedirectDecisionFailure::InvalidResponse);
        }
        Ok(())
    }
}
impl std::fmt::Debug for RedirectTargetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedirectTargetConfig")
            .field("transport", &self.transport)
            .field("client_id", &self.client_id)
            .field("session", &self.session)
            .field(
                "authentication_configured",
                &(self.username.is_some() || self.password.is_some()),
            )
            .field(
                "reuse_authentication_authority",
                &self.reuse_authentication_authority,
            )
            .field("reuse_network_credentials", &self.reuse_network_credentials)
            .finish()
    }
}

/// Response bound to the exact request owner, with copied configuration and no async token.
#[derive(Clone, Debug)]
pub struct RedirectResponse {
    pub(crate) request: Arc<RedirectRequest>,
    pub(crate) target: Option<(usize, Box<RedirectTargetConfig>)>,
}
impl RedirectResponse {
    #[must_use]
    pub const fn request(&self) -> &Arc<RedirectRequest> {
        &self.request
    }
    pub fn target_mut(&mut self) -> Option<&mut RedirectTargetConfig> {
        self.target.as_mut().map(|(_, target)| target.as_mut())
    }
    #[must_use]
    pub const fn reject(request: Arc<RedirectRequest>) -> Self {
        Self {
            request,
            target: None,
        }
    }
    /// Select a reference from this request and copy its target configuration.
    ///
    /// # Errors
    /// Returns a typed failure for an out-of-range selection or invalid target fields.
    pub fn follow(
        request: Arc<RedirectRequest>,
        index: usize,
        target: RedirectTargetConfig,
    ) -> Result<Self, RedirectDecisionFailure> {
        if index >= request.references.len() {
            return Err(RedirectDecisionFailure::InvalidResponse);
        }
        target.validate()?;
        Ok(Self {
            request,
            target: Some((index, Box::new(target))),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RedirectDecisionFailure {
    #[error("redirect callback failed")]
    Callback,
    #[error("redirect callback panicked")]
    Panic,
    #[error("redirect decision deadline expired")]
    Timeout,
    #[error("invalid redirect response")]
    InvalidResponse,
    #[error("redirect policy resource limit exceeded")]
    ResourceLimit,
    #[error("redirect session store key is already owned")]
    StoreInUse,
}

/// Synchronous authority. Calls are serialized per client and may overlap across clients.
///
/// Runs on the driver without lifecycle/admission locks. Callbacks and destructors must
/// return promptly and never wait for MQTT completion or join their own driver. Nonblocking
/// admission is permitted. Panics and late responses terminate the redirect with typed failures;
/// a deadline cannot preempt a blocking callback. Requests may outlive the client.
pub trait RedirectAuthority: Send + Sync + 'static {
    /// Decide which advertised reference, if any, may be followed.
    ///
    /// # Errors
    /// Return a typed, credential-free failure when a decision cannot be produced.
    fn decide(
        &self,
        request: Arc<RedirectRequest>,
    ) -> Result<RedirectResponse, RedirectDecisionFailure>;
}
#[derive(Clone)]
pub struct RedirectAuthorityConfig {
    pub authority: Arc<dyn RedirectAuthority>,
    pub max_attempts: usize,
    pub decision_timeout: std::time::Duration,
}
impl std::fmt::Debug for RedirectAuthorityConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedirectAuthorityConfig")
            .field("max_attempts", &self.max_attempts)
            .field("decision_timeout", &self.decision_timeout)
            .finish_non_exhaustive()
    }
}
impl PartialEq for RedirectAuthorityConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authority, &other.authority)
            && self.max_attempts == other.max_attempts
            && self.decision_timeout == other.decision_timeout
    }
}
impl Eq for RedirectAuthorityConfig {}

impl RedirectFailure {
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Callback(_) => 1,
            Self::Disabled => 2,
            Self::Rejected => 3,
            Self::InvalidReference => 4,
            Self::UnsupportedTarget => 5,
            Self::Loop => 6,
            Self::AttemptLimit => 7,
            Self::Dns => 8,
            Self::Timeout => 9,
            Self::Transport => 10,
            Self::Policy(failure) => match failure {
                RedirectDecisionFailure::Callback => 11,
                RedirectDecisionFailure::Panic => 12,
                RedirectDecisionFailure::Timeout => 13,
                RedirectDecisionFailure::InvalidResponse => 14,
                RedirectDecisionFailure::ResourceLimit => 15,
                RedirectDecisionFailure::StoreInUse => 16,
            },
        }
    }
}
