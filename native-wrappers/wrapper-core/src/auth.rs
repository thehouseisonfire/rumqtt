use std::sync::Arc;
use std::time::Duration;
use std::{future::Future, pin::Pin};

use bytes::Bytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthExchange {
    Initial,
    Reauthentication,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    Success,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthStage {
    Started,
    Continue,
    Succeeded,
    Failed,
}

impl AuthStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Continue => "continue",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthEvent {
    pub exchange: AuthExchange,
    pub method: String,
    pub stage: AuthStage,
    pub failure: Option<AuthFailure>,
    /// MQTT AUTH reason code when this stage came from a broker AUTH packet.
    pub reason_code: Option<u8>,
    /// Owned broker AUTH properties, preserving optional values and property order.
    pub properties: Option<AuthProperties>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    /// Configured identity, allowing a shared owner to distinguish clients.
    pub client_id: String,
    pub exchange: AuthExchange,
    pub generation: u64,
    pub method: String,
}

/// Legal AUTH properties; presence and repeated user-property order are preserved.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AuthProperties {
    pub method: Option<String>,
    pub data: Option<Bytes>,
    pub reason_string: Option<String>,
    pub user_properties: Vec<(String, String)>,
}

impl std::fmt::Debug for AuthProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProperties")
            .field("method", &self.method)
            .field("data", &self.data.as_ref().map(|_| "[REDACTED]"))
            .finish_non_exhaustive()
    }
}

impl AuthProperties {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if let Some(method) = &self.method {
            crate::validation::validate_mqtt_utf8_string(method, "authentication method")?;
        }
        if let Some(reason) = &self.reason_string {
            crate::validation::validate_mqtt_utf8_string(reason, "authentication reason")?;
        }
        if self.data.as_ref().is_some_and(|data| data.len() > 65535) {
            return Err(crate::validation::protocol_option_error(
                "authentication data exceeds 65535 bytes",
            ));
        }
        for (key, value) in &self.user_properties {
            crate::validation::validate_mqtt_utf8_string(key, "authentication user property")?;
            crate::validation::validate_mqtt_utf8_string(value, "authentication user property")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthChallenge {
    Start,
    Continue(Option<AuthProperties>),
    Success(Option<AuthProperties>),
    Failed,
}

/// Packet details for a deferred authentication response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AsyncAuthChallenge {
    Start,
    Continue {
        reason_code: u8,
        properties: Option<AuthProperties>,
    },
    Success {
        reason_code: u8,
        properties: Option<AuthProperties>,
    },
}

pub type AuthFuture =
    Pin<Box<dyn Future<Output = Result<AuthAction, AuthFailure>> + Send + 'static>>;

/// An owned, cancellable authentication authority. Dropping the future must
/// release pending work; one client invokes at most one response at a time.
///
/// Panics during construction, polling, or destruction of a response future
/// are contained and reported as [`AuthFailure::Panic`] without logging the payload.
pub trait AsyncAuthenticator: Send + Sync + 'static {
    fn respond(&self, context: AuthContext, challenge: AsyncAuthChallenge) -> AuthFuture;
    fn failure(&self, _context: AuthContext, _failure: AuthFailure) {}
}

#[derive(Clone)]
pub struct AsyncAuthenticatorConfig {
    pub authenticator: Arc<dyn AsyncAuthenticator>,
    /// Deadline for the whole exchange, including callbacks and waits for the broker.
    pub exchange_timeout: Duration,
    /// CONNECT method installed by a foreign wrapper, if any.
    pub configured_method: Option<String>,
}

impl AsyncAuthenticatorConfig {
    pub fn new(authenticator: Arc<dyn AsyncAuthenticator>) -> Self {
        Self {
            authenticator,
            exchange_timeout: Duration::from_secs(30),
            configured_method: None,
        }
    }
}

impl std::fmt::Debug for AsyncAuthenticatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncAuthenticatorConfig")
            .field("exchange_timeout", &self.exchange_timeout)
            .finish_non_exhaustive()
    }
}

impl PartialEq for AsyncAuthenticatorConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authenticator, &other.authenticator)
            && self.exchange_timeout == other.exchange_timeout
            && self.configured_method == other.configured_method
    }
}
impl Eq for AsyncAuthenticatorConfig {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthAction {
    Send(AuthProperties),
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthFailure {
    #[error("authenticator rejected the exchange")]
    Rejected,
    #[error("authenticator panicked")]
    Panic,
    #[error("authentication exchange timed out")]
    Timeout,
    #[error("authenticator returned an invalid response")]
    InvalidResponse,
    #[error("authentication exchange overlapped another exchange")]
    Overlapping,
    #[error("authentication exchange lost its connection")]
    ConnectionClosed,
    #[error("authentication method is missing or changed")]
    Method,
    #[error("broker rejected authentication")]
    BrokerRejected,
}

/// A synchronous, nonblocking authentication mechanism. Calls for one client are
/// serialized on its driver thread; clients sharing an owner may call concurrently.
///
/// Inputs are owned and no wrapper lifecycle lock is held.
/// The native backend requires synchronous responses; do not perform blocking
/// I/O or wait on futures here. Prepare credentials before starting the client.
/// Reentrant nonblocking `ClientHandle` calls are permitted, but waiting for an
/// MQTT completion prevents the exchange from advancing.
///
/// One configured authenticator is the sole authority for challenges; the event
/// consumer cannot supply competing responses. The exchange deadline includes
/// callbacks, but cannot preempt a callback that violates the nonblocking
/// contract. Panics and invalid outputs terminate the client with `AuthFailure`.
/// Cancellation occurs between calls; no late response is applied. The driver's
/// owner reference is released at failed start or driver teardown after close,
/// abandonment, or failure. Other configuration clones retain their references.
/// Destructors must not block or panic.
pub trait Authenticator: Send + Sync + 'static {
    /// Produces the response to a broker authentication challenge.
    ///
    /// # Errors
    ///
    /// Returns an authentication failure when the authority cannot answer the challenge.
    fn respond(
        &self,
        context: AuthContext,
        challenge: AuthChallenge,
    ) -> Result<AuthAction, AuthFailure>;
}

#[derive(Clone)]
pub struct AuthenticatorConfig {
    pub authenticator: Arc<dyn Authenticator>,
    pub exchange_timeout: Duration,
}

impl AuthenticatorConfig {
    pub fn new(authenticator: Arc<dyn Authenticator>) -> Self {
        Self {
            authenticator,
            exchange_timeout: Duration::from_secs(30),
        }
    }
}
impl std::fmt::Debug for AuthenticatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatorConfig")
            .field("exchange_timeout", &self.exchange_timeout)
            .finish_non_exhaustive()
    }
}
impl PartialEq for AuthenticatorConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authenticator, &other.authenticator)
            && self.exchange_timeout == other.exchange_timeout
    }
}
impl Eq for AuthenticatorConfig {}
