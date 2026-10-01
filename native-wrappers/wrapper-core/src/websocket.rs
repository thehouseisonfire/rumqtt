use crate::{Error, Result};

/// Declarative handshake edits, applied in order to each new connection's
/// request. Values are always redacted, including nonstandard credential headers.
#[derive(Clone, PartialEq, Eq)]
pub enum WebSocketHeader {
    Append { name: String, value: String },
    Replace { name: String, value: String },
    Remove { name: String },
}

impl std::fmt::Debug for WebSocketHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketHeader([REDACTED])")
    }
}

impl WebSocketHeader {
    /// Validate a static edit before opening a connection.
    ///
    /// # Errors
    /// Rejects disabled WebSocket support, invalid fields, or protected headers.
    pub fn validate(&self) -> Result<()> {
        if !cfg!(feature = "websocket") {
            return Err(Error::configuration("WebSocket feature is disabled"));
        }
        #[cfg(feature = "websocket")]
        self.parse()?;
        Ok(())
    }

    #[cfg(feature = "websocket")]
    fn parse(&self) -> Result<(http::HeaderName, Option<http::HeaderValue>)> {
        let (name, value) = match self {
            Self::Append { name, value } | Self::Replace { name, value } => (name, Some(value)),
            Self::Remove { name } => (name, None),
        };
        let name = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::configuration("invalid WebSocket header name"))?;
        if matches!(
            name.as_str(),
            "host" | "connection" | "upgrade" | "content-length" | "transfer-encoding"
        ) || name.as_str().starts_with("sec-websocket-")
        {
            return Err(Error::configuration(
                "cannot modify protected WebSocket headers",
            ));
        }
        let value = value
            .map(|v| {
                let mut value = http::HeaderValue::from_str(v)
                    .map_err(|_| Error::configuration("invalid WebSocket header value"))?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        Ok((name, value))
    }
}

#[cfg(feature = "websocket")]
pub fn prepare(
    headers: &[WebSocketHeader],
) -> Result<
    impl Fn(http::Request<()>) -> std::future::Ready<http::Request<()>> + Send + Sync + 'static,
> {
    let parsed: Vec<_> = headers
        .iter()
        .map(|header| {
            let (name, value) = header.parse()?;
            Ok((
                matches!(header, WebSocketHeader::Append { .. }),
                name,
                value,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(move |mut request: http::Request<()>| {
        for (append, name, value) in &parsed {
            match value {
                Some(value) if *append => {
                    request.headers_mut().append(name.clone(), value.clone());
                }
                Some(value) => {
                    request.headers_mut().insert(name.clone(), value.clone());
                }
                None => {
                    request.headers_mut().remove(name);
                }
            }
        }
        std::future::ready(request)
    })
}

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub const MAX_WEBSOCKET_HEADERS: usize = 128;
pub const MAX_WEBSOCKET_PATH: usize = 8 * 1024;
pub const MAX_WEBSOCKET_BYTES: usize = 64 * 1024;
pub const MAX_WEBSOCKET_EDITS: usize = 256;

/// Fixed diagnostics never contain host-supplied text or credential values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[repr(u32)]
pub enum WebSocketHandshakeFailure {
    #[error("WebSocket handshake rejected")]
    Rejected = 1,
    #[error("WebSocket handshake response abandoned")]
    Abandoned = 2,
    #[error("invalid WebSocket handshake response")]
    InvalidResponse = 3,
    #[error("WebSocket handshake resource limit exceeded")]
    ResourceLimit = 4,
    #[error("WebSocket handshake timed out")]
    Timeout = 5,
    #[error("WebSocket handshake callback panicked")]
    Panic = 6,
}

impl WebSocketHandshakeFailure {
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Rejected | Self::Abandoned | Self::Timeout)
    }
}

#[derive(Clone)]
pub struct WebSocketRequestHeader {
    pub name: String,
    pub value: bytes::Bytes,
}

impl std::fmt::Debug for WebSocketRequestHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketRequestHeader([REDACTED])")
    }
}

/// Owned snapshot after static edits. Header names are sorted, with duplicate
/// values retaining their per-name order. This is not a wire serialization.
#[derive(Clone)]
pub struct WebSocketHandshakeRequest {
    pub protocol: crate::ProtocolVersion,
    pub client_id: String,
    pub attempt: u64,
    pub method: String,
    pub version: String,
    pub uri: String,
    pub path_and_query: String,
    pub headers: Vec<WebSocketRequestHeader>,
    pub broker_host: String,
    pub broker_port: u16,
    pub dial_target: String,
    pub tls_authority: Option<String>,
    pub deadline: std::time::Instant,
}

impl std::fmt::Debug for WebSocketHandshakeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketHandshakeRequest([REDACTED])")
    }
}

