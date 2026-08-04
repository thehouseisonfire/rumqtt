use std::fmt::{self, Debug, Formatter};
use std::net::{IpAddr, Ipv6Addr};
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::{Broker, ConnectAuth, SrvLookupError, Transport, broker_transport_matches};

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

/// URI scheme advertised by an MQTT 5 Server Reference.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RedirectScheme {
    Mqtt,
    Mqtts,
    Ws,
    Wss,
}

impl RedirectScheme {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Mqtt => "mqtt",
            Self::Mqtts => "mqtts",
            Self::Ws => "ws",
            Self::Wss => "wss",
        }
    }

    const fn default_port(self) -> u16 {
        match self {
            Self::Mqtt => 1883,
            Self::Mqtts => 8883,
            Self::Ws => 80,
            Self::Wss => 443,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RedirectReferenceKind {
    Authority {
        host: String,
        port: Option<u16>,
    },
    Uri {
        scheme: RedirectScheme,
        host: String,
        port: Option<u16>,
        resource_name: String,
    },
    Srv {
        owner: SrvOwner,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SrvOwner {
    normalized: String,
}

impl SrvOwner {
    pub(crate) fn query_name(&self) -> String {
        format!("{}.", self.normalized)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.normalized
    }
}

/// One validated token from an MQTT 5 Server Reference.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RedirectReference {
    raw: String,
    kind: RedirectReferenceKind,
}

impl RedirectReference {
    /// Return the advertised token exactly as received.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Return the URI scheme, or `None` for an authority reference.
    #[must_use]
    pub const fn scheme(&self) -> Option<RedirectScheme> {
        match self.kind {
            RedirectReferenceKind::Authority { .. } | RedirectReferenceKind::Srv { .. } => None,
            RedirectReferenceKind::Uri { scheme, .. } => Some(scheme),
        }
    }

    #[must_use]
    pub fn host(&self) -> &str {
        match &self.kind {
            RedirectReferenceKind::Authority { host, .. }
            | RedirectReferenceKind::Uri { host, .. } => host,
            RedirectReferenceKind::Srv { owner } => owner.as_str(),
        }
    }

    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        match self.kind {
            RedirectReferenceKind::Authority { port, .. }
            | RedirectReferenceKind::Uri { port, .. } => port,
            RedirectReferenceKind::Srv { .. } => None,
        }
    }

    /// Return the WebSocket request target, including its query string.
    #[must_use]
    pub fn websocket_resource_name(&self) -> Option<&str> {
        match &self.kind {
            RedirectReferenceKind::Uri {
                scheme: RedirectScheme::Ws | RedirectScheme::Wss,
                resource_name,
                ..
            } => Some(resource_name),
            _ => None,
        }
    }

    /// Check whether the current feature set can materialize this reference.
    ///
    /// This does not select TLS configuration or any other credentials.
    ///
    /// # Errors
    ///
    /// Returns a feature-specific error when the reference needs WebSocket or
    /// TLS support which is not enabled in this build.
    pub const fn ensure_supported(&self) -> Result<(), RedirectTargetError> {
        match self.scheme() {
            Some(RedirectScheme::Ws | RedirectScheme::Wss) if !cfg!(feature = "websocket") => {
                Err(RedirectTargetError::WebsocketUnavailable)
            }
            Some(RedirectScheme::Mqtts | RedirectScheme::Wss)
                if !cfg!(any(
                    feature = "use-rustls-no-provider",
                    feature = "use-native-tls"
                )) =>
            {
                Err(RedirectTargetError::TlsUnavailable)
            }
            _ => Ok(()),
        }
    }

    const fn is_srv_name(&self) -> bool {
        matches!(self.kind, RedirectReferenceKind::Srv { .. })
    }

    /// Return the normalized DNS SRV owner, if this is an SRV reference.
    #[must_use]
    pub fn srv_owner(&self) -> Option<&str> {
        match &self.kind {
            RedirectReferenceKind::Srv { owner } => Some(owner.as_str()),
            _ => None,
        }
    }

    pub(crate) fn srv_owner_value(&self) -> Option<SrvOwner> {
        match &self.kind {
            RedirectReferenceKind::Srv { owner } => Some(owner.clone()),
            _ => None,
        }
    }

    fn effective_port(&self, transport: &Transport) -> Option<u16> {
        self.port().or_else(|| {
            self.scheme()
                .map(RedirectScheme::default_port)
                .or_else(|| transport.redirect_default_port())
        })
    }

    #[cfg(feature = "websocket")]
    fn websocket_url(&self) -> Option<String> {
        let scheme = self.scheme()?;
        let resource = self.websocket_resource_name()?;
        let host = uri_host(self.host());
        let port = self
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default();
        Some(format!("{}://{host}{port}{resource}", scheme.as_str()))
    }

    pub(super) fn endpoint_key(&self, transport: &Transport) -> Option<String> {
        if self.is_srv_name() {
            return None;
        }
        let port = self.effective_port(transport)?;
        let endpoint = normalized_endpoint_key(self.host(), port);
        self.websocket_resource_name().map_or_else(
            || Some(format!("{}://{}", transport.redirect_identity(), endpoint)),
            |resource| {
                Some(format!(
                    "{}://{}{}",
                    transport.redirect_identity(),
                    endpoint,
                    normalize_resource_name(resource)
                ))
            },
        )
    }
}

pub fn normalized_broker_key(broker: &Broker, transport: &Transport) -> Option<String> {
    if let Some((host, port)) = broker.tcp_address() {
        return Some(normalized_profile_key(host, port, transport));
    }
    #[cfg(feature = "websocket")]
    if let Some(url) = broker.websocket_url() {
        let uri = url.parse::<http::Uri>().ok()?;
        let host = unbracket_host(uri.host()?).to_owned();
        let port = uri
            .port_u16()
            .or_else(|| match transport.redirect_identity() {
                "ws" => Some(80),
                "wss" => Some(443),
                _ => None,
            })?;
        let resource = uri
            .path_and_query()
            .map_or("/", http::uri::PathAndQuery::as_str);
        return Some(format!(
            "{}://{}{}",
            transport.redirect_identity(),
            normalized_endpoint_key(&host, port),
            normalize_resource_name(resource)
        ));
    }
    None
}

pub fn normalized_profile_key(host: &str, port: u16, transport: &Transport) -> String {
    format!(
        "{}://{}",
        transport.redirect_identity(),
        normalized_endpoint_key(host, port)
    )
}

pub fn normalized_endpoint_key(host: &str, port: u16) -> String {
    let unbracketed = unbracket_host(host);
    unbracketed.parse::<IpAddr>().map_or_else(
        |_| {
            let host = unbracketed.to_ascii_lowercase();
            let host = host.strip_suffix('.').unwrap_or(&host);
            format!("{host}:{port}")
        },
        |address| match address {
            IpAddr::V4(address) => format!("{address}:{port}"),
            IpAddr::V6(address) => format!("[{address}]:{port}"),
        },
    )
}

fn unbracket_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(feature = "websocket")]
fn uri_host(host: &str) -> String {
    if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[non_exhaustive]
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum RedirectReferenceError {
    #[error("Server Reference is missing")]
    Missing,
    #[error("Server Reference contains an empty authority")]
    Empty,
    #[error("Server Reference uses unsupported URI scheme `{0}`")]
    UnsupportedScheme(String),
    #[error("Server Reference contains an invalid URI")]
    InvalidUri,
    #[error("Server Reference authority contains a path, query, fragment, or user information")]
    NonAuthority,
    #[error("Server Reference URI contains user information")]
    UserInformation,
    #[error("Server Reference URI contains a fragment")]
    Fragment,
    #[error("Server Reference URI contains a path or query not supported by its scheme")]
    InvalidResource,
    #[error("Server Reference contains an invalid host")]
    InvalidHost,
    #[error("Server Reference contains an invalid port")]
    InvalidPort,
    #[error("an SRV Server Reference must not contain an explicit port")]
    SrvExplicitPort,
}

/// Parse the space-separated authority or absolute URI list described by MQTT 5 section 4.11.
///
/// # Errors
///
/// Returns a [`RedirectReferenceError`] if the value is missing, empty, or
/// contains an invalid reference.
pub fn parse_server_references(
    value: Option<&str>,
) -> Result<Vec<RedirectReference>, RedirectReferenceError> {
    let value = value.ok_or(RedirectReferenceError::Missing)?;
    if value.trim().is_empty() {
        return Err(RedirectReferenceError::Empty);
    }
    value
        .split_ascii_whitespace()
        .map(|reference| {
            if reference.contains("://") {
                parse_uri(reference)
            } else {
                parse_authority(reference)
            }
        })
        .collect()
}

fn parse_uri(raw: &str) -> Result<RedirectReference, RedirectReferenceError> {
    if raw
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err(RedirectReferenceError::InvalidUri);
    }
    validate_percent_triplets(raw)?;
    if raw.contains('#') {
        return Err(RedirectReferenceError::Fragment);
    }

    let (scheme, _) = raw
        .split_once("://")
        .ok_or(RedirectReferenceError::InvalidUri)?;
    let scheme_name = scheme.to_ascii_lowercase();
    let scheme = match scheme_name.as_str() {
        "mqtt" => RedirectScheme::Mqtt,
        "mqtts" => RedirectScheme::Mqtts,
        "ws" => RedirectScheme::Ws,
        "wss" => RedirectScheme::Wss,
        _ if valid_scheme_name(scheme) => {
            return Err(RedirectReferenceError::UnsupportedScheme(scheme_name));
        }
        _ => return Err(RedirectReferenceError::InvalidUri),
    };

    // `http::Uri` recognizes URI schemes case-insensitively but accepts only
    // lowercase known forms reliably across versions, so normalize only the
    // scheme before generic parsing while retaining `raw` for policy.
    let normalized_input = format!("{}{}", scheme.as_str(), &raw[scheme_name.len()..]);
    let uri = normalized_input
        .parse::<http::Uri>()
        .map_err(|_| classify_uri_parse_error(raw))?;
    let authority = uri.authority().ok_or(RedirectReferenceError::InvalidHost)?;
    if authority.as_str().contains('@') {
        return Err(RedirectReferenceError::UserInformation);
    }
    let host = uri.host().ok_or(RedirectReferenceError::InvalidHost)?;
    let host = validate_uri_host(host)?;
    let port = parse_uri_port(authority.as_str(), uri.port_u16())?;

    let (path, query) = uri.path_and_query().map_or(("", None), |path_and_query| {
        (path_and_query.path(), path_and_query.query())
    });
    let resource_name = match scheme {
        RedirectScheme::Mqtt | RedirectScheme::Mqtts => {
            if !matches!(path, "" | "/") || query.is_some() {
                return Err(RedirectReferenceError::InvalidResource);
            }
            String::new()
        }
        RedirectScheme::Ws | RedirectScheme::Wss => uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), ToString::to_string),
    };

    Ok(RedirectReference {
        raw: raw.to_owned(),
        kind: RedirectReferenceKind::Uri {
            scheme,
            host,
            port,
            resource_name,
        },
    })
}

