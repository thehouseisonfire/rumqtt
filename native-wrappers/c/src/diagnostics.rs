//! Owned synchronous diagnostics; no driver admission or mutable native access.

use std::mem::size_of;
use std::ptr;
use std::time::{Duration, Instant};

use rumqttc_wrapper_core::{ClientDiagnosticsSnapshot, ProtocolVersion};

use super::{
    boundary, client_ref, core_error, destroy_box, rumqttc_client, rumqttc_configuration_status_t,
    rumqttc_error, rumqttc_ordered_shutdown_diagnostics_t, rumqttc_reconnect_diagnostics_t,
    rumqttc_string_view_t, view_string,
};
use crate::error::ErrorHandle;

pub struct rumqttc_diagnostics_snapshot {
    value: ClientDiagnosticsSnapshot,
}

#[repr(C)]
pub struct rumqttc_diagnostics_status_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub lifecycle: u32,
    pub flags: u32,
    pub native_generation: u64,
    pub native_age_ns: u64,
    pub snapshot_age_ns: u64,
}

#[repr(C)]
pub struct rumqttc_diagnostics_group_info_t {
    pub struct_size: u32,
    pub availability: u32,
    pub source: u32,
    pub reserved: u32,
    pub generation: u64,
    pub capture_age_ns: u64,
}

#[repr(C)]
pub struct rumqttc_diagnostics_queues_t {
    pub struct_size: u32,
    pub availability: u32,
    pub pending_replay_len: u64,
    pub queued_len: u64,
    pub pending_len: u64,
    pub requests_rx_len: u64,
    pub control_requests_rx_len: u64,
    pub immediate_disconnect_rx_len: u64,
}

#[repr(C)]
pub struct rumqttc_diagnostics_outbound_t {
    pub struct_size: u32,
    pub availability: u32,
    pub flags: u32,
    pub inflight: u32,
    pub max_inflight: u32,
    pub reserved: u32,
    pub packet_identifiers_in_use: u64,
    pub pending_subscribe: u64,
    pub pending_unsubscribe: u64,
    pub outgoing_publish: u64,
    pub outgoing_publish_notices: u64,
    pub outgoing_pubrel: u64,
    pub outgoing_pubrel_replay: u64,
    pub outgoing_pubrel_notices: u64,
    pub incoming_puback: u64,
    pub incoming_pub: u64,
    pub incoming_pubrec: u64,
}

#[repr(C)]
pub struct rumqttc_diagnostics_session_t {
    pub struct_size: u32,
    pub availability: u32,
    pub flags: u32,
    pub connack_diagnostic: u32,
}

#[repr(C)]
pub struct rumqttc_diagnostics_batching_t {
    pub struct_size: u32,
    pub availability: u32,
    pub configured_read_batch_size: u64,
    pub effective_read_batch_size: u64,
    pub max_request_batch: u64,
}

#[repr(C)]
pub struct rumqttc_diagnostics_redirect_t {
    pub struct_size: u32,
    pub availability: u32,
    pub flags: u32,
    pub reason: u32,
    pub attempts: u64,
    pub attempt_limit: u64,
    pub visited_endpoints: u64,
    pub srv_candidate_index: u64,
    pub srv_candidate_count: u64,
    pub selected_reference: rumqttc_string_view_t,
    pub srv_owner: rumqttc_string_view_t,
    pub srv_current_target: rumqttc_string_view_t,
}
// Stable values are declared with names in the public C header.
const AVAILABLE: u32 = 1;
const FEATURE_DISABLED: u32 = 2;
const INAPPLICABLE: u32 = 3;
const NOT_OBSERVED: u32 = 4;

fn nanos(value: Duration) -> u64 {
    u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
}
fn millis(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}
fn flag(value: bool, bit: u32) -> u32 {
    u32::from(value) << bit
}