#[cfg_attr(not(feature = "websocket"), allow(dead_code))]
#[derive(Clone)]
enum DynamicHeader {
    Append(String, bytes::Bytes),
    Replace(String, bytes::Bytes),
    Remove(String),
}

/// Owned, bounded patch of a prepared request. Upgrade fields remain read-only.
#[derive(Clone, Default)]
pub struct WebSocketHandshakeResponse {
    authority: Option<String>,
    path_and_query: Option<String>,
    edits: Vec<DynamicHeader>,
    bytes: usize,
}

impl std::fmt::Debug for WebSocketHandshakeResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketHandshakeResponse([REDACTED])")
    }
}

impl WebSocketHandshakeResponse {
    /// Set the HTTP authority and Host header together, preserving the scheme.
    ///
    /// Accepts an ASCII host or bracketed IPv6 with an optional numeric port in
    /// `0..=65535`, without user information. Replaces any previous override;
    /// a failed call leaves the response unchanged.
    ///
    /// This does not change the configured broker, TCP or proxy destination,
    /// TLS server name, or certificate verification identity.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid authorities or `ResourceLimit` if a limit is exceeded.
    pub fn set_authority(
        &mut self,
        value: &str,
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        if value.len() > MAX_WEBSOCKET_BYTES {
            return Err(WebSocketHandshakeFailure::ResourceLimit);
        }
        validate_authority(value)?;
        let bytes = self.bytes - self.authority.as_ref().map_or(0, String::len) + value.len();
        if bytes > MAX_WEBSOCKET_BYTES {
            return Err(WebSocketHandshakeFailure::ResourceLimit);
        }
        self.authority = Some(value.into());
        self.bytes = bytes;
        Ok(())
    }

    /// Set an origin-form path and query without changing the endpoint.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid paths or `ResourceLimit` if a limit is exceeded.
    pub fn set_path_and_query(
        &mut self,
        value: &str,
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        if value.len() > MAX_WEBSOCKET_PATH {
            return Err(WebSocketHandshakeFailure::ResourceLimit);
        }
        if !value.starts_with('/') || value.contains('#') {
            return Err(WebSocketHandshakeFailure::InvalidResponse);
        }
        #[cfg(feature = "websocket")]
        value
            .parse::<http::uri::PathAndQuery>()
            .map_err(|_| WebSocketHandshakeFailure::InvalidResponse)?;
        let bytes = self.bytes - self.path_and_query.as_ref().map_or(0, String::len) + value.len();
        if bytes > MAX_WEBSOCKET_BYTES {
            return Err(WebSocketHandshakeFailure::ResourceLimit);
        }
        self.path_and_query = Some(value.into());
        self.bytes = bytes;
        Ok(())
    }

    /// Append a value, preserving existing values for this name.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid or protected fields, or `ResourceLimit` if a limit is exceeded.
    pub fn append_header(
        &mut self,
        name: &str,
        value: &[u8],
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        self.edit(name, Some(value), true)
    }

    /// Replace all values for a header name.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid or protected fields, or `ResourceLimit` if a limit is exceeded.
    pub fn replace_header(
        &mut self,
        name: &str,
        value: &[u8],
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        self.edit(name, Some(value), false)
    }

    /// Remove all values for a header name.
    ///
    /// # Errors
    /// Returns `InvalidResponse` for invalid or protected names, or `ResourceLimit` if a limit is exceeded.
    pub fn remove_header(
        &mut self,
        name: &str,
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        self.edit(name, None, false)
    }

