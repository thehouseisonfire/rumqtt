//! Typed classification; formatted peer/error text never participates in policy.
#[cfg(any(
    feature = "use-rustls-no-provider",
    feature = "use-native-tls",
    feature = "http-proxy",
    feature = "socks-proxy"
))]
use std::error::Error as StdError;
use std::io;

pub(super) const fn mapped(error: &crate::Error, native: bool) -> bool {
    if let Some(failure) = error.transport_failure() {
        return failure.retryable();
    }
    if let Some(failure) = error.tls_callback_failure() {
        return failure.retryable();
    }
    if let Some(failure) = error.websocket_failure() {
        return failure.retryable();
    }
    native
}

fn io_retry(error: &io::Error) -> bool {
    // ByteReader/ByteWriter preserve non-I/O tungstenite errors as the inner
    // cause of io::ErrorKind::Other. Classify that cause before generic I/O.
    #[cfg(feature = "websocket")]
    if let Some(source) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<async_tungstenite::tungstenite::Error>())
    {
        return websocket(source);
    }
    // Rustls puts certificate/protocol failures inside io::Error. Inspect them
    // before allowing a generic transport retry.
    #[cfg(feature = "use-rustls-no-provider")]
    if error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<rustls::Error>().is_some())
    {
        return false;
    }
    !matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::Unsupported
            | io::ErrorKind::PermissionDenied
    )
}