/// All callers use C records with an initial u32 size and zero-valid fields.
/// Only the known writable prefix is cleared; future extension bytes are untouched.
unsafe fn reset<'a, T>(out: *mut T) -> Result<&'a mut T, ErrorHandle> {
    if out.is_null() {
        return Err(ErrorHandle::argument("diagnostics output is NULL"));
    }
    let size = unsafe { out.cast::<u32>().read() } as usize;
    unsafe {
        ptr::write_bytes(
            out.cast::<u8>().add(size_of::<u32>()),
            0,
            size.min(size_of::<T>()).saturating_sub(size_of::<u32>()),
        );
    }
    if size < size_of::<T>() {
        return Err(ErrorHandle::argument("diagnostics output is too small"));
    }
    Ok(unsafe { &mut *out })
}
unsafe fn snapshot<'a>(
    value: *const rumqttc_diagnostics_snapshot,
) -> Result<&'a ClientDiagnosticsSnapshot, ErrorHandle> {
    Ok(&unsafe { value.as_ref() }
        .ok_or_else(|| ErrorHandle::argument("diagnostics snapshot is NULL"))?
        .value)
}
const fn native_availability(value: &ClientDiagnosticsSnapshot) -> u32 {
    if value.native.is_some() {
        AVAILABLE
    } else {
        NOT_OBSERVED
    }
}
fn redirect_availability(value: &ClientDiagnosticsSnapshot) -> u32 {
    if value.protocol == ProtocolVersion::V4 {
        INAPPLICABLE
    } else {
        native_availability(value)
    }
}
const fn ordered_availability(value: &ClientDiagnosticsSnapshot, wrapper: bool) -> u32 {
    if !cfg!(feature = "ordered-shutdown") {
        FEATURE_DISABLED
    } else if wrapper {
        if value.ordered_wrapper.is_some() {
            AVAILABLE
        } else {
            NOT_OBSERVED
        }
    } else {
        native_availability(value)
    }
}
fn optional_view(value: Option<&str>) -> rumqttc_string_view_t {
    value.map_or(
        rumqttc_string_view_t {
            data: ptr::null(),
            len: 0,
        },
        view_string,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_diagnostics_snapshot(
    client: *mut rumqttc_client,
    out: *mut *mut rumqttc_diagnostics_snapshot,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("diagnostics snapshot output is NULL"));
        }
        let value = unsafe { client_ref(client) }?.handle.diagnostics_snapshot();
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_diagnostics_snapshot { value }));
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_destroy(
    value: *mut rumqttc_diagnostics_snapshot,
) {
    unsafe { destroy_box(value) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_status(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_status_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.protocol = match value.protocol {
            ProtocolVersion::V4 => 1,
            ProtocolVersion::V5 => 2,
        };
        out.lifecycle = value.lifecycle as u32;
        out.flags = flag(value.terminated, 0) | flag(value.native.is_some(), 1);
        out.snapshot_age_ns = nanos(value.captured_at.elapsed());
        if let Some(native) = &value.native {
            out.native_generation = native.generation;
            out.native_age_ns = nanos(native.captured_at.elapsed());
            out.flags |= flag(native.connected, 2)
                | flag(native.disconnecting, 3)
                | flag(native.disconnect_complete, 4);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_group_info(
    value: *const rumqttc_diagnostics_snapshot,
    group: u32,
    out: *mut rumqttc_diagnostics_group_info_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        let mut capture: Option<(Instant, u64)> = None;
        match group {
            1..=7 => {
                out.source = 1;
                out.availability = match group {
                    6 => redirect_availability(value),
                    7 => ordered_availability(value, false),
                    _ => native_availability(value),
                };
                if out.availability == AVAILABLE {
                    capture = value
                        .native
                        .as_ref()
                        .map(|native| (native.captured_at, native.generation));
                }
            }
            8 => {
                out.source = 2;
                out.availability = ordered_availability(value, true);
                capture = value
                    .ordered_wrapper
                    .as_ref()
                    .map(|ordered| (ordered.captured_at, ordered.fence_sequence.unwrap_or(0)));
            }
            9 => {
                out.source = 3;
                out.availability = AVAILABLE;
                capture = Some((value.reconnect.captured_at, value.reconnect.cycles_started));
            }
            10..=12 => {
                out.source = if group == 12 { 5 } else { 4 };
                out.availability = if value.configuration.is_some() {
                    AVAILABLE
                } else {
                    NOT_OBSERVED
                };
                capture = value
                    .configuration
                    .as_ref()
                    .map(|configuration| match group {
                        10 => (configuration.captured_at, configuration.revision),
                        11 => (
                            configuration.effective_captured_at,
                            configuration.effective_tuning_revision,
                        ),
                        _ => (
                            configuration.connection.captured_at,
                            configuration.connection.attempt,
                        ),
                    });
            }
            _ => return Err(ErrorHandle::argument("unknown diagnostics group")),
        }
        if let Some((at, generation)) = capture {
            out.capture_age_ns = nanos(at.elapsed());
            out.generation = generation;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_queues(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_queues_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.availability = native_availability(value);
        if let Some(native) = &value.native {
            let queues = &native.queues;
            out.pending_replay_len = queues.pending_replay_len as u64;
            out.queued_len = queues.queued_len as u64;
            out.pending_len = queues.pending_len as u64;
            out.requests_rx_len = queues.requests_rx_len as u64;
            out.control_requests_rx_len = queues.control_requests_rx_len as u64;
            out.immediate_disconnect_rx_len = queues.immediate_disconnect_rx_len as u64;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_outbound(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_outbound_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.availability = native_availability(value);
        if let Some(native) = &value.native {
            let outbound = &native.outbound;
            out.flags = flag(outbound.publish_window_full, 0)
                | flag(outbound.collision, 1)
                | flag(outbound.collision_notice, 2)
                | flag(outbound.outbound_drained, 3);
            out.inflight = u32::from(outbound.inflight);
            out.max_inflight = u32::from(outbound.max_inflight);
            out.packet_identifiers_in_use = outbound.packet_identifiers_in_use as u64;
            out.pending_subscribe = outbound.pending_subscribe as u64;
            out.pending_unsubscribe = outbound.pending_unsubscribe as u64;
            out.outgoing_publish = outbound.outgoing_publish as u64;
            out.outgoing_publish_notices = outbound.outgoing_publish_notices as u64;
            out.outgoing_pubrel = outbound.outgoing_pubrel as u64;
            out.outgoing_pubrel_replay = outbound.outgoing_pubrel_replay as u64;
            out.outgoing_pubrel_notices = outbound.outgoing_pubrel_notices as u64;
            out.incoming_puback = outbound.incoming_puback as u64;
            out.incoming_pub = outbound.incoming_pub as u64;
            out.incoming_pubrec = outbound.incoming_pubrec as u64;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_session(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_session_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.availability = native_availability(value);
        if let Some(native) = &value.native {
            let session = &native.session;
            out.flags = flag(session.store_configured, 0)
                | flag(session.store_loaded, 1)
                | flag(session.store_clear_pending, 2)
                | flag(session.identity_matches, 3)
                | flag(session.broker_only_session_resume.is_some(), 7)
                | flag(session.broker_only_session_resume.unwrap_or(false), 8);
            if let Some(connack) = session.connack {
                out.flags |=
                    16 | flag(connack.raw_session_present, 5) | flag(connack.session_resumed, 6);
                out.connack_diagnostic = match connack.diagnostic {
                    None => 0,
                    Some(rumqttc_wrapper_core::ConnAckDiagnostic::SessionPresentMismatchAcceptedAsClean) => 1,
                    Some(rumqttc_wrapper_core::ConnAckDiagnostic::BrokerOnlySessionResume) => 2,
                };
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_batching(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_batching_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.availability = native_availability(value);
        if let Some(native) = &value.native {
            out.configured_read_batch_size = native.batching.configured_read_batch_size as u64;
            out.effective_read_batch_size = native.batching.effective_read_batch_size as u64;
            out.max_request_batch = native.batching.max_request_batch as u64;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_redirect(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_diagnostics_redirect_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        out.availability = redirect_availability(value);
        if let Some(redirect) = value
            .native
            .as_ref()
            .and_then(|native| native.redirect.as_ref())
        {
            out.flags = flag(redirect.policy_configured, 0)
                | flag(redirect.active, 1)
                | flag(redirect.target_established, 2)
                | flag(redirect.attempt_limit.is_some(), 3)
                | flag(redirect.reason.is_some(), 4)
                | flag(redirect.srv_candidate_index.is_some(), 5)
                | flag(redirect.srv_candidate_count.is_some(), 6)
                | flag(redirect.selected_reference.is_some(), 7)
                | flag(redirect.srv_owner.is_some(), 8)
                | flag(redirect.srv_current_target.is_some(), 9);
            out.reason = match redirect.reason {
                None => 0,
                Some(rumqttc_wrapper_core::RedirectReason::UseAnotherServer) => 1,
                Some(rumqttc_wrapper_core::RedirectReason::ServerMoved) => 2,
            };
            out.attempts = redirect.attempts as u64;
            out.attempt_limit = redirect.attempt_limit.unwrap_or(0) as u64;
            out.visited_endpoints = redirect.visited_endpoints as u64;
            out.srv_candidate_index = redirect.srv_candidate_index.unwrap_or(0) as u64;
            out.srv_candidate_count = redirect.srv_candidate_count.unwrap_or(0) as u64;
            out.selected_reference = optional_view(redirect.selected_reference.as_deref());
            out.srv_owner = optional_view(redirect.srv_owner.as_deref());
            out.srv_current_target = optional_view(redirect.srv_current_target.as_deref());
        }
        Ok(())
    })
}

unsafe fn ordered(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_ordered_shutdown_diagnostics_t,
    wrapper: bool,
) -> Result<(), ErrorHandle> {
    let out = unsafe { reset(out) }?;
    let value = unsafe { snapshot(value) }?;
    if ordered_availability(value, wrapper) != AVAILABLE {
        return Ok(());
    }
    let ordered = if wrapper {
        value.ordered_wrapper.as_deref()
    } else {
        value
            .native
            .as_ref()
            .and_then(|native| native.ordered.as_deref())
    };
    if let Some(ordered) = ordered {
        out.present = 1;
        out.phase = ordered.phase as u32;
        out.fence_present = u8::from(ordered.fence_sequence.is_some());
        out.fence_sequence = ordered.fence_sequence.unwrap_or(0);
        out.deadline_present = u8::from(ordered.remaining_at_capture.is_some());
        out.remaining_at_capture_ms = millis(ordered.remaining_at_capture.unwrap_or_default());
        out.local_count_present = u8::from(ordered.local_queued_publishes.is_some());
        out.local_queued_publishes = ordered.local_queued_publishes.unwrap_or(0) as u64;
        out.snapshot_age_ms = millis(ordered.captured_at.elapsed());
    }
    Ok(())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_ordered_native(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_ordered_shutdown_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || unsafe {
        ordered(value, out, false)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_ordered_wrapper(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_ordered_shutdown_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || unsafe {
        ordered(value, out, true)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_reconnect(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_reconnect_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        unsafe { super::reconnect::fill(out, &value.reconnect) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_last_failure(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut *mut rumqttc_error,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || out == error_out {
            return Err(ErrorHandle::argument("invalid failure output"));
        }
        let value = unsafe { snapshot(value) }?;
        if let Some(failure) = &value.reconnect.last_failure {
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_error {
                    inner: core_error(failure, None),
                }));
            }
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_diagnostics_snapshot_configuration(
    value: *const rumqttc_diagnostics_snapshot,
    out: *mut rumqttc_configuration_status_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let out = unsafe { reset(out) }?;
        let value = unsafe { snapshot(value) }?;
        if let Some(configuration) = &value.configuration {
            super::configuration::fill_status(out, configuration);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_records_follow_c_capabilities_even_when_core_observations_are_present() {
        use rumqttc_wrapper_core::{
            ClientConfig, NativeClient, OrderedShutdownDiagnostics, OrderedShutdownPhase,
        };
        let client =
            NativeClient::start(ClientConfig::v4("ordered-views", "127.0.0.1", 1)).unwrap();
        let mut value = client.handle().diagnostics_snapshot();
        let observation = Box::new(OrderedShutdownDiagnostics {
            phase: OrderedShutdownPhase::Draining,
            fence_sequence: Some(41),
            remaining_at_capture: Some(Duration::from_secs(2)),
            local_queued_publishes: Some(3),
            captured_at: Instant::now(),
        });
        value.ordered_wrapper = Some(observation.clone());
        std::sync::Arc::make_mut(value.native.as_mut().unwrap()).ordered = Some(observation);
        let owned = rumqttc_diagnostics_snapshot { value };
        client.closer().close_now(Duration::from_secs(5)).unwrap();
        drop(client);
        let enabled =
            super::super::rumqttc_library_capabilities() & super::super::CAP_ORDERED_SHUTDOWN != 0;
        for wrapper in [false, true] {
            let mut metadata: rumqttc_diagnostics_group_info_t = unsafe { std::mem::zeroed() };
            metadata.struct_size = u32::try_from(size_of_val(&metadata)).unwrap();
            assert_eq!(
                unsafe {
                    rumqttc_diagnostics_snapshot_group_info(
                        &raw const owned,
                        7 + u32::from(wrapper),
                        &raw mut metadata,
                        ptr::null_mut(),
                    )
                },
                0
            );
            assert_eq!(
                metadata.availability,
                if enabled { AVAILABLE } else { FEATURE_DISABLED }
            );
            let mut out: rumqttc_ordered_shutdown_diagnostics_t = unsafe { std::mem::zeroed() };
            // All fields admit arbitrary integer bytes; detect stale output even on a successful absent read.
            unsafe { ptr::write_bytes((&raw mut out).cast::<u8>(), 0xff, size_of_val(&out)) };
            out.struct_size = u32::try_from(size_of_val(&out)).unwrap();
            let status = unsafe {
                if wrapper {
                    rumqttc_diagnostics_snapshot_ordered_wrapper(
                        &raw const owned,
                        &raw mut out,
                        ptr::null_mut(),
                    )
                } else {
                    rumqttc_diagnostics_snapshot_ordered_native(
                        &raw const owned,
                        &raw mut out,
                        ptr::null_mut(),
                    )
                }
            };
            assert_eq!(status, 0);
            assert_eq!(out.struct_size as usize, size_of_val(&out));
            assert_eq!(out.present, u8::from(enabled));
            if enabled {
                assert_eq!(out.phase, OrderedShutdownPhase::Draining as u32);
                assert_eq!((out.fence_present, out.fence_sequence), (1, 41));
                assert_eq!(
                    (out.deadline_present, out.remaining_at_capture_ms),
                    (1, 2000)
                );
                assert_eq!(
                    (out.local_count_present, out.local_queued_publishes),
                    (1, 3)
                );
            } else {
                let bytes = unsafe {
                    std::slice::from_raw_parts((&raw const out).cast::<u8>(), size_of_val(&out))
                };
                assert!(bytes[4..].iter().all(|byte| *byte == 0));
            }
        }
    }

    #[test]
    fn failed_accessors_initialize_outputs_and_preserve_extension_and_short_prefix() {
        macro_rules! failure {
            ($ty:ty, $accessor:ident $(, $argument:expr)?) => {{
                // These C records contain only integer and raw-pointer fields.
                let mut out: $ty = unsafe { std::mem::zeroed() };
                unsafe { ptr::write_bytes((&raw mut out).cast::<u8>(), 0xff, size_of::<$ty>()); }
                out.struct_size = u32::try_from(size_of::<$ty>()).unwrap();
                assert_eq!(unsafe { $accessor(ptr::null() $(, $argument)?, &raw mut out, ptr::null_mut()) }, 1);
                let bytes = unsafe { std::slice::from_raw_parts((&raw const out).cast::<u8>(), size_of::<$ty>()) };
                assert!(bytes[4..].iter().all(|byte| *byte == 0));
                assert_eq!(out.struct_size as usize, size_of::<$ty>());
            }};
        }
        failure!(
            rumqttc_diagnostics_status_t,
            rumqttc_diagnostics_snapshot_status
        );
        failure!(
            rumqttc_diagnostics_group_info_t,
            rumqttc_diagnostics_snapshot_group_info,
            1
        );
        failure!(
            rumqttc_diagnostics_queues_t,
            rumqttc_diagnostics_snapshot_queues
        );
        failure!(
            rumqttc_diagnostics_outbound_t,
            rumqttc_diagnostics_snapshot_outbound
        );
        failure!(
            rumqttc_diagnostics_session_t,
            rumqttc_diagnostics_snapshot_session
        );
        failure!(
            rumqttc_diagnostics_batching_t,
            rumqttc_diagnostics_snapshot_batching
        );
        failure!(
            rumqttc_diagnostics_redirect_t,
            rumqttc_diagnostics_snapshot_redirect
        );
        failure!(
            rumqttc_ordered_shutdown_diagnostics_t,
            rumqttc_diagnostics_snapshot_ordered_native
        );
        failure!(
            rumqttc_ordered_shutdown_diagnostics_t,
            rumqttc_diagnostics_snapshot_ordered_wrapper
        );
        failure!(
            rumqttc_reconnect_diagnostics_t,
            rumqttc_diagnostics_snapshot_reconnect
        );
        failure!(
            rumqttc_configuration_status_t,
            rumqttc_diagnostics_snapshot_configuration
        );
    }

    #[test]
    fn failed_accessors_preserve_short_and_extended_record_storage() {
        #[repr(C)]
        struct Short {
            size: u32,
            writable: u32,
            sentinel: u64,
        }
        #[repr(C)]
        struct Extended {
            record: rumqttc_diagnostics_status_t,
            sentinel: u64,
        }
        let mut short = Short {
            size: 8,
            writable: u32::MAX,
            sentinel: u64::MAX,
        };
        assert_eq!(
            unsafe {
                rumqttc_diagnostics_snapshot_status(
                    ptr::null(),
                    (&raw mut short).cast(),
                    ptr::null_mut(),
                )
            },
            1
        );
        assert_eq!(
            (short.size, short.writable, short.sentinel),
            (8, 0, u64::MAX)
        );
        let mut extended = Extended {
            record: unsafe { std::mem::zeroed() },
            sentinel: u64::MAX,
        };
        extended.record.struct_size = u32::try_from(size_of::<Extended>()).unwrap();
        assert_eq!(
            unsafe {
                rumqttc_diagnostics_snapshot_status(
                    ptr::null(),
                    &raw mut extended.record,
                    ptr::null_mut(),
                )
            },
            1
        );
        assert_eq!(extended.sentinel, u64::MAX);
        let mut acquired = ptr::dangling_mut();
        assert_eq!(
            unsafe {
                rumqttc_client_diagnostics_snapshot(
                    ptr::null_mut(),
                    &raw mut acquired,
                    ptr::null_mut(),
                )
            },
            1
        );
        assert!(acquired.is_null());
        let mut failure = ptr::dangling_mut();
        assert_eq!(
            unsafe {
                rumqttc_diagnostics_snapshot_last_failure(
                    ptr::null(),
                    &raw mut failure,
                    ptr::null_mut(),
                )
            },
            1
        );
        assert!(failure.is_null());
        assert_eq!(
            unsafe {
                rumqttc_diagnostics_snapshot_last_failure(
                    ptr::null(),
                    &raw mut failure,
                    &raw mut failure,
                )
            },
            1
        );
        assert!(!failure.is_null());
        unsafe {
            destroy_box(failure);
            rumqttc_diagnostics_snapshot_destroy(ptr::null_mut());
        }
    }

    #[test]
    fn nonempty_redirect_views_borrow_owned_snapshot_and_keep_presence_semantics() {
        use rumqttc_wrapper_core::{ClientConfig, NativeClient, RedirectReason};
        let client = NativeClient::start(ClientConfig::v5("views", "127.0.0.1", 1)).unwrap();
        let mut value = client.handle().diagnostics_snapshot();
        let native = std::sync::Arc::make_mut(value.native.as_mut().unwrap());
        let redirect = native.redirect.as_mut().unwrap();
        redirect.selected_reference = Some("_mqtt._tcp.service.invalid".into());
        redirect.srv_owner = Some("_mqtt._tcp.service.invalid".into());
        redirect.srv_current_target = Some("target.invalid:1883".into());
        redirect.srv_candidate_index = Some(1);
        redirect.srv_candidate_count = Some(2);
        redirect.attempt_limit = Some(0); // present zero differs from absent
        redirect.reason = Some(RedirectReason::ServerMoved);
        let owned = rumqttc_diagnostics_snapshot { value };
        client.closer().close_now(Duration::from_secs(5)).unwrap();
        drop(client);
        let mut out: rumqttc_diagnostics_redirect_t = unsafe { std::mem::zeroed() };
        out.struct_size = u32::try_from(size_of::<rumqttc_diagnostics_redirect_t>()).unwrap();
        assert_eq!(
            unsafe {
                rumqttc_diagnostics_snapshot_redirect(
                    &raw const owned,
                    &raw mut out,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(out.availability, AVAILABLE);
        assert_ne!(out.flags & (1 << 3), 0);
        assert_eq!(out.attempt_limit, 0);
        assert_eq!(
            (out.srv_candidate_index, out.srv_candidate_count, out.reason),
            (1, 2, 2)
        );
        let bytes = unsafe {
            std::slice::from_raw_parts(
                out.srv_current_target.data.cast::<u8>(),
                out.srv_current_target.len,
            )
        };
        assert_eq!(bytes, b"target.invalid:1883");
        let copy = bytes.to_vec();
        drop(owned);
        assert_eq!(copy, b"target.invalid:1883");
    }
}