    fn edit(
        &mut self,
        name: &str,
        value: Option<&[u8]>,
        append: bool,
    ) -> std::result::Result<(), WebSocketHandshakeFailure> {
        let bytes = self
            .bytes
            .checked_add(name.len())
            .and_then(|n| n.checked_add(value.map_or(0, <[u8]>::len)))
            .ok_or(WebSocketHandshakeFailure::ResourceLimit)?;
        if bytes > MAX_WEBSOCKET_BYTES || self.edits.len() >= MAX_WEBSOCKET_EDITS {
            return Err(WebSocketHandshakeFailure::ResourceLimit);
        }
        let lower = name.to_ascii_lowercase();
        if protected(&lower) || lower.is_empty() {
            return Err(WebSocketHandshakeFailure::InvalidResponse);
        }
        #[cfg(feature = "websocket")]
        {
            http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| WebSocketHandshakeFailure::InvalidResponse)?;
            if let Some(value) = value {
                http::HeaderValue::from_bytes(value)
                    .map_err(|_| WebSocketHandshakeFailure::InvalidResponse)?;
            }
        }
        self.edits.push(match value {
            Some(value) if append => {
                DynamicHeader::Append(lower, bytes::Bytes::copy_from_slice(value))
            }
            Some(value) => DynamicHeader::Replace(lower, bytes::Bytes::copy_from_slice(value)),
            None => DynamicHeader::Remove(lower),
        });
        self.bytes = bytes;
        Ok(())
    }
}

fn validate_authority(value: &str) -> std::result::Result<(), WebSocketHandshakeFailure> {
    use WebSocketHandshakeFailure::InvalidResponse;

    // Keep builder validation consistent even when WebSocket support is disabled.
    let port = if let Some(ipv6) = value.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']').ok_or(InvalidResponse)?;
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| InvalidResponse)?;
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(InvalidResponse)?)
        }
    } else {
        let (host, port) = value
            .split_once(':')
            .map_or((value, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&b))
        {
            return Err(InvalidResponse);
        }
        port
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InvalidResponse);
        }
        port.parse::<u16>().map_err(|_| InvalidResponse)?;
    }
    #[cfg(feature = "websocket")]
    value
        .parse::<http::uri::Authority>()
        .map_err(|_| InvalidResponse)?;
    Ok(())
}

fn protected(name: &str) -> bool {
    matches!(
        name,
        "host" | "connection" | "upgrade" | "content-length" | "transfer-encoding"
    ) || name.starts_with("sec-websocket-")
}

pub type WebSocketHandshakeFuture = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<WebSocketHandshakeResponse, WebSocketHandshakeFailure>,
            > + Send
            + 'static,
    >,
>;

/// One call per prepared handshake, serialized per client. Shared registrations
/// may be called concurrently by different clients.
///
/// Return promptly: waiting
/// for MQTT progress on this driver prevents the handshake from advancing.
/// Dropping the returned future cancels deferred work. Destructors must not block.
pub trait WebSocketHandshake: Send + Sync + 'static {
    fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture;
}

#[derive(Clone)]
pub struct WebSocketHandshakeConfig(pub Arc<dyn WebSocketHandshake>);

impl std::fmt::Debug for WebSocketHandshakeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebSocketHandshakeConfig([REDACTED])")
    }
}
impl PartialEq for WebSocketHandshakeConfig {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for WebSocketHandshakeConfig {}

#[derive(Default)]
pub struct HandshakeMonitor(parking_lot::Mutex<Option<WebSocketHandshakeFailure>>);
impl HandshakeMonitor {
    pub(crate) fn reset(&self) {
        *self.0.lock() = None;
    }
    pub(crate) fn failure(&self) -> Option<WebSocketHandshakeFailure> {
        *self.0.lock()
    }
    #[cfg(feature = "websocket")]
    fn fail(&self, failure: WebSocketHandshakeFailure) {
        if failure == WebSocketHandshakeFailure::Panic {
            *self.0.lock() = Some(failure);
        } else {
            self.0.lock().get_or_insert(failure);
        }
    }
}

#[cfg(feature = "websocket")]
mod dynamic {
    use super::{
        Arc, DynamicHeader, Future, HandshakeMonitor, MAX_WEBSOCKET_BYTES, MAX_WEBSOCKET_HEADERS,
        MAX_WEBSOCKET_PATH, Pin, Result, WebSocketHandshakeConfig, WebSocketHandshakeFailure,
        WebSocketHandshakeFuture, WebSocketHandshakeRequest, WebSocketHandshakeResponse,
        WebSocketHeader, WebSocketRequestHeader,
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll};

    type Failure = WebSocketHandshakeFailure;