fn valid_scheme_name(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

fn classify_uri_parse_error(raw: &str) -> RedirectReferenceError {
    let authority = raw
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?']).next().unwrap_or(rest))
        .unwrap_or_default();
    if authority.rsplit_once(':').is_some_and(|(_, port)| {
        !authority.ends_with(']')
            && (port.is_empty() || port.bytes().all(|byte| byte.is_ascii_digit()))
    }) {
        RedirectReferenceError::InvalidPort
    } else {
        RedirectReferenceError::InvalidUri
    }
}

fn validate_uri_host(host: &str) -> Result<String, RedirectReferenceError> {
    let bracketed = host.starts_with('[');
    let host = unbracket_host(host);
    if host.is_empty() {
        return Err(RedirectReferenceError::InvalidHost);
    }
    if bracketed {
        host.parse::<Ipv6Addr>()
            .map_err(|_| RedirectReferenceError::InvalidHost)?;
    } else if host.contains(':')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(RedirectReferenceError::InvalidHost);
    }
    Ok(host.to_ascii_lowercase())
}

fn parse_uri_port(
    authority: &str,
    parsed: Option<u16>,
) -> Result<Option<u16>, RedirectReferenceError> {
    let suffix = authority.rfind(']').map_or_else(
        || authority.rsplit_once(':').map_or("", |(_, suffix)| suffix),
        |bracket_end| &authority[bracket_end + 1..],
    );
    if authority.ends_with(':') || (!suffix.is_empty() && parsed.is_none()) {
        return Err(RedirectReferenceError::InvalidPort);
    }
    if parsed == Some(0) {
        return Err(RedirectReferenceError::InvalidPort);
    }
    Ok(parsed)
}