#[cfg(any(
    feature = "use-rustls-no-provider",
    feature = "use-native-tls",
    feature = "http-proxy",
    feature = "socks-proxy"
))]
fn nested_io(error: &(dyn StdError + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(error) = source {
        if let Some(error) = error.downcast_ref::<io::Error>() {
            return io_retry(error);
        }
        source = error.source();
    }
    false
}

#[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
fn proxy(error: &rumqttc_core::ProxyError) -> bool {
    #[cfg(feature = "socks-proxy")]
    if let rumqttc_core::ProxyError::Socks5(error) = error {
        use tokio_socks::Error;
        return match error {
            Error::Io(error) => io_retry(error),
            Error::ProxyServerUnreachable
            | Error::GeneralSocksServerFailure
            | Error::NetworkUnreachable
            | Error::HostUnreachable
            | Error::ConnectionRefused
            | Error::TtlExpired => true,
            // Authentication, policy, address and protocol failures require a
            // configuration or peer change rather than another connection.
            _ => false,
        };
    }
    nested_io(error)
}

#[cfg(feature = "websocket")]
fn websocket(error: &async_tungstenite::tungstenite::Error) -> bool {
    use async_tungstenite::tungstenite::{Error, error::ProtocolError};
    match error {
        Error::Io(error) => io_retry(error),
        // EOF without a closing handshake is a transport loss, not a malformed
        // frame. Other WebSocket protocol violations remain terminal.
        Error::ConnectionClosed
        | Error::AlreadyClosed
        | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => true,
        Error::Http(response) => {
            let code = response.status().as_u16();
            matches!(code, 408 | 429 | 500..=599)
        }
        _ => false,
    }
}

pub(super) fn v4(error: &rumqttc_v4::ConnectionError) -> bool {
    use rumqttc_v4::{ConnectionError as E, StateError as S};
    match error {
        E::Io(error) | E::MqttState(S::Io(error)) => io_retry(error),
        E::MqttState(S::Deserialization(rumqttc_v4::mqttbytes::Error::Io(error))) => {
            io_retry(error)
        }
        E::NetworkTimeout
        | E::FlushTimeout
        | E::MqttState(S::ConnectionAborted | S::AwaitPingResp) => true,
        E::ConnectionRefused(rumqttc_v4::ConnectReturnCode::ServiceUnavailable) => true,
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        E::Tls(error) => nested_io(error),
        #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
        E::Proxy(error) => proxy(error),
        #[cfg(feature = "websocket")]
        E::Websocket(error) => websocket(error),
        _ => false,
    }
}

pub(super) fn v5(error: &rumqttc_v5::ConnectionError) -> bool {
    use rumqttc_v5::{ConnectionError as E, StateError as S};
    match error {
        E::Io(error) | E::MqttState(S::Io(error)) => io_retry(error),
        E::MqttState(S::Deserialization(rumqttc_v5::mqttbytes::Error::Io(error))) => {
            io_retry(error)
        }
        E::Timeout(_) | E::MqttState(S::ConnectionAborted | S::AwaitPingResp) => true,
        E::ConnectionRefused(reason) | E::MqttState(S::ConnFail { reason }) => {
            matches!(super::v5::connack_reason(*reason), 3 | 0x88 | 0x89 | 0x9f)
        }
        E::MqttState(S::ServerDisconnect { reason_code, .. }) => {
            matches!(*reason_code as u8, 0 | 0x89 | 0x8b | 0x8d | 0x9f | 0xa0)
        }
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        E::Tls(error) => nested_io(error),
        #[cfg(any(feature = "http-proxy", feature = "socks-proxy"))]
        E::Proxy(error) => proxy(error),
        #[cfg(feature = "websocket")]
        E::Websocket(error) => websocket(error),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "socks-proxy")]
    #[test]
    fn socks_failures_distinguish_availability_from_configuration_and_protocol_errors() {
        use tokio_socks::Error as E;
        for (error, expected) in [
            (E::ProxyServerUnreachable, true),
            (E::GeneralSocksServerFailure, true),
            (E::NetworkUnreachable, true),
            (E::HostUnreachable, true),
            (E::ConnectionRefused, true),
            (E::TtlExpired, true),
            (E::InvalidTargetAddress("invalid"), false),
            (E::InvalidResponseVersion, false),
            (E::NoAcceptableAuthMethods, false),
            (E::UnknownAuthMethod, false),
            (E::ConnectionNotAllowedByRuleset, false),
            (E::CommandNotSupported, false),
            (E::AddressTypeNotSupported, false),
            (E::UnknownError, false),
            (E::InvalidReservedByte, false),
            (E::UnknownAddressType, false),
            (E::InvalidAuthValues("invalid"), false),
            (E::PasswordAuthFailure(1), false),
            (E::AuthorizationRequired, false),
            (E::IdentdAuthFailure, false),
            (E::InvalidUserIdAuthFailure, false),
            (E::Io(io::ErrorKind::ConnectionReset.into()), true),
            (E::Io(io::ErrorKind::InvalidData.into()), false),
        ] {
            let error = rumqttc_v4::ConnectionError::Proxy(rumqttc_core::ProxyError::Socks5(error));
            assert_eq!(v4(&error), expected, "{error:?}");
            let rumqttc_v4::ConnectionError::Proxy(error) = error else {
                unreachable!()
            };
            let error = rumqttc_v5::ConnectionError::Proxy(error);
            assert_eq!(v5(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn nested_transport_and_ping_failures_are_not_protocol_violations() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::TimedOut,
        ] {
            assert!(v4(&rumqttc_v4::ConnectionError::MqttState(
                rumqttc_v4::StateError::Io(io::Error::from(kind))
            )));
            assert!(v5(&rumqttc_v5::ConnectionError::MqttState(
                rumqttc_v5::StateError::Io(io::Error::from(kind))
            )));
        }
        assert!(v4(&rumqttc_v4::ConnectionError::MqttState(
            rumqttc_v4::StateError::AwaitPingResp
        )));
        assert!(v5(&rumqttc_v5::ConnectionError::MqttState(
            rumqttc_v5::StateError::AwaitPingResp
        )));
        assert!(!v4(&rumqttc_v4::ConnectionError::MqttState(
            rumqttc_v4::StateError::Unsolicited(1)
        )));
        assert!(!v5(&rumqttc_v5::ConnectionError::MqttState(
            rumqttc_v5::StateError::Unsolicited(1)
        )));
    }

    #[test]
    fn broker_refusals_distinguish_unavailability_from_authentication() {
        assert!(v4(&rumqttc_v4::ConnectionError::ConnectionRefused(
            rumqttc_v4::ConnectReturnCode::ServiceUnavailable
        )));
        assert!(!v4(&rumqttc_v4::ConnectionError::ConnectionRefused(
            rumqttc_v4::ConnectReturnCode::NotAuthorized
        )));
        for reason in [
            rumqttc_v5::ConnectReturnCode::ServerUnavailable,
            rumqttc_v5::ConnectReturnCode::ServerBusy,
            rumqttc_v5::ConnectReturnCode::ConnectionRateExceeded,
        ] {
            assert!(v5(&rumqttc_v5::ConnectionError::ConnectionRefused(reason)));
        }
        for reason in [
            rumqttc_v5::ConnectReturnCode::BadUserNamePassword,
            rumqttc_v5::ConnectReturnCode::NotAuthorized,
            rumqttc_v5::ConnectReturnCode::BadAuthenticationMethod,
            rumqttc_v5::ConnectReturnCode::QuotaExceeded,
        ] {
            assert!(!v5(&rumqttc_v5::ConnectionError::ConnectionRefused(reason)));
        }
    }

    #[test]
    fn broker_disconnect_policy_does_not_create_takeover_retry_loops() {
        use rumqttc_v5::mqttbytes::v5::DisconnectReasonCode as D;
        for (reason, expected) in [
            (D::ServerBusy, true),
            (D::ServerShuttingDown, true),
            (D::KeepAliveTimeout, true),
            (D::SessionTakenOver, false),
            (D::NotAuthorized, false),
            (D::MalformedPacket, false),
        ] {
            assert_eq!(
                v5(&rumqttc_v5::ConnectionError::MqttState(
                    rumqttc_v5::StateError::ServerDisconnect {
                        reason_code: reason,
                        reason_string: Some("peer secret".into())
                    }
                )),
                expected
            );
        }
    }

    #[test]
    fn a_store_error_with_a_transport_cause_remains_terminal_persistence() {
        let error =
            rumqttc_v4::ConnectionError::SessionStore(Box::new(crate::TransportFailure::Connect));
        let mapped_error = super::super::v4::map_connection_error(&error);
        assert_eq!(mapped_error.kind(), crate::ErrorKind::Persistence);
        assert!(!mapped(&mapped_error, v4(&error)));
        let error =
            rumqttc_v5::ConnectionError::SessionStore(Box::new(crate::TransportFailure::Connect));
        let mapped_error = super::super::v5::map_connection_error(&error);
        assert_eq!(mapped_error.kind(), crate::ErrorKind::Persistence);
        assert!(!mapped(&mapped_error, v5(&error)));
    }

    #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
    #[test]
    fn tls_transport_failures_retry_but_configuration_failures_stop() {
        assert!(v4(&rumqttc_v4::ConnectionError::Tls(
            io::Error::from(io::ErrorKind::ConnectionReset).into()
        )));
        assert!(v5(&rumqttc_v5::ConnectionError::Tls(
            io::Error::from(io::ErrorKind::TimedOut).into()
        )));
        assert!(!v4(&rumqttc_v4::ConnectionError::Tls(
            rumqttc_v4::TlsError::UnsupportedBackendConfiguration
        )));
        assert!(!v5(&rumqttc_v5::ConnectionError::Tls(
            rumqttc_v5::TlsError::UnsupportedBackendConfiguration
        )));
    }

    #[cfg(feature = "use-rustls-no-provider")]
    #[test]
    fn certificate_failures_inside_io_remain_terminal() {
        let certificate = || rustls::Error::InvalidCertificate(rustls::CertificateError::Expired);
        assert!(!v4(&rumqttc_v4::ConnectionError::Io(io::Error::other(
            certificate()
        ))));
        assert!(!v5(&rumqttc_v5::ConnectionError::Tls(
            io::Error::other(certificate()).into()
        )));
        assert!(!v5(&rumqttc_v5::ConnectionError::Tls(certificate().into())));
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_http_refusals_distinguish_transient_and_permanent_failures() {
        use async_tungstenite::tungstenite::{Error, http::Response};
        for (status, eligible) in [
            (401, false),
            (403, false),
            (408, true),
            (429, true),
            (503, true),
        ] {
            assert_eq!(
                websocket(&Error::Http(Box::new(
                    Response::builder().status(status).body(None).unwrap()
                ))),
                eligible
            );
        }
        assert!(websocket(&Error::ConnectionClosed));
        assert!(websocket(&Error::Protocol(
            async_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake
        )));
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn websocket_errors_wrapped_as_io_retain_their_typed_classification() {
        use async_tungstenite::tungstenite::{Error, error::ProtocolError};
        for mqtt5 in [false, true] {
            for (error, eligible) in [
                (Error::Protocol(ProtocolError::InvalidOpcode(3)), false),
                (Error::Protocol(ProtocolError::NonZeroReservedBits), false),
                (Error::ConnectionClosed, true),
                (Error::AlreadyClosed, true),
                (
                    Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                    true,
                ),
                (Error::Io(io::ErrorKind::ConnectionReset.into()), true),
                (Error::Io(io::ErrorKind::InvalidData.into()), false),
            ] {
                let wrapped = io::Error::other(error);
                assert_eq!(io_retry(&wrapped), eligible);
                // Established streams surface through native state/codec I/O
                // errors, rather than Websocket variants used during the upgrade.
                let native = if mqtt5 {
                    v5(&rumqttc_v5::ConnectionError::MqttState(
                        rumqttc_v5::StateError::Deserialization(rumqttc_v5::mqttbytes::Error::Io(
                            wrapped,
                        )),
                    ))
                } else {
                    v4(&rumqttc_v4::ConnectionError::MqttState(
                        rumqttc_v4::StateError::Io(wrapped),
                    ))
                };
                assert_eq!(native, eligible);
            }
        }
    }
}
