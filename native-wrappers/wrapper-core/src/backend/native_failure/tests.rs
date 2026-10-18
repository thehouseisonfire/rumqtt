use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;
use crate::{DeliveryStatus, Error, ErrorContext, ErrorKind, NativeIoKind};

fn mapped4(error: rumqttc_v4::ConnectionError) -> Error {
    let mapped = super::super::v4::map_connection_error(&error);
    drop(error);
    mapped
}
fn mapped5(error: rumqttc_v5::ConnectionError) -> Error {
    let mapped = super::super::v5::map_connection_error(&error);
    drop(error);
    mapped
}

#[test]
fn state_failures_preserve_evidence_and_legacy_classification() {
    for (error, code, id) in [
        (
            mapped4(rumqttc_v4::StateError::AwaitPingResp.into()),
            C::PingResponseTimeout,
            None,
        ),
        (
            mapped5(rumqttc_v5::StateError::AwaitPingResp.into()),
            C::PingResponseTimeout,
            None,
        ),
        (
            mapped4(rumqttc_v4::StateError::Unsolicited(42).into()),
            C::UnsolicitedAck,
            Some(42),
        ),
        (
            mapped5(rumqttc_v5::StateError::Unsolicited(43).into()),
            C::UnsolicitedAck,
            Some(43),
        ),
    ] {
        let details = error.native_failure().unwrap();
        assert_eq!(details.code, code);
        assert_eq!(details.packet_id, id);
        assert_eq!(details.packet_kind, None);
        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert_eq!(error.delivery_status(), DeliveryStatus::Ambiguous);
        assert!(!error.retryable()); // Legacy metadata is deliberately unchanged.
    }
}

#[test]
fn count_mismatches_have_expected_counts_not_maxima() {
    for error in [
        mapped4(
            rumqttc_v4::StateError::ProtocolViolation(
                rumqttc_v4::ProtocolViolation::SubAckReturnCodeCountMismatch {
                    pkid: 4,
                    expected: 2,
                    actual: 0,
                },
            )
            .into(),
        ),
        mapped5(
            rumqttc_v5::StateError::ProtocolViolation(
                rumqttc_v5::ProtocolViolation::UnsubAckReasonCodeCountMismatch {
                    pkid: 4,
                    expected: 2,
                    actual: 0,
                },
            )
            .into(),
        ),
    ] {
        let details = error.native_failure().unwrap();
        assert_eq!(details.code, C::AckCountMismatch);
        assert_eq!(details.quantity, Some(Q::Items));
        assert_eq!(details.actual, Some(0));
        assert_eq!(details.expected, Some(2));
        assert_eq!(details.maximum, None);
        assert_eq!(details.packet_id, Some(4));
        assert!(matches!(details.packet_kind, Some(9 | 11)));
    }
}

#[test]
fn packet_limits_preserve_partial_v4_and_full_v5_evidence() {
    let v4 = mapped4(
        rumqttc_v4::StateError::Deserialization(
            rumqttc_v4::mqttbytes::Error::PayloadSizeLimitExceeded(1234),
        )
        .into(),
    );
    let details = v4.native_failure().unwrap();
    assert_eq!(details.quantity, Some(Q::RemainingBytes));
    assert_eq!(details.actual, Some(1234));
    assert_eq!(details.maximum, None);
    let v5 = mapped5(
        rumqttc_v5::StateError::Deserialization(
            rumqttc_v5::mqttbytes::Error::PayloadSizeLimitExceeded {
                pkt_size: 1238,
                max: 1024,
            },
        )
        .into(),
    );
    let details = v5.native_failure().unwrap();
    assert_eq!(details.quantity, Some(Q::PacketBytes));
    assert_eq!(details.actual, Some(1238));
    assert_eq!(details.maximum, Some(1024));
    for error in [
        mapped4(
            rumqttc_v4::StateError::Deserialization(
                rumqttc_v4::mqttbytes::Error::OutgoingPacketTooLarge {
                    pkt_size: 200,
                    max: 100,
                },
            )
            .into(),
        ),
        mapped5(
            rumqttc_v5::StateError::OutgoingPacketTooLarge {
                pkt_size: 200,
                max: 100,
            }
            .into(),
        ),
    ] {
        assert_eq!(error.native_failure().unwrap().code, C::OutgoingTooLarge);
        assert_eq!(error.native_failure().unwrap().maximum, Some(100));
    }
}