    struct Work {
        future: Option<WebSocketHandshakeFuture>,
        monitor: Arc<HandshakeMonitor>,
    }
    impl Work {
        fn destroy(&mut self) -> std::result::Result<(), Failure> {
            if let Some(future) = self.future.take()
                && let Err(payload) = catch_unwind(AssertUnwindSafe(|| {
                    crate::runtime::with_host_callback(|| drop(future));
                }))
            {
                std::mem::forget(payload);
                self.monitor.fail(Failure::Panic);
                return Err(Failure::Panic);
            }
            Ok(())
        }
    }
    impl Future for Work {
        type Output = std::result::Result<WebSocketHandshakeResponse, Failure>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            match catch_unwind(AssertUnwindSafe(|| {
                crate::runtime::with_host_callback(|| {
                    this.future
                        .as_mut()
                        .expect("handshake work present")
                        .as_mut()
                        .poll(cx)
                })
            })) {
                Ok(result) => result,
                Err(payload) => {
                    std::mem::forget(payload);
                    Poll::Ready(Err(Failure::Panic))
                }
            }
        }
    }
    impl Drop for Work {
        fn drop(&mut self) {
            let _ = self.destroy();
        }
    }

    struct Attempt {
        deadline: std::time::Instant,
        monitor: Arc<HandshakeMonitor>,
    }
    impl Drop for Attempt {
        fn drop(&mut self) {
            if std::time::Instant::now() >= self.deadline {
                self.monitor.fail(Failure::Timeout);
            }
        }
    }

    fn validate(request: &http::Request<()>) -> std::result::Result<(), Failure> {
        if request.method() != http::Method::GET || request.version() != http::Version::HTTP_11 {
            return Err(Failure::InvalidResponse);
        }
        let path = request
            .uri()
            .path_and_query()
            .ok_or(Failure::InvalidResponse)?;
        if path.as_str().len() > MAX_WEBSOCKET_PATH
            || request.headers().len() > MAX_WEBSOCKET_HEADERS
        {
            return Err(Failure::ResourceLimit);
        }
        let mut bytes = request.uri().to_string().len() + 16;
        for (name, value) in request.headers() {
            bytes = bytes
                .checked_add(name.as_str().len() + value.as_bytes().len() + 4)
                .ok_or(Failure::ResourceLimit)?;
        }
        if bytes > MAX_WEBSOCKET_BYTES {
            return Err(Failure::ResourceLimit);
        }
        Ok(())
    }

    fn snapshot(
        request: &http::Request<()>,
        context: crate::backend::WebSocketRequestContext,
        protocol: crate::ProtocolVersion,
        client_id: String,
        attempt: u64,
        deadline: std::time::Instant,
    ) -> std::result::Result<WebSocketHandshakeRequest, Failure> {
        let mut headers: Vec<_> = request
            .headers()
            .iter()
            .map(|(name, value)| WebSocketRequestHeader {
                name: name.as_str().into(),
                value: bytes::Bytes::copy_from_slice(value.as_bytes()),
            })
            .collect();
        headers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(WebSocketHandshakeRequest {
            protocol,
            client_id,
            attempt,
            method: "GET".into(),
            version: "HTTP/1.1".into(),
            uri: request.uri().to_string(),
            path_and_query: request
                .uri()
                .path_and_query()
                .ok_or(Failure::InvalidResponse)?
                .as_str()
                .into(),
            headers,
            broker_host: context.broker_host,
            broker_port: context.broker_port,
            dial_target: context.dial_target,
            tls_authority: context.tls_authority,
            deadline,
        })
    }

