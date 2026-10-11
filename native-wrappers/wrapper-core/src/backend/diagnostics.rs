//! Protocol-specific diagnostic mapping at completed native capture boundaries.

use std::time::Instant;

#[cfg(feature = "ordered-shutdown")]
use crate::OrderedShutdownDiagnostics;
use crate::{
    BatchingDiagnostics, ConnAckDiagnostic, ConnAckSessionDiagnostics, NativeDiagnosticsSnapshot,
    OutboundDiagnostics, QueueDiagnostics, RedirectDiagnostics, RedirectReason, SessionDiagnostics,
};

macro_rules! copy_fields {
    ($ty:ident, $value:expr, $($field:ident),+ $(,)?) => {
        $ty { $($field: $value.$field),+ }
    };
}

macro_rules! native_capture {
    ($value:ident, $captured_at:ident, $connack:expr, $broker_only:expr, $redirect:expr, $version:ident) => {
        NativeDiagnosticsSnapshot {
            generation: 0,
            captured_at: $captured_at,
            connected: $value.connected,
            disconnecting: $value.disconnecting,
            disconnect_complete: $value.disconnect_complete,
            queues: copy_fields!(
                QueueDiagnostics,
                $value.queues,
                pending_replay_len,
                queued_len,
                pending_len,
                requests_rx_len,
                control_requests_rx_len,
                immediate_disconnect_rx_len
            ),
            outbound: copy_fields!(
                OutboundDiagnostics,
                $value.outbound,
                inflight,
                max_inflight,
                publish_window_full,
                packet_identifiers_in_use,
                collision,
                collision_notice,
                pending_subscribe,
                pending_unsubscribe,
                outgoing_publish,
                outgoing_publish_notices,
                outgoing_pubrel,
                outgoing_pubrel_replay,
                outgoing_pubrel_notices,
                incoming_puback,
                incoming_pub,
                incoming_pubrec,
                outbound_drained
            ),
            session: SessionDiagnostics {
                connack: $connack,
                store_configured: $value.session.session_store_configured,
                store_loaded: $value.session.session_store_loaded,
                store_clear_pending: $value.session.session_store_clear_pending,
                identity_matches: $value.session.local_session_state_matches_client_id,
                broker_only_session_resume: $broker_only,
            },
            batching: copy_fields!(
                BatchingDiagnostics,
                $value.config,
                configured_read_batch_size,
                effective_read_batch_size,
                max_request_batch
            ),
            redirect: $redirect,
            #[cfg(not(feature = "ordered-shutdown"))]
            ordered: None,
            #[cfg(feature = "ordered-shutdown")]
            ordered: Some(Box::new(OrderedShutdownDiagnostics {
                phase: match $value.shutdown_phase {
                    $version::ShutdownPhase::Open => crate::OrderedShutdownPhase::Open,
                    $version::ShutdownPhase::AdmittedDrain => {
                        crate::OrderedShutdownPhase::AdmittedDrain
                    }
                    $version::ShutdownPhase::Approaching => {
                        crate::OrderedShutdownPhase::Approaching
                    }
                    $version::ShutdownPhase::Draining => crate::OrderedShutdownPhase::Draining,
                    $version::ShutdownPhase::Flushing => crate::OrderedShutdownPhase::Flushing,
                    $version::ShutdownPhase::Completed => crate::OrderedShutdownPhase::Completed,
                    $version::ShutdownPhase::TimedOut => crate::OrderedShutdownPhase::TimedOut,
                    $version::ShutdownPhase::Failed => crate::OrderedShutdownPhase::Failed,
                },
                fence_sequence: $value.disconnect_fence_sequence,
                remaining_at_capture: $value
                    .disconnect_deadline
                    .map(|deadline| deadline.saturating_duration_since($captured_at)),
                local_queued_publishes: $value.ordered_local_queued_publishes,
                captured_at: $captured_at,
            })),
        }
    };
}