#[test]
fn malformed_packets_and_handshake_packet_kinds_are_sanitized() {
    for error in [
        mapped4(
            rumqttc_v4::StateError::Deserialization(rumqttc_v4::mqttbytes::Error::MalformedPacket)
                .into(),
        ),
        mapped5(
            rumqttc_v5::StateError::Deserialization(rumqttc_v5::mqttbytes::Error::MalformedPacket)
                .into(),
        ),
    ] {
        assert_eq!(error.native_failure().unwrap().code, C::MalformedPacket);
    }
    let error = mapped4(rumqttc_v4::ConnectionError::NotConnAck(Box::new(
        rumqttc_v4::mqttbytes::v4::Packet::Publish(rumqttc_v4::mqttbytes::v4::Publish::new(
            "secret/topic",
            rumqttc_v4::QoS::AtMostOnce,
            "secret payload",
        )),
    )));
    assert_eq!(error.native_failure().unwrap().packet_kind, Some(3));
    assert!(!format!("{error:?} {error}").contains("secret"));
}

#[test]
fn io_kinds_and_os_codes_have_independent_presence() {
    for error in [
        mapped4(io::Error::new(io::ErrorKind::ConnectionRefused, "secret DNS name").into()),
        mapped5(io::Error::new(io::ErrorKind::ConnectionRefused, "secret DNS name").into()),
    ] {
        let details = error.native_failure().unwrap();
        assert_eq!(details.io_kind, Some(NativeIoKind::ConnectionRefused));
        assert_eq!(details.os_error, None);
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    let error = mapped5(io::Error::from_raw_os_error(12345).into());
    assert_eq!(
        error.native_failure().unwrap().os_error,
        Some((crate::native_failure::os_domain(), 12345))
    );
    assert_eq!(
        NativeIoKind::from(io::ErrorKind::StorageFull),
        NativeIoKind::Unknown
    );
}

#[derive(Debug)]
struct SecretOwner(Arc<()>);
impl std::fmt::Display for SecretOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = &self.0;
        f.write_str("secret nested source")
    }
}
impl StdError for SecretOwner {}

#[test]
fn sanitized_summaries_retain_scalars_and_release_nested_owners() {
    let owner = Arc::new(());
    let weak = Arc::downgrade(&owner);
    let native = rumqttc_v5::ConnectionError::Io(io::Error::new(
        io::ErrorKind::ConnectionReset,
        SecretOwner(owner),
    ));
    let error = super::super::v5::map_connection_error(&native);
    let summary = error.diagnostic_copy();
    drop(error);
    drop(native);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        summary.native_failure().unwrap().io_kind,
        Some(NativeIoKind::ConnectionReset)
    );
    assert!(summary.source().is_none());
    assert!(!format!("{summary:?} {summary}").contains("secret"));
}

#[test]
fn reconnect_retention_and_exhaustion_strip_opaque_source_owners() {
    for exhaustion in [false, true] {
        let owner = Arc::new(());
        let weak = Arc::downgrade(&owner);
        let error = Error::sourced(
            ErrorKind::Network,
            DeliveryStatus::Ambiguous,
            io::Error::new(io::ErrorKind::ConnectionReset, SecretOwner(owner)),
        )
        .with_native_failure(Some(Details::new(D::Io, C::Io)));
        let retained = if exhaustion {
            Error::reconnect_exhausted(crate::ReconnectExhaustion {
                cycles_started: 1,
                retries_since_reset: 0,
                last_failure: Some(error),
            })
            .reconnect_exhaustion()
            .unwrap()
            .last_failure
            .clone()
            .unwrap()
        } else {
            let controller = crate::reconnect::Controller::new(crate::ReconnectPolicy::Legacy);
            controller.failed(error);
            controller.snapshot().last_failure.unwrap()
        };
        assert!(weak.upgrade().is_none());
        assert!(retained.source().is_none());
        assert_eq!(retained.native_failure().unwrap().code, C::Io);
        assert!(!format!("{retained:?} {retained}").contains("secret"));
    }
}

#[test]
fn source_cycles_stop_at_the_shared_budget() {
    #[derive(Debug)]
    struct Cycle(AtomicUsize);
    impl std::fmt::Display for Cycle {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("must not render")
        }
    }
    impl StdError for Cycle {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Some(self)
        }
    }
    let cycle = Cycle(AtomicUsize::new(0));
    let mut budget = SOURCE_BUDGET;
    assert_eq!(source_details(&cycle, &mut budget), None);
    assert_eq!(cycle.0.load(Ordering::Relaxed), SOURCE_BUDGET);
    assert_eq!(budget, 0);
}