fn validate_percent_triplets(value: &str) -> Result<(), RedirectReferenceError> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(RedirectReferenceError::InvalidUri);
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn parse_authority(authority: &str) -> Result<RedirectReference, RedirectReferenceError> {
    if authority
        .chars()
        .any(|character| matches!(character, '/' | '?' | '#' | '@'))
    {
        return Err(RedirectReferenceError::NonAuthority);
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
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
        (host.to_ascii_lowercase(), port)
    } else {
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
        (host.to_ascii_lowercase(), port)
    };

    let kind = match parse_srv_owner(&host) {
        Some(_) if port.is_some() => return Err(RedirectReferenceError::SrvExplicitPort),
        Some(owner) => RedirectReferenceKind::Srv { owner },
        None => RedirectReferenceKind::Authority { host, port },
    };

    Ok(RedirectReference {
        raw: authority.to_owned(),
        kind,
    })
}

fn parse_srv_owner(host: &str) -> Option<SrvOwner> {
    let normalized = host.to_ascii_lowercase();
    let normalized = normalized.strip_suffix('.').unwrap_or(&normalized);
    let mut labels = normalized.split('.');
    let service = labels.next()?;
    let protocol = labels.next()?;
    let domain = labels.collect::<Vec<_>>();
    if normalized.len() > 253
        || !service.starts_with('_')
        || service.len() == 1
        || service.len() > 63
        || !service[1..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !protocol.eq_ignore_ascii_case("_tcp")
        || domain.is_empty()
        || !valid_dns_labels(&domain)
    {
        return None;
    }
    Some(SrvOwner {
        normalized: normalized.to_owned(),
    })
}

fn valid_dns_labels(labels: &[&str]) -> bool {
    let total = labels.iter().map(|label| label.len()).sum::<usize>() + labels.len() - 1;
    total <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
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

fn normalize_resource_name(resource: &str) -> String {
    let (path, query) = resource
        .split_once('?')
        .map_or((resource, None), |(path, query)| (path, Some(query)));
    let path = remove_dot_segments(if path.is_empty() { "/" } else { path });
    let mut normalized = normalize_percent_encoding(&path);
    if let Some(query) = query {
        normalized.push('?');
        normalized.push_str(&normalize_percent_encoding(query));
    }
    normalized
}

fn normalize_percent_encoding(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            let high = (bytes[index + 1] as char).to_digit(16).expect("hex digit");
            let low = (bytes[index + 2] as char).to_digit(16).expect("hex digit");
            let decoded = u8::try_from((high << 4) | low).expect("two hex digits fit in one byte");
            if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
                output.push(decoded as char);
            } else {
                output.push('%');
                output.push(char::from(bytes[index + 1]).to_ascii_uppercase());
                output.push(char::from(bytes[index + 2]).to_ascii_uppercase());
            }
            index += 3;
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    output
}

fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments = Vec::new();
    let source: Vec<_> = path.split('/').collect();
    for (index, segment) in source.iter().copied().enumerate() {
        if absolute && index == 0 {
            continue;
        }
        match segment {
            "." => {
                if index + 1 == source.len() {
                    segments.push("");
                }
            }
            ".." => {
                segments.pop();
                if index + 1 == source.len() {
                    segments.push("");
                }
            }
            segment => segments.push(segment),
        }
    }
    let mut output = if absolute {
        "/".to_owned()
    } else {
        String::new()
    };
    output.push_str(&segments.join("/"));
    if output.is_empty() {
        "/".to_owned()
    } else {
        output
    }
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

#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RedirectTargetError {
    #[error("Server Reference scheme {scheme:?} is incompatible with the selected transport")]
    TransportMismatch { scheme: Option<RedirectScheme> },
    #[error("Server Reference requires the disabled `websocket` feature")]
    WebsocketUnavailable,
    #[error("Server Reference requires an enabled TLS backend and explicit TLS configuration")]
    TlsUnavailable,
    #[error("Server Reference did not provide a selectable target")]
    NoTarget,
}