    fn apply_response(
        request: &mut http::Request<()>,
        response: WebSocketHandshakeResponse,
    ) -> std::result::Result<(), Failure> {
        if response.authority.is_some() || response.path_and_query.is_some() {
            let mut parts = request.uri().clone().into_parts();
            let host = response
                .authority
                .map(|authority| {
                    parts.authority =
                        Some(authority.parse().map_err(|_| Failure::InvalidResponse)?);
                    let mut host = http::HeaderValue::from_str(&authority)
                        .map_err(|_| Failure::InvalidResponse)?;
                    host.set_sensitive(true);
                    Ok::<_, Failure>(host)
                })
                .transpose()?;
            if let Some(path) = response.path_and_query {
                parts.path_and_query = Some(path.parse().map_err(|_| Failure::InvalidResponse)?);
            }
            let uri = http::Uri::from_parts(parts).map_err(|_| Failure::InvalidResponse)?;
            *request.uri_mut() = uri;
            if let Some(host) = host {
                request.headers_mut().insert(http::header::HOST, host);
            }
        }
        for edit in response.edits {
            let (name, value, append) = match edit {
                DynamicHeader::Append(name, value) => (name, Some(value), true),
                DynamicHeader::Replace(name, value) => (name, Some(value), false),
                DynamicHeader::Remove(name) => (name, None, false),
            };
            let name = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Failure::InvalidResponse)?;
            match value {
                Some(value) => {
                    let mut value = http::HeaderValue::from_bytes(&value)
                        .map_err(|_| Failure::InvalidResponse)?;
                    value.set_sensitive(true);
                    if append {
                        request.headers_mut().append(name, value);
                    } else {
                        request.headers_mut().insert(name, value);
                    }
                }
                None => {
                    request.headers_mut().remove(name);
                }
            }
        }
        validate(request)?;
        Ok(())
    }

    type ModifierFuture =
        Pin<Box<dyn Future<Output = std::result::Result<http::Request<()>, Failure>> + Send>>;

    pub fn prepare_dynamic(
        headers: &[WebSocketHeader],
        config: WebSocketHandshakeConfig,
        protocol: crate::ProtocolVersion,
        client_id: String,
        monitor: Arc<HandshakeMonitor>,
    ) -> Result<impl Fn(http::Request<()>) -> ModifierFuture + Send + Sync + 'static> {
        let static_edits = super::prepare(headers)?;
        let attempts = AtomicU64::new(0);
        Ok(move |request: http::Request<()>| -> ModifierFuture {
            let prepared = static_edits(request);
            let config = config.clone();
            let client_id = client_id.clone();
            let monitor = monitor.clone();
            #[allow(deprecated)]
            let attempt = attempts
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map(|n| n + 1);
            Box::pin(async move {
                let result = async {
                    let mut request = prepared.await;
                    validate(&request)?;
                    let context = request
                        .extensions()
                        .get::<crate::backend::WebSocketRequestContext>()
                        .ok_or(Failure::InvalidResponse)?
                        .clone();
                    let deadline = context.deadline.ok_or(Failure::InvalidResponse)?;
                    let _attempt = Attempt {
                        deadline,
                        monitor: monitor.clone(),
                    };
                    if std::time::Instant::now() >= deadline {
                        return Err(Failure::Timeout);
                    }
                    let snapshot = snapshot(
                        &request,
                        context,
                        protocol,
                        client_id,
                        attempt.map_err(|_| Failure::ResourceLimit)?,
                        deadline,
                    )?;
                    let future = catch_unwind(AssertUnwindSafe(|| {
                        crate::runtime::with_host_callback(|| config.0.prepare(snapshot))
                    }))
                    .map_err(|payload| {
                        std::mem::forget(payload);
                        Failure::Panic
                    })?;
                    let mut work = Work {
                        future: Some(future),
                        monitor: monitor.clone(),
                    };
                    let response = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        &mut work,
                    )
                    .await
                    .unwrap_or(Err(Failure::Timeout));
                    work.destroy()?;
                    let response = match response {
                        Err(Failure::Panic) => return Err(Failure::Panic),
                        _ if std::time::Instant::now() >= deadline => return Err(Failure::Timeout),
                        response => response?,
                    };
                    apply_response(&mut request, response)?;
                    if std::time::Instant::now() >= deadline {
                        return Err(Failure::Timeout);
                    }
                    Ok(request)
                }
                .await;
                if let Err(failure) = result {
                    monitor.fail(failure);
                }
                result
            })
        })
    }
}