#[test]
fn origin_absence_and_revision_survive_later_context_and_exhaustion() {
    let error = mapped5(io::Error::from(io::ErrorKind::ConnectionReset).into())
        .with_context(ErrorContext {
            protocol: Some(crate::ProtocolVersion::V5),
            phase: Some(crate::ConnectionPhase::Attempt),
            generation: None,
            operation_id: None,
        })
        .with_configuration_revision(Some(3))
        .capture_origin();
    let later = error
        .clone()
        .with_configuration_revision(Some(9))
        .with_context(ErrorContext {
            generation: Some(7),
            phase: Some(crate::ConnectionPhase::Established),
            ..Default::default()
        });
    assert_eq!(later.context(), error.context());
    assert_eq!(later.configuration_revision(), Some(3));
    let exhausted = Error::reconnect_exhausted(crate::ReconnectExhaustion {
        cycles_started: 2,
        retries_since_reset: 1,
        last_failure: Some(error.diagnostic_copy()),
    })
    .with_context(ErrorContext {
        generation: Some(99),
        ..Default::default()
    });
    assert_eq!(exhausted.context().generation, None);
    assert_eq!(exhausted.configuration_revision(), Some(3));
    assert_eq!(exhausted.native_failure(), None);
    assert_eq!(
        exhausted
            .reconnect_exhaustion()
            .unwrap()
            .last_failure
            .as_ref()
            .unwrap()
            .native_failure(),
        error.native_failure()
    );
}

#[test]
fn dedicated_failures_are_preserved_without_chasing_store_causes() {
    let error = mapped5(rumqttc_v5::ConnectionError::SessionStore(Box::new(
        io::Error::new(io::ErrorKind::ConnectionReset, "secret store path"),
    )));
    assert!(error.store_failure().is_some());
    assert_eq!(error.native_failure(), None);
    assert_eq!(error.kind(), ErrorKind::Persistence);
    assert!(!error.retryable());
}

#[test]
fn error_layout_stays_within_existing_large_error_limit() {
    assert!(size_of::<Error>() <= 128);
    // Completion dominates this result; details must remain behind the error pointer.
    assert!(size_of::<crate::TerminalOutcome>() <= 256);
    let error = Error::new(ErrorKind::Network, "plain");
    assert!(error.native_failure().is_none());
}

fn redirect(failure: rumqttc_v5::RedirectFailure) -> rumqttc_v5::ConnectionError {
    rumqttc_v5::RedirectError {
        outcome: rumqttc_v5::RedirectOutcome {
            reason: rumqttc_v5::RedirectReason::ServerMoved,
            source: rumqttc_v5::RedirectSource::ConnAck,
            server_reference: Some("secret target".into()),
        },
        failure,
    }
    .into()
}

#[test]
fn redirect_target_causes_supplement_dedicated_terminal_failures() {
    let error = mapped5(redirect(rumqttc_v5::RedirectFailure::FollowFailed(
        Box::new(io::Error::from(io::ErrorKind::ConnectionRefused).into()),
    )));
    assert!(error.redirect_failure().is_some());
    assert!(!error.retryable());
    assert_eq!(
        error.native_failure().unwrap().io_kind,
        Some(NativeIoKind::ConnectionRefused)
    );
    assert!(!format!("{error:?} {error}").contains("secret"));
    let error = mapped5(redirect(rumqttc_v5::RedirectFailure::FollowFailed(
        Box::new(rumqttc_v5::ConnectionError::SessionStore(Box::new(
            crate::StoreFailure::Load,
        ))),
    )));
    assert!(error.redirect_failure().is_some());
    assert_eq!(error.store_failure(), Some(crate::StoreFailure::Load));
    assert_eq!(error.native_failure(), None);
}

#[test]
fn redirects_and_nested_sources_share_one_extraction_budget() {
    let mut error = io::Error::from(io::ErrorKind::ConnectionRefused).into();
    for _ in 0..SOURCE_BUDGET {
        error = redirect(rumqttc_v5::RedirectFailure::FollowFailed(Box::new(error)));
    }
    let details = super::v5(&error).unwrap();
    assert_eq!((details.domain, details.code), (D::Connection, C::Unknown));
    assert_eq!(details.io_kind, None);
}