impl NativeDiagnosticsSnapshot {
    pub(crate) fn v4(value: &rumqttc_v4::EventLoopDiagnostics) -> Self {
        let captured_at = Instant::now();
        let connack = value
            .session
            .connack
            .map(|session| ConnAckSessionDiagnostics {
                raw_session_present: session.raw_session_present,
                session_resumed: session.session_resumed,
                diagnostic: session.diagnostic.and_then(|diagnostic| match diagnostic {
                    rumqttc_v4::ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean => {
                        Some(ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean)
                    }
                    _ => None,
                }),
            });
        native_capture!(value, captured_at, connack, None, None, rumqttc_v4)
    }

    pub(crate) fn v5(value: rumqttc_v5::EventLoopDiagnostics) -> Self {
        let captured_at = Instant::now();
        let connack = value
            .session
            .connack
            .map(|session| ConnAckSessionDiagnostics {
                raw_session_present: session.raw_session_present,
                session_resumed: session.session_resumed,
                diagnostic: session.diagnostic.and_then(|diagnostic| match diagnostic {
                    rumqttc_v5::ConnAckDiagnostic::BrokerOnlySessionResume => {
                        Some(ConnAckDiagnostic::BrokerOnlySessionResume)
                    }
                    _ => None,
                }),
            });
        let redirect = value.redirect;
        let redirect = RedirectDiagnostics {
            selected_reference: redirect.selected_reference,
            policy_configured: redirect.policy_configured,
            attempts: redirect.attempts,
            attempt_limit: redirect.attempt_limit,
            visited_endpoints: redirect.visited_endpoints,
            active: redirect.active,
            target_established: redirect.target_established,
            reason: redirect.reason.map(|reason| match reason {
                rumqttc_v5::RedirectReason::UseAnotherServer => RedirectReason::UseAnotherServer,
                rumqttc_v5::RedirectReason::ServerMoved => RedirectReason::ServerMoved,
            }),
            srv_owner: redirect.srv_owner,
            srv_candidate_index: redirect.srv_candidate_index,
            srv_candidate_count: redirect.srv_candidate_count,
            srv_current_target: redirect.srv_current_target,
        };
        native_capture!(
            value,
            captured_at,
            connack,
            Some(value.session.broker_only_session_resume),
            Some(redirect),
            rumqttc_v5
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! map_for_test {
        (v4, $value:ident) => {
            NativeDiagnosticsSnapshot::v4(&$value)
        };
        (v5, $value:ident) => {
            NativeDiagnosticsSnapshot::v5($value)
        };
    }

    macro_rules! assert_mapping {
        ($value:ident, $map:ident, $broker:expr) => {{
            $value.connected = true;
            $value.disconnecting = true;
            $value.disconnect_complete = true;
            $value.queues.pending_replay_len = 7;
            $value.queues.queued_len = 11;
            $value.queues.pending_len = 18;
            $value.queues.requests_rx_len = 13;
            $value.queues.control_requests_rx_len = 17;
            $value.queues.immediate_disconnect_rx_len = 19;
            $value.outbound.inflight = 3;
            $value.outbound.max_inflight = 5;
            $value.outbound.publish_window_full = true;
            $value.outbound.collision = true;
            $value.outbound.collision_notice = true;
            $value.outbound.outbound_drained = false;
            $value.outbound.packet_identifiers_in_use = 23;
            $value.outbound.pending_subscribe = 29;
            $value.outbound.pending_unsubscribe = 31;
            $value.outbound.outgoing_publish = 37;
            $value.outbound.outgoing_publish_notices = 41;
            $value.outbound.outgoing_pubrel = 43;
            $value.outbound.outgoing_pubrel_replay = 47;
            $value.outbound.outgoing_pubrel_notices = 53;
            $value.outbound.incoming_puback = 59;
            $value.outbound.incoming_pub = 61;
            $value.outbound.incoming_pubrec = 67;
            $value.session.session_store_configured = true;
            $value.session.session_store_loaded = true;
            $value.session.session_store_clear_pending = true;
            $value.session.local_session_state_matches_client_id = true;
            $value.config.configured_read_batch_size = 0;
            $value.config.effective_read_batch_size = 71;
            $value.config.max_request_batch = 73;
            let mapped = map_for_test!($map, $value);
            assert!(mapped.connected && mapped.disconnecting && mapped.disconnect_complete);
            assert_eq!(
                [
                    mapped.queues.pending_replay_len,
                    mapped.queues.queued_len,
                    mapped.queues.pending_len,
                    mapped.queues.requests_rx_len,
                    mapped.queues.control_requests_rx_len,
                    mapped.queues.immediate_disconnect_rx_len
                ],
                [7, 11, 18, 13, 17, 19]
            );
            let outbound = &mapped.outbound;
            assert_eq!((outbound.inflight, outbound.max_inflight), (3, 5));
            assert!(
                outbound.publish_window_full && outbound.collision && outbound.collision_notice
            );
            assert!(!outbound.outbound_drained);
            assert_eq!(
                [
                    outbound.packet_identifiers_in_use,
                    outbound.pending_subscribe,
                    outbound.pending_unsubscribe,
                    outbound.outgoing_publish,
                    outbound.outgoing_publish_notices,
                    outbound.outgoing_pubrel,
                    outbound.outgoing_pubrel_replay,
                    outbound.outgoing_pubrel_notices,
                    outbound.incoming_puback,
                    outbound.incoming_pub,
                    outbound.incoming_pubrec
                ],
                [23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67]
            );
            assert!(
                mapped.session.store_configured
                    && mapped.session.store_loaded
                    && mapped.session.store_clear_pending
                    && mapped.session.identity_matches
            );
            assert_eq!(mapped.session.broker_only_session_resume, $broker);
            assert_eq!(
                (
                    mapped.batching.configured_read_batch_size,
                    mapped.batching.effective_read_batch_size,
                    mapped.batching.max_request_batch
                ),
                (0, 71, 73)
            );
            let legacy = mapped.legacy();
            assert_eq!((legacy.pending_requests, legacy.queued_requests), (18, 30));
            assert_eq!(
                (legacy.pending_subscribes, legacy.pending_unsubscribes),
                (29, 31)
            );
            mapped
        }};
    }

    #[test]
    fn complete_native_mapping_preserves_distinct_counts_and_protocol_absence() {
        let (_, v4) = rumqttc_v4::AsyncClient::builder(rumqttc_v4::MqttOptions::new(
            "mapping",
            ("localhost", 1883),
        ))
        .build();
        let mut value = v4.diagnostics();
        let v4 = assert_mapping!(value, v4, None);
        assert!(v4.redirect.is_none());
        let (_, v5) = rumqttc_v5::AsyncClient::builder(rumqttc_v5::MqttOptions::new(
            "mapping",
            ("localhost", 1883),
        ))
        .build();
        let mut value = v5.diagnostics();
        value.session.broker_only_session_resume = true;
        value.redirect.selected_reference = Some("service.invalid".into());
        value.redirect.policy_configured = true;
        value.redirect.attempts = 3;
        value.redirect.attempt_limit = Some(4);
        value.redirect.visited_endpoints = 2;
        value.redirect.active = true;
        value.redirect.target_established = true;
        value.redirect.reason = Some(rumqttc_v5::RedirectReason::ServerMoved);
        value.redirect.srv_owner = Some("_mqtt._tcp.service.invalid".into());
        value.redirect.srv_candidate_index = Some(2);
        value.redirect.srv_candidate_count = Some(3);
        value.redirect.srv_current_target = Some("target.invalid:1883".into());
        let v5 = assert_mapping!(value, v5, Some(true));
        assert_eq!(
            v5.redirect.unwrap(),
            RedirectDiagnostics {
                selected_reference: Some("service.invalid".into()),
                policy_configured: true,
                attempts: 3,
                attempt_limit: Some(4),
                visited_endpoints: 2,
                active: true,
                target_established: true,
                reason: Some(RedirectReason::ServerMoved),
                srv_owner: Some("_mqtt._tcp.service.invalid".into()),
                srv_candidate_index: Some(2),
                srv_candidate_count: Some(3),
                srv_current_target: Some("target.invalid:1883".into()),
            }
        );
    }
}