#[cfg(feature = "websocket")]
pub use dynamic::prepare_dynamic;

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn authority_validation_accepts_hosts_ports_and_ipv6_without_user_info() {
        for value in [
            "mqtt.customer.example",
            "localhost",
            "192.0.2.1:80",
            "host:0",
            "host:65535",
            "host:00443",
            "[::1]",
            "[2001:db8::1]:443",
        ] {
            WebSocketHandshakeResponse::default()
                .set_authority(value)
                .unwrap();
        }
        let mut response = WebSocketHandshakeResponse::default();
        response.set_authority("original.example:443").unwrap();
        for value in [
            "",
            ":443",
            "user@host",
            "user:password@host",
            "ws://host",
            "//host",
            "host/path",
            "host?query",
            "host#fragment",
            "host ",
            "host\r\nInjected: value",
            "host:",
            "host:+80",
            "host:-1",
            "host:65536",
            "host:port",
            "host:80:90",
            "::1",
            "[]",
            "[invalid]",
            "[::1",
            "[::1]suffix",
            "[::1]:",
            "[::1]:65536",
            "[fe80::1%25eth0]",
            "mütt.example",
        ] {
            assert_eq!(
                response.set_authority(value),
                Err(WebSocketHandshakeFailure::InvalidResponse),
                "{value:?}"
            );
            assert_eq!(response.authority.as_deref(), Some("original.example:443"));
        }
        assert!(!format!("{response:?}").contains("original.example"));
    }

    #[test]
    fn replacing_authority_releases_its_previous_byte_budget_and_failures_are_atomic() {
        let mut response = WebSocketHandshakeResponse::default();
        response.set_authority("original.example").unwrap();
        response.set_authority("a").unwrap();
        response
            .append_header("x", &vec![b'a'; MAX_WEBSOCKET_BYTES - 2])
            .unwrap();
        assert_eq!(
            response.set_authority("longer.example"),
            Err(WebSocketHandshakeFailure::ResourceLimit)
        );
        assert_eq!(response.authority.as_deref(), Some("a"));
        assert_eq!(
            response.set_authority(&"a".repeat(MAX_WEBSOCKET_BYTES + 1)),
            Err(WebSocketHandshakeFailure::ResourceLimit)
        );
        response.set_authority("b").unwrap();
    }
}

