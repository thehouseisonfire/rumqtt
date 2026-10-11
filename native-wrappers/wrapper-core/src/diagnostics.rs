//! Immutable native captures and independently sampled wrapper observations.

use std::sync::Arc;
use std::time::Instant;

use crate::{
    ConfigurationSnapshot, ConnAckSessionDiagnostics, DiagnosticsSnapshot, LifecycleState,
    OrderedShutdownDiagnostics, ProtocolVersion, ReconnectDiagnostics, RedirectReason,
};

/// Owned observation. Native data can be indefinitely older than assembly time.
/// Groups are sampled independently; producer channel lengths are not transactional.
#[derive(Clone, Debug)]
pub struct ClientDiagnosticsSnapshot {
    pub protocol: ProtocolVersion,
    pub lifecycle: LifecycleState,
    pub terminated: bool,
    pub captured_at: Instant,
    pub native: Option<Arc<NativeDiagnosticsSnapshot>>,
    pub reconnect: ReconnectDiagnostics,
    pub configuration: Option<ConfigurationSnapshot>,
    /// Wrapper fence admission/result, never a fresh native queue observation.
    pub ordered_wrapper: Option<Box<OrderedShutdownDiagnostics>>,
}

/// One completed native capture. Copies never advance its generation or timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDiagnosticsSnapshot {
    pub generation: u64,
    pub captured_at: Instant,
    pub connected: bool,
    pub disconnecting: bool,
    pub disconnect_complete: bool,
    pub queues: QueueDiagnostics,
    pub outbound: OutboundDiagnostics,
    pub session: SessionDiagnostics,
    pub batching: BatchingDiagnostics,
    pub redirect: Option<RedirectDiagnostics>,
    /// Available with ordered-shutdown, including Open before fence admission.
    pub ordered: Option<Box<OrderedShutdownDiagnostics>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueDiagnostics {
    pub pending_replay_len: usize,
    pub queued_len: usize,
    /// `pending_replay_len + queued_len`, not an additional queue.
    pub pending_len: usize,
    pub requests_rx_len: usize,
    pub control_requests_rx_len: usize,
    pub immediate_disconnect_rx_len: usize,
}

/// Counts overlap; outgoing notices are subsets of their corresponding flows.
/// Incoming acknowledgement tracking is excluded from `outbound_drained`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundDiagnostics {
    pub inflight: u16,
    pub max_inflight: u16,
    pub publish_window_full: bool,
    pub packet_identifiers_in_use: usize,
    pub collision: bool,
    pub collision_notice: bool,
    pub pending_subscribe: usize,
    pub pending_unsubscribe: usize,
    pub outgoing_publish: usize,
    pub outgoing_publish_notices: usize,
    pub outgoing_pubrel: usize,
    pub outgoing_pubrel_replay: usize,
    pub outgoing_pubrel_notices: usize,
    pub incoming_puback: usize,
    pub incoming_pub: usize,
    pub incoming_pubrec: usize,
    pub outbound_drained: bool,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDiagnostics {
    pub connack: Option<ConnAckSessionDiagnostics>,
    pub store_configured: bool,
    pub store_loaded: bool,
    pub store_clear_pending: bool,
    pub identity_matches: bool,
    pub broker_only_session_resume: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchingDiagnostics {
    pub configured_read_batch_size: usize,
    pub effective_read_batch_size: usize,
    pub max_request_batch: usize,
}

/// Current native redirect observation, distinct from retained Redirect events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectDiagnostics {
    pub selected_reference: Option<String>,
    pub policy_configured: bool,
    pub attempts: usize,
    pub attempt_limit: Option<usize>,
    pub visited_endpoints: usize,
    pub active: bool,
    pub target_established: bool,
    pub reason: Option<RedirectReason>,
    pub srv_owner: Option<String>,
    pub srv_candidate_index: Option<usize>,
    pub srv_candidate_count: Option<usize>,
    pub srv_current_target: Option<String>,
}

impl NativeDiagnosticsSnapshot {
    pub(crate) fn legacy(&self) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            reconnect: None,
            ordered_shutdown: self
                .ordered
                .as_ref()
                .filter(|ordered| ordered.fence_sequence.is_some())
                .cloned(),
            connack: self.session.connack,
            connected: self.connected,
            disconnecting: self.disconnecting,
            pending_requests: self.queues.pending_len,
            queued_requests: self.queues.requests_rx_len + self.queues.control_requests_rx_len,
            inflight_publishes: self.outbound.inflight,
            max_inflight_publishes: self.outbound.max_inflight,
            pending_subscribes: self.outbound.pending_subscribe,
            pending_unsubscribes: self.outbound.pending_unsubscribe,
            outbound_drained: self.outbound.outbound_drained,
        }
    }
}