#[test]
fn reconnect_snapshot_equality_includes_native_evidence() {
    let controller = crate::reconnect::Controller::new(crate::ReconnectPolicy::Legacy);
    controller.failed(mapped4(
        io::Error::from(io::ErrorKind::ConnectionRefused).into(),
    ));
    let snapshot = controller.snapshot();
    let mut changed = snapshot.clone();
    changed.last_failure =
        Some(mapped4(io::Error::from(io::ErrorKind::ConnectionReset).into()).diagnostic_copy());
    assert_ne!(snapshot, changed);
    assert_eq!(snapshot, snapshot.clone());
}

#[cfg(feature = "http-proxy")]
#[test]
fn http_proxy_status_and_nested_io_have_safe_typed_mappings() {
    for (status, code) in [
        (407, C::ProxyAuthentication),
        (403, C::ProxyPolicy),
        (503, C::ProxyUnavailable),
    ] {
        let error = mapped5(rumqttc_v5::ConnectionError::Proxy(
            rumqttc_core::ProxyError::Http(async_http_proxy::HttpError::HttpCode200(status)),
        ));
        let details = error.native_failure().unwrap();
        assert_eq!(details.http_status, Some(status));
        assert_eq!(details.code, code);
        assert_eq!(details.os_error, None);
    }
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn opaque_tls_causes_have_explicit_unknown_classification_without_text() {
    let error = mapped5(rumqttc_v5::ConnectionError::Tls(
        rustls::Error::General("secret backend text".into()).into(),
    ));
    assert_eq!(
        error.native_failure().unwrap().cause,
        Some((D::Tls, C::Unknown))
    );
    assert!(!format!("{error:?} {error}").contains("secret"));
}

#[cfg(feature = "use-rustls-no-provider")]
#[test]
fn tls_inside_io_retains_outer_domain_and_specific_safe_cause() {
    let error = mapped5(
        io::Error::other(rustls::Error::InvalidCertificate(
            rustls::CertificateError::Expired,
        ))
        .into(),
    );
    let details = error.native_failure().unwrap();
    assert_eq!((details.domain, details.code), (D::Io, C::Io));
    assert_eq!(details.cause, Some((D::Tls, C::Certificate)));
    assert_eq!(details.io_kind, Some(NativeIoKind::Other));
    assert_eq!(details.os_error, None);
}

#[cfg(feature = "websocket")]
#[test]
fn websocket_http_and_established_stream_errors_keep_safe_evidence() {
    use async_tungstenite::tungstenite::Error as E;
    let response = http::Response::builder()
        .status(503)
        .header("secret-header", "secret value")
        .body(Some(b"secret body".to_vec()))
        .unwrap();
    let error = mapped5(rumqttc_v5::ConnectionError::Websocket(E::Http(Box::new(
        response,
    ))));
    assert_eq!(error.native_failure().unwrap().http_status, Some(503));
    assert!(!format!("{error:?} {error}").contains("secret"));
    let error = mapped4(
        rumqttc_v4::StateError::Io(io::Error::other(E::Protocol(
            async_tungstenite::tungstenite::error::ProtocolError::InvalidOpcode(3),
        )))
        .into(),
    );
    assert_eq!(
        error.native_failure().unwrap().cause,
        Some((D::WebSocket, C::WebSocketProtocol))
    );
}

#[cfg(feature = "socks-proxy")]
#[test]
fn socks_policy_and_availability_remain_distinct() {
    for (native, code) in [
        (tokio_socks::Error::ConnectionRefused, C::ProxyUnavailable),
        (
            tokio_socks::Error::PasswordAuthFailure(1),
            C::ProxyAuthentication,
        ),
        (
            tokio_socks::Error::ConnectionNotAllowedByRuleset,
            C::ProxyPolicy,
        ),
    ] {
        let error = mapped5(rumqttc_v5::ConnectionError::Proxy(
            rumqttc_core::ProxyError::Socks5(native),
        ));
        assert_eq!(error.native_failure().unwrap().code, code);
        assert_eq!(error.native_failure().unwrap().os_error, None);
    }
}

#[cfg(feature = "use-native-tls")]
#[test]
fn native_tls_opaque_backend_errors_remain_unknown_without_backend_text() {
    let Err(source) = native_tls::Certificate::from_pem(b"secret invalid certificate") else {
        panic!("invalid fixture certificate was accepted");
    };
    let error = mapped5(rumqttc_v5::ConnectionError::Tls(
        rumqttc_v5::TlsError::NativeTls(source),
    ));
    let details = error.native_failure().unwrap();
    assert_eq!((details.domain, details.code), (D::Tls, C::Unknown));
    assert_eq!(details.os_error, None);
    assert!(!format!("{error:?} {error}").contains("secret"));
}