/// Application-approved connection profile for one advertised redirect target.
#[derive(Clone)]
pub struct RedirectTargetProfile {
    reference: RedirectReference,
    target: RedirectProfileTarget,
    transport: Transport,
    client_id: RedirectClientId,
    session: RedirectSession,
    authentication: Option<ConnectAuth>,
    reuse_authenticator: bool,
    reuse_network_credentials: bool,
}

#[derive(Clone, Debug)]
enum RedirectProfileTarget {
    Direct(Broker),
    SrvPending(SrvOwner),
}

impl RedirectTargetProfile {
    /// Construct a profile which clears authentication and starts an isolated clean session.
    ///
    /// # Errors
    ///
    /// Returns [`RedirectTargetError`] when the reference cannot be materialized by this build or
    /// is incompatible with `transport`.
    pub fn isolated(
        reference: RedirectReference,
        transport: Transport,
    ) -> Result<Self, RedirectTargetError> {
        reference.ensure_supported()?;
        if !reference.transport_matches_scheme(&transport) {
            return Err(RedirectTargetError::TransportMismatch {
                scheme: reference.scheme(),
            });
        }
        let target = if let Some(owner) = reference.srv_owner_value() {
            RedirectProfileTarget::SrvPending(owner)
        } else {
            let port = reference.effective_port(&transport).ok_or_else(|| {
                RedirectTargetError::TransportMismatch {
                    scheme: reference.scheme(),
                }
            })?;
            let broker = match reference.scheme() {
                None | Some(RedirectScheme::Mqtt | RedirectScheme::Mqtts) => {
                    Broker::tcp(reference.host().to_owned(), port)
                }
                #[cfg(feature = "websocket")]
                Some(RedirectScheme::Ws | RedirectScheme::Wss) => Broker::redirect_websocket(
                    reference.websocket_url().expect("websocket URI reference"),
                    matches!(reference.scheme(), Some(RedirectScheme::Wss)),
                ),
                #[cfg(not(feature = "websocket"))]
                Some(RedirectScheme::Ws | RedirectScheme::Wss) => unreachable!("checked above"),
            };
            if !broker_transport_matches(&broker, &transport) {
                return Err(RedirectTargetError::TransportMismatch {
                    scheme: reference.scheme(),
                });
            }
            RedirectProfileTarget::Direct(broker)
        };
        Ok(Self {
            reference,
            target,
            transport,
            client_id: RedirectClientId::Fresh,
            session: RedirectSession::Isolated,
            authentication: None,
            reuse_authenticator: false,
            reuse_network_credentials: false,
        })
    }

