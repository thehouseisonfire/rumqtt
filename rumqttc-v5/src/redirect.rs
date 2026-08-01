use std::fmt::{self, Debug, Formatter};
use std::net::Ipv6Addr;
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::{ConnectAuth, Transport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectReason {
    UseAnotherServer,
    ServerMoved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectSource {
    ConnAck,
    Disconnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectOutcome {
    pub reason: RedirectReason,
    pub server_reference: Option<String>,
    pub source: RedirectSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RedirectReference {
    host: String,
    port: Option<u16>,
}

impl RedirectReference {
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    pub(crate) fn is_srv_name(&self) -> bool {
        let mut labels = self.host.split('.');
        matches!(
            (labels.next(), labels.next()),
            (Some(service), Some(protocol))
                if service.starts_with('_')
                    && service.len() > 1
                    && protocol.starts_with('_')
                    && protocol.len() > 1
        )
    }

    pub(crate) fn endpoint_key(&self, default_port: u16, transport: &Transport) -> String {
        normalized_profile_key(&self.host, self.port.unwrap_or(default_port), transport)
    }
}

pub fn normalized_profile_key(host: &str, port: u16, transport: &Transport) -> String {
    format!(
        "{}://{}",
        transport.redirect_identity(),
        normalized_endpoint_key(host, port)
    )
}

pub fn normalized_endpoint_key(host: &str, port: u16) -> String {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed.parse::<Ipv6Addr>().map_or_else(
        |_| {
            let host = host.to_ascii_lowercase();
            let host = host.strip_suffix('.').unwrap_or(&host);
            format!("{host}:{port}")
        },
        |address| format!("[{address}]:{port}"),
    )
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RedirectReferenceError {
    #[error("Server Reference is missing")]
    Missing,
    #[error("Server Reference contains an empty authority")]
    Empty,
    #[error("URI schemes are not supported in Server Reference authorities")]
    Scheme,
    #[error("Server Reference authority contains a path, query, fragment, or user information")]
    NonAuthority,
    #[error("Server Reference contains an invalid host")]
    InvalidHost,
    #[error("Server Reference contains an invalid port")]
    InvalidPort,
}

/// Parse the space-separated authority list described by MQTT 5 section 4.11.
///
/// # Errors
///
/// Returns a [`RedirectReferenceError`] if the value is missing, empty, or
/// contains an invalid authority.
pub fn parse_server_references(
    value: Option<&str>,
) -> Result<Vec<RedirectReference>, RedirectReferenceError> {
    let value = value.ok_or(RedirectReferenceError::Missing)?;
    if value.trim().is_empty() {
        return Err(RedirectReferenceError::Empty);
    }
    value
        .split_ascii_whitespace()
        .map(parse_authority)
        .collect()
}

fn parse_authority(authority: &str) -> Result<RedirectReference, RedirectReferenceError> {
    if authority.contains("://") {
        return Err(RedirectReferenceError::Scheme);
    }
    if authority
        .chars()
        .any(|character| matches!(character, '/' | '?' | '#' | '@'))
    {
        return Err(RedirectReferenceError::NonAuthority);
    }

    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed
            .split_once(']')
            .ok_or(RedirectReferenceError::InvalidHost)?;
        host.parse::<Ipv6Addr>()
            .map_err(|_| RedirectReferenceError::InvalidHost)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(parse_port(
                suffix
                    .strip_prefix(':')
                    .ok_or(RedirectReferenceError::InvalidPort)?,
            )?)
        };
        return Ok(RedirectReference {
            host: host.to_ascii_lowercase(),
            port,
        });
    }

    if authority.contains('[') || authority.contains(']') {
        return Err(RedirectReferenceError::InvalidHost);
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            if host.contains(':') {
                return Err(RedirectReferenceError::InvalidHost);
            }
            (host, Some(parse_port(port)?))
        }
        None => (authority, None),
    };
    if host.is_empty()
        || !host.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
    {
        return Err(RedirectReferenceError::InvalidHost);
    }

    Ok(RedirectReference {
        host: host.to_ascii_lowercase(),
        port,
    })
}

fn parse_port(value: &str) -> Result<u16, RedirectReferenceError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| RedirectReferenceError::InvalidPort)?;
    if port == 0 {
        return Err(RedirectReferenceError::InvalidPort);
    }
    Ok(port)
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

/// Application-approved connection profile for one advertised redirect target.
#[derive(Clone)]
pub struct RedirectTargetProfile {
    reference: RedirectReference,
    transport: Transport,
    client_id: RedirectClientId,
    session: RedirectSession,
    authentication: Option<ConnectAuth>,
    reuse_authenticator: bool,
    reuse_network_credentials: bool,
}

impl RedirectTargetProfile {
    /// Construct a profile which clears authentication and starts an isolated clean session.
    #[must_use]
    pub const fn isolated(reference: RedirectReference, transport: Transport) -> Self {
        Self {
            reference,
            transport,
            client_id: RedirectClientId::Fresh,
            session: RedirectSession::Isolated,
            authentication: None,
            reuse_authenticator: false,
            reuse_network_credentials: false,
        }
    }

    #[must_use]
    pub fn client_id(mut self, client_id: RedirectClientId) -> Self {
        self.client_id = client_id;
        self
    }

    #[must_use]
    pub fn session(mut self, session: RedirectSession) -> Self {
        self.session = session;
        self
    }

    /// Reuse the supplied current CONNECT authentication and enhanced authenticator.
    #[must_use]
    pub fn reuse_authentication(mut self, auth: ConnectAuth) -> Self {
        self.authentication = Some(auth);
        self.reuse_authenticator = true;
        self
    }

    /// Replace CONNECT authentication without reusing the enhanced authenticator.
    #[must_use]
    pub fn authentication(mut self, auth: ConnectAuth) -> Self {
        self.authentication = Some(auth);
        self.reuse_authenticator = false;
        self
    }

    /// Explicitly retain proxy configuration and websocket request modifiers.
    ///
    /// These hooks can carry endpoint credentials, so isolated profiles clear
    /// them unless this method is called.
    #[must_use]
    pub const fn reuse_network_credentials(mut self) -> Self {
        self.reuse_network_credentials = true;
        self
    }

    #[must_use]
    pub const fn reference(&self) -> &RedirectReference {
        &self.reference
    }

    pub(crate) fn transport(&self) -> Transport {
        self.transport.clone()
    }

    pub(crate) const fn default_port(&self) -> Option<u16> {
        self.transport.redirect_default_port()
    }

    pub(crate) const fn client_id_policy(&self) -> &RedirectClientId {
        &self.client_id
    }

    pub(crate) const fn session_policy(&self) -> &RedirectSession {
        &self.session
    }

    pub(crate) const fn authentication_policy(&self) -> Option<&ConnectAuth> {
        self.authentication.as_ref()
    }

    pub(crate) const fn reuses_authenticator(&self) -> bool {
        self.reuse_authenticator
    }

    pub(crate) const fn reuses_network_credentials(&self) -> bool {
        self.reuse_network_credentials
    }
}

impl Debug for RedirectTargetProfile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedirectTargetProfile")
            .field("reference", &self.reference)
            .field("client_id", &self.client_id)
            .field("session", &self.session)
            .field("authentication_configured", &self.authentication.is_some())
            .field("reuse_authenticator", &self.reuse_authenticator)
            .field("reuse_network_credentials", &self.reuse_network_credentials)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct RedirectContext<'a> {
    pub outcome: &'a RedirectOutcome,
    pub references: &'a [RedirectReference],
    /// One-based decision number in the current redirect chain.
    pub attempt: usize,
}

