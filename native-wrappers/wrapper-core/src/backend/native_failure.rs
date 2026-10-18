//! Typed extraction only. Neither source text nor source owners cross this boundary.
use std::error::Error as StdError;
use std::io;

use crate::{
    NativeFailureCode as C, NativeFailureDetails as Details, NativeFailureDomain as D,
    NativeFailureQuantity as Q,
};

mod v4;
mod v5;
pub(super) use v4::connection as v4;
pub(super) use v5::connection as v5;

/// One budget covers typed wrappers, arbitrary sources and redirect recursion.
const SOURCE_BUDGET: usize = 16;

const fn enter(budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

fn merge(mut outer: Details, inner: Option<Details>) -> Details {
    if let Some(inner) = inner {
        outer.cause = inner.cause.or(Some((inner.domain, inner.code)));
        if inner.io_kind.is_some() {
            outer.io_kind = inner.io_kind;
            outer.os_error = inner.os_error;
        }
        outer.http_status = inner.http_status.or(outer.http_status);
        if inner.quantity.is_some() {
            outer.quantity = inner.quantity;
            outer.actual = inner.actual;
            outer.maximum = inner.maximum;
            outer.expected = inner.expected;
        }
    }
    outer
}

fn io(error: &io::Error, budget: &mut usize) -> Details {
    let mut details = Details::new(D::Io, C::Io);
    details.io_kind = Some(error.kind().into());
    details.os_error = error
        .raw_os_error()
        .map(|code| (crate::native_failure::os_domain(), code));
    merge(
        details,
        error
            .get_ref()
            .and_then(|source| source_details(source, budget)),
    )
}

fn source_details(mut error: &(dyn StdError + 'static), budget: &mut usize) -> Option<Details> {
    while enter(budget) {
        if let Some(error) = error.downcast_ref::<io::Error>() {
            return Some(io(error, budget));
        }
        #[cfg(feature = "websocket")]
        if let Some(error) = error.downcast_ref::<async_tungstenite::tungstenite::Error>() {
            return Some(websocket(error, budget));
        }
        #[cfg(feature = "use-rustls-no-provider")]
        if let Some(error) = error.downcast_ref::<rustls::Error>() {
            return Some(rustls(error));
        }
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        if let Some(error) = error.downcast_ref::<rumqttc_core::TlsError>() {
            return Some(tls(error, budget));
        }
        #[cfg(feature = "use-native-tls")]
        if let Some(error) = error.downcast_ref::<native_tls::Error>() {
            return Some(merge(
                Details::new(D::Tls, C::Unknown),
                error
                    .source()
                    .and_then(|source| source_details(source, budget)),
            ));
        }
        error = error.source()?;
    }
    None
}

#[cfg(feature = "use-rustls-no-provider")]
const fn rustls(error: &rustls::Error) -> Details {
    use rustls::Error as E;
    let code = match error {
        E::InvalidCertificate(_) | E::NoCertificatesPresented | E::InvalidCertRevocationList(_) => {
            C::Certificate
        }
        E::BadMaxFragmentSize | E::InconsistentKeys(_) | E::UnsupportedNameType => C::Configuration,
        E::InappropriateMessage { .. }
        | E::InappropriateHandshakeMessage { .. }
        | E::InvalidMessage(_)
        | E::PeerIncompatible(_)
        | E::PeerMisbehaved(_)
        | E::AlertReceived(_)
        | E::DecryptError
        | E::EncryptError
        | E::HandshakeNotComplete
        | E::PeerSentOversizedRecord
        | E::NoApplicationProtocol => C::TlsProtocol,
        _ => C::Unknown,
    };
    Details::new(D::Tls, code)
}

#[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
fn tls(error: &rumqttc_core::TlsError, budget: &mut usize) -> Details {
    use rumqttc_core::TlsError as E;
    match error {
        E::Io(error) => merge(Details::new(D::Tls, C::Io), source_details(error, budget)),
        #[cfg(feature = "use-rustls-no-provider")]
        E::TLS(error) => merge(
            Details::new(D::Tls, C::TlsProtocol),
            source_details(error, budget),
        ),
        #[cfg(feature = "use-rustls-no-provider")]
        E::WebPki(_) => Details::new(D::Tls, C::Certificate),
        #[cfg(feature = "use-rustls-no-provider")]
        E::DNSName(_)
        | E::NativeCerts(_)
        | E::CryptoProviderUnavailable
        | E::Pem(_)
        | E::NoValidCertInChain
        | E::NoValidClientCertInChain
        | E::NoValidKeyInChain => Details::new(D::Tls, C::Configuration),
        E::Addr(_) | E::UnsupportedBackendConfiguration => Details::new(D::Tls, C::Configuration),
        #[cfg(feature = "use-native-tls")]
        E::NativeTls(error) => merge(
            Details::new(D::Tls, C::Unknown),
            source_details(error, budget),
        ),
    }
}

#[cfg(feature = "websocket")]
fn websocket(error: &async_tungstenite::tungstenite::Error, budget: &mut usize) -> Details {
    use async_tungstenite::tungstenite::{Error as E, error::ProtocolError};
    match error {
        E::Io(error) => merge(
            Details::new(D::WebSocket, C::Io),
            source_details(error, budget),
        ),
        E::ConnectionClosed
        | E::AlreadyClosed
        | E::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
            Details::new(D::WebSocket, C::PeerClosed)
        }
        E::Http(response) => {
            let mut details = Details::new(D::WebSocket, C::WebSocketHttp);
            details.http_status = Some(response.status().as_u16());
            details
        }
        E::Protocol(_) | E::Utf8(_) | E::AttackAttempt => {
            Details::new(D::WebSocket, C::WebSocketProtocol)
        }
        E::HttpFormat(_) | E::Url(_) => Details::new(D::WebSocket, C::Configuration),
        // Capacity/write-buffer variants can own messages. Do not inspect them.
        _ => Details::new(D::WebSocket, C::Unknown),
    }
}

#[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
fn proxy(error: &rumqttc_core::ProxyError, budget: &mut usize) -> Details {
    use rumqttc_core::ProxyError as E;
    match error {
        E::Io(error) => merge(Details::new(D::Proxy, C::Io), source_details(error, budget)),
        #[cfg(all(
            feature = "http-proxy",
            any(feature = "use-rustls-no-provider", feature = "use-native-tls")
        ))]
        E::Tls(error) => merge(
            Details::new(D::Proxy, C::TlsProtocol),
            source_details(error, budget),
        ),
        #[cfg(feature = "http-proxy")]
        E::Http(error) => {
            use async_http_proxy::HttpError as H;
            #[allow(unreachable_patterns)] // Retain UNKNOWN for future dependency variants.
            match error {
                H::IoError(error) => {
                    merge(Details::new(D::Proxy, C::Io), source_details(error, budget))
                }
                H::HttpCode200(status) => {
                    let mut details = Details::new(
                        D::Proxy,
                        match status {
                            401 | 407 => C::ProxyAuthentication,
                            403 => C::ProxyPolicy,
                            _ => C::ProxyUnavailable,
                        },
                    );
                    details.http_status = Some(*status);
                    details
                }
                H::EndOfFile => Details::new(D::Proxy, C::PeerClosed),
                H::HttpParseError(_)
                | H::MaximumResponseHeaderLengthExceeded(_)
                | H::NoHttpCode
                | H::NoHttpReason
                | H::HttpReasonConnectionEstablished(_) => {
                    Details::new(D::Proxy, C::MalformedPacket)
                }
                _ => Details::new(D::Proxy, C::Unknown),
            }
        }
        #[cfg(feature = "socks-proxy")]
        E::Socks5(error) => {
            use tokio_socks::Error as S;
            match error {
                S::Io(error) => merge(Details::new(D::Proxy, C::Io), source_details(error, budget)),
                S::NoAcceptableAuthMethods
                | S::UnknownAuthMethod
                | S::InvalidAuthValues(_)
                | S::PasswordAuthFailure(_)
                | S::AuthorizationRequired
                | S::IdentdAuthFailure
                | S::InvalidUserIdAuthFailure => Details::new(D::Proxy, C::ProxyAuthentication),
                S::ConnectionNotAllowedByRuleset => Details::new(D::Proxy, C::ProxyPolicy),
                S::ProxyServerUnreachable
                | S::GeneralSocksServerFailure
                | S::NetworkUnreachable
                | S::HostUnreachable
                | S::ConnectionRefused
                | S::TtlExpired => Details::new(D::Proxy, C::ProxyUnavailable),
                S::InvalidTargetAddress(_)
                | S::CommandNotSupported
                | S::AddressTypeNotSupported => Details::new(D::Proxy, C::Configuration),
                S::InvalidResponseVersion | S::InvalidReservedByte | S::UnknownAddressType => {
                    Details::new(D::Proxy, C::MalformedPacket)
                }
                _ => Details::new(D::Proxy, C::Unknown),
            }
        }
        _ => Details::new(D::Proxy, C::Unknown),
    }
}

#[cfg(test)]
mod tests;