    #[must_use]
    pub const fn broker(&self) -> Option<&Broker> {
        match &self.target {
            RedirectProfileTarget::Direct(broker) => Some(broker),
            RedirectProfileTarget::SrvPending(_) => None,
        }
    }

    pub(super) const fn srv_owner(&self) -> Option<&SrvOwner> {
        match &self.target {
            RedirectProfileTarget::Direct(_) => None,
            RedirectProfileTarget::SrvPending(owner) => Some(owner),
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

    #[must_use]
    pub fn reuse_authentication(mut self, auth: ConnectAuth) -> Self {
        self.authentication = Some(auth);
        self.reuse_authenticator = true;
        self
    }

    #[must_use]
    pub fn authentication(mut self, auth: ConnectAuth) -> Self {
        self.authentication = Some(auth);
        self.reuse_authenticator = false;
        self
    }

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

impl RedirectReference {
    fn transport_matches_scheme(&self, transport: &Transport) -> bool {
        match self.scheme() {
            None => {
                matches!(transport, Transport::Tcp)
                    || cfg!(any(
                        feature = "use-rustls-no-provider",
                        feature = "use-native-tls"
                    )) && transport.redirect_identity() == "tls"
            }
            Some(RedirectScheme::Mqtt) => matches!(transport, Transport::Tcp),
            Some(RedirectScheme::Mqtts) => transport.redirect_identity() == "tls",
            Some(RedirectScheme::Ws) => transport.redirect_identity() == "ws",
            Some(RedirectScheme::Wss) => transport.redirect_identity() == "wss",
        }
    }
}

impl Debug for RedirectTargetProfile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedirectTargetProfile")
            .field("reference", &self.reference)
            .field("target", &self.target)
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

pub type RedirectPolicyResult = Result<RedirectDecision, RedirectTargetError>;
type RedirectHandler = dyn for<'a> Fn(&RedirectContext<'a>) -> RedirectPolicyResult + Send + Sync;

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
            handler: Arc::new(move |context| Ok(handler(context))),
        }
    }

    #[must_use]
    pub fn try_new<F>(max_attempts: NonZeroUsize, handler: F) -> Self
    where
        F: for<'a> Fn(&RedirectContext<'a>) -> RedirectPolicyResult + Send + Sync + 'static,
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

    pub(crate) fn decide(&self, context: &RedirectContext<'_>) -> RedirectPolicyResult {
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

#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RedirectFailure {
    #[error("automatic redirects are disabled")]
    Disabled,
    #[error("redirect was rejected by application policy")]
    Rejected,
    #[error("invalid Server Reference: {0}")]
    InvalidReference(#[from] RedirectReferenceError),
    #[error("redirect target could not be materialized: {0}")]
    Target(#[from] RedirectTargetError),
    #[error("redirect policy selected a target not present in Server Reference")]
    UnadvertisedTarget,
    #[error("redirect loop detected")]
    Loop,
    #[error("redirect attempt limit reached")]
    AttemptLimit,
    #[error("no DNS SRV resolver is available for `{owner}`")]
    SrvResolverUnavailable { owner: String },
    #[error("SRV lookup for `{owner}` failed: {source}")]
    SrvLookup {
        owner: String,
        #[source]
        source: SrvLookupError,
    },
    #[error("SRV lookup for `{owner}` timed out")]
    SrvLookupTimeout { owner: String },
    #[error("SRV owner `{owner}` explicitly reports that the service is unavailable")]
    SrvServiceUnavailable { owner: String },
    #[error("SRV answer for `{owner}` contains {count} usable targets; the limit is {max}")]
    SrvAnswerTooLarge {
        owner: String,
        count: usize,
        max: usize,
    },
    #[error("SRV answer for `{owner}` contains no usable targets ({rejected} malformed records)")]
    SrvNoUsableTargets { owner: String, rejected: usize },
    #[error("every SRV target for `{owner}` was already visited")]
    SrvAllTargetsVisited { owner: String },
    #[error("all {attempted} SRV targets for `{owner}` failed: {last_error}")]
    SrvTargetsExhausted {
        owner: String,
        attempted: usize,
        #[source]
        last_error: Box<crate::ConnectionError>,
    },
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

    #[cfg(feature = "use-rustls-no-provider")]
    fn test_tls_transport() -> Transport {
        Transport::tls(Vec::new(), None, None)
    }

    #[cfg(all(feature = "use-native-tls", not(feature = "use-rustls-no-provider")))]
    fn test_tls_transport() -> Transport {
        Transport::tls_with_default_config()
    }

    #[cfg(all(feature = "websocket", feature = "use-rustls-no-provider"))]
    fn test_wss_transport() -> Transport {
        Transport::wss(Vec::new(), None, None)
    }

    #[cfg(all(
        feature = "websocket",
        feature = "use-native-tls",
        not(feature = "use-rustls-no-provider")
    ))]
    fn test_wss_transport() -> Transport {
        Transport::wss_with_default_config()
    }

    #[test]
    fn parses_authorities_and_absolute_uris() {
        let references = parse_server_references(Some(
            "myserver.xyz.org myserver.xyz.org:8883 10.10.151.22:8883 [fe80::9610:3eff:fe1c]:1883 MQTT://BROKER.EXAMPLE mqtts://secure.example/ ws://socket.example/mqtt/v5?tenant=green wss://[2001:db8::1]:8443",
        ))
        .unwrap();
        assert_eq!(references.len(), 8);
        assert_eq!(references[0].host(), "myserver.xyz.org");
        assert_eq!(references[3].host(), "fe80::9610:3eff:fe1c");
        assert_eq!(references[4].scheme(), Some(RedirectScheme::Mqtt));
        assert_eq!(references[4].port(), None);
        assert_eq!(
            references[6].websocket_resource_name(),
            Some("/mqtt/v5?tenant=green")
        );
        assert_eq!(references[7].host(), "2001:db8::1");
        assert_eq!(references[7].websocket_resource_name(), Some("/"));
    }

    #[test]
    fn recognizes_complete_tcp_srv_authorities() {
        let reference = &parse_server_references(Some("_mqtt._tcp.example.com")).unwrap()[0];
        assert!(reference.is_srv_name());
        assert_eq!(reference.srv_owner(), Some("_mqtt._tcp.example.com"));
        assert_eq!(reference.ensure_supported(), Ok(()));
        assert!(!parse_server_references(Some("broker.example")).unwrap()[0].is_srv_name());
    }

    #[test]
    fn classifies_only_complete_usable_srv_authorities() {
        for (input, expected_owner) in [
            (
                "_MQTT._TCP.Cluster.Example.COM.",
                Some("_mqtt._tcp.cluster.example.com"),
            ),
            (
                "_custom-1._tcp.example.com",
                Some("_custom-1._tcp.example.com"),
            ),
            ("_mqtt._udp.example.com", None),
            ("_mqtt._tcp", None),
            ("_mqtt._tcp..example.com", None),
            ("_mqtt.example.com", None),
            ("broker_name.example.com", None),
        ] {
            let reference = parse_server_references(Some(input)).unwrap().remove(0);
            assert_eq!(reference.srv_owner(), expected_owner, "{input}");
        }

        assert_eq!(
            parse_server_references(Some("_mqtt._tcp.example.com:1883")),
            Err(RedirectReferenceError::SrvExplicitPort)
        );
    }

    #[test]
    fn srv_profile_is_deferred_without_a_placeholder_broker() {
        let reference = parse_server_references(Some("_mqtt._tcp.example.com"))
            .unwrap()
            .remove(0);
        let profile = RedirectTargetProfile::isolated(reference, Transport::tcp()).unwrap();
        assert!(profile.broker().is_none());
        assert_eq!(
            profile.srv_owner().unwrap().as_str(),
            "_mqtt._tcp.example.com"
        );
    }

    #[test]
    fn applies_scheme_specific_resource_rules() {
        for accepted in [
            "mqtt://broker.example",
            "mqtt://broker.example/",
            "mqtts://broker.example/",
            "ws://broker.example",
            "ws://broker.example?tenant=green",
            "wss://broker.example/mqtt?tenant=green",
        ] {
            parse_server_references(Some(accepted))
                .unwrap_or_else(|error| panic!("{accepted}: {error}"));
        }
        for rejected in [
            "mqtt://broker.example/mqtt",
            "mqtt://broker.example?tenant=green",
            "mqtts://broker.example/#fragment",
        ] {
            assert!(
                parse_server_references(Some(rejected)).is_err(),
                "{rejected}"
            );
        }

        let reference = parse_server_references(Some("WS://BROKER.EXAMPLE?tenant=green"))
            .unwrap()
            .remove(0);
        assert_eq!(reference.raw(), "WS://BROKER.EXAMPLE?tenant=green");
        assert_eq!(reference.scheme(), Some(RedirectScheme::Ws));
        assert_eq!(reference.host(), "broker.example");
        assert_eq!(reference.websocket_resource_name(), Some("/?tenant=green"));
    }

    #[test]
    fn rejects_invalid_uri_security_components() {
        assert_eq!(
            parse_server_references(Some("ftp://broker.example")),
            Err(RedirectReferenceError::UnsupportedScheme("ftp".to_owned()))
        );
        for invalid in [
            "ws://user@broker.example/mqtt",
            "ws://broker.example/#fragment",
            "ws://broker.example:0/mqtt",
            "ws://broker.example:/mqtt",
            "ws://broker.example:65536/mqtt",
            "ws://fe80::1/mqtt",
            "ws://broker_example/mqtt",
            "ws://broker.example/bad%2",
            "ws://broker.example\\mqtt",
        ] {
            assert!(parse_server_references(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn normalizes_websocket_loop_resources() {
        assert_eq!(
            normalize_resource_name("/a/./b/../c/%7euser?x=%2f"),
            "/a/c/~user?x=%2F"
        );
        assert_eq!(normalize_resource_name("//a///b"), "//a///b");
    }

    #[test]
    fn materializes_mqtt_and_authority_profiles() {
        let mqtt = parse_server_references(Some("mqtt://BROKER.EXAMPLE"))
            .unwrap()
            .remove(0);
        let profile = RedirectTargetProfile::isolated(mqtt, Transport::tcp()).unwrap();
        assert_eq!(
            profile.broker().unwrap().tcp_address(),
            Some(("broker.example", 1883))
        );

        let authority = parse_server_references(Some("broker.example"))
            .unwrap()
            .remove(0);
        let profile = RedirectTargetProfile::isolated(authority, Transport::tcp()).unwrap();
        assert_eq!(
            profile.broker().unwrap().tcp_address(),
            Some(("broker.example", 1883))
        );

        let mqtts = parse_server_references(Some("mqtts://broker.example"))
            .unwrap()
            .remove(0);
        assert!(matches!(
            RedirectTargetProfile::isolated(mqtts, Transport::tcp()),
            Err(RedirectTargetError::TransportMismatch {
                scheme: Some(RedirectScheme::Mqtts)
            } | RedirectTargetError::TlsUnavailable)
        ));
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn materializes_websocket_profile_with_exact_resource() {
        let reference = parse_server_references(Some(
            "ws://BROKER.EXAMPLE:8080/mqtt/%7ev5?tenant=green%2fblue",
        ))
        .unwrap()
        .remove(0);
        let profile = RedirectTargetProfile::isolated(reference, Transport::ws()).unwrap();
        assert_eq!(
            profile.broker().unwrap().websocket_url(),
            Some("ws://broker.example:8080/mqtt/%7ev5?tenant=green%2fblue")
        );
        assert!(matches!(
            RedirectTargetProfile::isolated(
                parse_server_references(Some("ws://broker.example"))
                    .unwrap()
                    .remove(0),
                Transport::tcp()
            ),
            Err(RedirectTargetError::TransportMismatch {
                scheme: Some(RedirectScheme::Ws)
            })
        ));
    }

    #[cfg(not(feature = "websocket"))]
    #[test]
    fn reports_disabled_websocket_feature_when_selected() {
        let reference = parse_server_references(Some("ws://broker.example/mqtt"))
            .unwrap()
            .remove(0);
        assert_eq!(
            RedirectTargetProfile::isolated(reference, Transport::tcp()).unwrap_err(),
            RedirectTargetError::WebsocketUnavailable
        );
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[test]
    fn materializes_secure_mqtt_with_explicit_tls() {
        let mqtt = parse_server_references(Some("mqtt://plain.example"))
            .unwrap()
            .remove(0);
        let reference = parse_server_references(Some("mqtts://secure.example"))
            .unwrap()
            .remove(0);
        let transport = test_tls_transport();
        assert!(matches!(
            RedirectTargetProfile::isolated(mqtt, transport.clone()),
            Err(RedirectTargetError::TransportMismatch {
                scheme: Some(RedirectScheme::Mqtt)
            })
        ));
        let profile = RedirectTargetProfile::isolated(reference, transport).unwrap();
        assert_eq!(
            profile.broker().unwrap().tcp_address(),
            Some(("secure.example", 8883))
        );
    }

    #[cfg(all(
        feature = "websocket",
        any(feature = "use-rustls-no-provider", feature = "use-native-tls")
    ))]
    #[test]
    fn materializes_secure_websocket_with_explicit_tls() {
        let reference = parse_server_references(Some("wss://secure.example/mqtt?tenant=green"))
            .unwrap()
            .remove(0);
        let transport = test_wss_transport();
        let profile = RedirectTargetProfile::isolated(reference, transport).unwrap();
        assert_eq!(
            profile.broker().unwrap().websocket_url(),
            Some("wss://secure.example/mqtt?tenant=green")
        );
    }

    #[cfg(not(any(feature = "use-rustls-no-provider", feature = "use-native-tls")))]
    #[test]
    fn reports_disabled_tls_feature_when_selected() {
        let reference = parse_server_references(Some("mqtts://secure.example"))
            .unwrap()
            .remove(0);
        assert_eq!(
            RedirectTargetProfile::isolated(reference, Transport::tcp()).unwrap_err(),
            RedirectTargetError::TlsUnavailable
        );
    }
}