#[derive(Clone, Debug)]
pub enum RedirectDecision {
    Reject,
    Follow(Box<RedirectTargetProfile>),
}

impl RedirectDecision {
    #[must_use]
    pub fn follow(profile: RedirectTargetProfile) -> Self {
        Self::Follow(Box::new(profile))
    }
}

type RedirectHandler = dyn for<'a> Fn(&RedirectContext<'a>) -> RedirectDecision + Send + Sync;

/// Opt-in bounded application policy for MQTT 5 redirects.
#[derive(Clone)]
pub struct RedirectPolicy {
    max_attempts: NonZeroUsize,
    handler: Arc<RedirectHandler>,
}

impl RedirectPolicy {
    #[must_use]
    pub fn new<F>(max_attempts: NonZeroUsize, handler: F) -> Self
    where
        F: for<'a> Fn(&RedirectContext<'a>) -> RedirectDecision + Send + Sync + 'static,
    {
        Self {
            max_attempts,
            handler: Arc::new(handler),
        }
    }

    #[must_use]
    pub const fn max_attempts(&self) -> NonZeroUsize {
        self.max_attempts
    }

    pub(crate) fn decide(&self, context: &RedirectContext<'_>) -> RedirectDecision {
        (self.handler)(context)
    }
}

impl Debug for RedirectPolicy {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedirectPolicy")
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RedirectFailure {
    #[error("automatic redirects are disabled")]
    Disabled,
    #[error("redirect was rejected by application policy")]
    Rejected,
    #[error("invalid Server Reference: {0}")]
    InvalidReference(#[from] RedirectReferenceError),
    #[error("redirect target is incompatible with the current broker form")]
    UnsupportedTarget,
    #[error("redirect policy selected a target not present in Server Reference")]
    UnadvertisedTarget,
    #[error("redirect loop detected")]
    Loop,
    #[error("redirect attempt limit reached")]
    AttemptLimit,
    #[error("redirected connection failed: {0}")]
    FollowFailed(#[source] Box<crate::ConnectionError>),
}

#[derive(Debug, thiserror::Error)]
#[error("MQTT redirect {outcome:?} could not be followed: {failure}")]
pub struct RedirectError {
    pub outcome: RedirectOutcome,
    #[source]
    pub failure: RedirectFailure,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mqtt_authority_examples() {
        let references = parse_server_references(Some(
            "myserver.xyz.org myserver.xyz.org:8883 10.10.151.22:8883 [fe80::9610:3eff:fe1c]:1883",
        ))
        .unwrap();

        assert_eq!(references.len(), 4);
        assert_eq!(references[0].host(), "myserver.xyz.org");
        assert_eq!(references[0].port(), None);
        assert_eq!(references[1].port(), Some(8883));
        assert_eq!(references[2].host(), "10.10.151.22");
        assert_eq!(references[3].host(), "fe80::9610:3eff:fe1c");
        assert_eq!(references[3].port(), Some(1883));
        assert_eq!(
            parse_server_references(Some("_mqtt._tcp.example.com")).unwrap()[0].host(),
            "_mqtt._tcp.example.com"
        );
        assert!(parse_server_references(Some("_mqtt._tcp.example.com")).unwrap()[0].is_srv_name());
        assert!(!parse_server_references(Some("broker.example")).unwrap()[0].is_srv_name());
    }

    #[test]
    fn rejects_missing_empty_malformed_and_scheme_references() {
        assert_eq!(
            parse_server_references(None),
            Err(RedirectReferenceError::Missing)
        );
        assert_eq!(
            parse_server_references(Some("  ")),
            Err(RedirectReferenceError::Empty)
        );
        assert_eq!(
            parse_server_references(Some("mqtt://broker.example")),
            Err(RedirectReferenceError::Scheme)
        );
        assert_eq!(
            parse_server_references(Some("broker.example:0")),
            Err(RedirectReferenceError::InvalidPort)
        );
        assert_eq!(
            parse_server_references(Some("user@broker.example")),
            Err(RedirectReferenceError::NonAuthority)
        );
        assert_eq!(
            parse_server_references(Some("fe80::1")),
            Err(RedirectReferenceError::InvalidHost)
        );
    }
}