#[cfg(all(test, feature = "websocket"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn request(deadline: Instant) -> http::Request<()> {
        let mut request = http::Request::builder()
            .method("GET")
            .version(http::Version::HTTP_11)
            .uri("ws://broker.test/mqtt")
            .header("host", "broker.test")
            .body(())
            .unwrap();
        request
            .extensions_mut()
            .insert(crate::backend::WebSocketRequestContext {
                broker_host: "broker.test".into(),
                broker_port: 80,
                dial_target: "proxy.test:8080".into(),
                tls_authority: None,
                deadline: Some(deadline),
            });
        request
    }

    struct AuthorityOverride(WebSocketHandshakeResponse);
    impl WebSocketHandshake for AuthorityOverride {
        fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
            assert_eq!(request.broker_host, "broker.test");
            assert_eq!(request.dial_target, "proxy.test:8080");
            assert_eq!(request.uri, "ws://broker.test/mqtt");
            Box::pin(std::future::ready(Ok(self.0.clone())))
        }
    }

    #[tokio::test]
    async fn authority_and_host_change_together_with_path_while_endpoint_metadata_is_preserved() {
        for (authority, path) in [
            ("mqtt.customer.example", Some("/signed?encoded=%2F")),
            ("192.0.2.1:8080", None),
            ("[2001:db8::1]:00443", Some("/signed?encoded=%2F")),
        ] {
            let mut response = WebSocketHandshakeResponse::default();
            response.set_authority(authority).unwrap();
            if let Some(path) = path {
                response.set_path_and_query(path).unwrap();
            }
            assert_eq!(
                response.replace_header("Host", b"other"),
                Err(WebSocketHandshakeFailure::InvalidResponse)
            );
            assert_eq!(
                response.append_header("host", b"other"),
                Err(WebSocketHandshakeFailure::InvalidResponse)
            );
            assert_eq!(
                response.remove_header("HOST"),
                Err(WebSocketHandshakeFailure::InvalidResponse)
            );
            let modifier = prepare_dynamic(
                &[],
                WebSocketHandshakeConfig(Arc::new(AuthorityOverride(response))),
                crate::ProtocolVersion::V4,
                "client".into(),
                Arc::default(),
            )
            .unwrap();
            let mut original = request(Instant::now() + Duration::from_secs(1));
            original
                .headers_mut()
                .append(http::header::HOST, "duplicate.test".parse().unwrap());
            let modified = modifier(original).await.unwrap();
            assert_eq!(modified.uri().scheme_str(), Some("ws"));
            assert_eq!(modified.uri().authority().unwrap().as_str(), authority);
            assert_eq!(modified.headers()[http::header::HOST], authority);
            assert_eq!(
                modified
                    .headers()
                    .get_all(http::header::HOST)
                    .iter()
                    .count(),
                1
            );
            assert_eq!(
                modified.uri().path_and_query().unwrap().as_str(),
                path.unwrap_or("/mqtt")
            );
            let context = modified
                .extensions()
                .get::<crate::backend::WebSocketRequestContext>()
                .unwrap();
            assert_eq!(context.broker_host, "broker.test");
            assert_eq!(context.dial_target, "proxy.test:8080");
        }
    }

    #[tokio::test]
    async fn final_request_budget_counts_authority_in_both_uri_and_host() {
        let mut response = WebSocketHandshakeResponse::default();
        response
            .set_authority(&"a".repeat(MAX_WEBSOCKET_BYTES / 2))
            .unwrap();
        let modifier = prepare_dynamic(
            &[],
            WebSocketHandshakeConfig(Arc::new(AuthorityOverride(response))),
            crate::ProtocolVersion::V5,
            "client".into(),
            Arc::default(),
        )
        .unwrap();
        assert!(matches!(
            modifier(request(Instant::now() + Duration::from_secs(1))).await,
            Err(WebSocketHandshakeFailure::ResourceLimit)
        ));
    }

    struct Decisions(Arc<AtomicUsize>);
    impl WebSocketHandshake for Decisions {
        fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
            assert_eq!(request.dial_target, "proxy.test:8080");
            let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            assert_eq!(request.attempt, call as u64);
            Box::pin(async move {
                match call {
                    1 => Err(WebSocketHandshakeFailure::Rejected),
                    2 => {
                        let mut response = WebSocketHandshakeResponse::default();
                        for _ in 0..MAX_WEBSOCKET_HEADERS {
                            response.append_header("x-many", b"value")?;
                        }
                        Ok(response)
                    }
                    _ => Ok(WebSocketHandshakeResponse::default()),
                }
            })
        }
    }

    #[tokio::test]
    async fn rejection_and_final_header_limits_preserve_typed_attempt_results() {
        let calls = Arc::new(AtomicUsize::new(0));
        let monitor = Arc::new(HandshakeMonitor::default());
        let modifier = prepare_dynamic(
            &[],
            WebSocketHandshakeConfig(Arc::new(Decisions(calls.clone()))),
            crate::ProtocolVersion::V5,
            "client".into(),
            monitor.clone(),
        )
        .unwrap();
        for failure in [
            WebSocketHandshakeFailure::Rejected,
            WebSocketHandshakeFailure::ResourceLimit,
        ] {
            monitor.reset();
            assert!(
                matches!(modifier(request(Instant::now() + Duration::from_secs(1))).await,
                Err(actual) if actual == failure)
            );
            assert_eq!(monitor.failure(), Some(failure));
            assert_eq!(
                failure.retryable(),
                failure == WebSocketHandshakeFailure::Rejected
            );
        }
        monitor.reset();
        assert!(
            modifier(request(Instant::now() + Duration::from_secs(1)))
                .await
                .is_ok()
        );
        assert_eq!(monitor.failure(), None);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn expired_native_deadline_skips_the_authority() {
        let calls = Arc::new(AtomicUsize::new(0));
        let monitor = Arc::new(HandshakeMonitor::default());
        let modifier = prepare_dynamic(
            &[],
            WebSocketHandshakeConfig(Arc::new(Decisions(calls.clone()))),
            crate::ProtocolVersion::V4,
            "client".into(),
            monitor.clone(),
        )
        .unwrap();
        assert!(matches!(
            modifier(request(Instant::now())).await,
            Err(WebSocketHandshakeFailure::Timeout)
        ));
        assert_eq!(monitor.failure(), Some(WebSocketHandshakeFailure::Timeout));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    struct LateReply(bool);
    impl WebSocketHandshake for LateReply {
        fn prepare(&self, request: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
            let reject = self.0;
            Box::pin(async move {
                // A misbehaving host future can return Ready after monopolizing the driver.
                std::thread::sleep(
                    request.deadline.saturating_duration_since(Instant::now())
                        + Duration::from_millis(5),
                );
                if reject {
                    Err(WebSocketHandshakeFailure::Rejected)
                } else {
                    Ok(WebSocketHandshakeResponse::default())
                }
            })
        }
    }

    #[tokio::test]
    async fn ready_results_after_the_native_deadline_are_timeouts() {
        for reject in [false, true] {
            let modifier = prepare_dynamic(
                &[],
                WebSocketHandshakeConfig(Arc::new(LateReply(reject))),
                crate::ProtocolVersion::V5,
                "client".into(),
                Arc::default(),
            )
            .unwrap();
            assert!(matches!(
                modifier(request(Instant::now() + Duration::from_millis(20))).await,
                Err(WebSocketHandshakeFailure::Timeout)
            ));
        }
    }
}
