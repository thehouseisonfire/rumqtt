//! Deferred MQTT admission. Foreign memory is borrowed only during creation.
use super::{
    ack_token, boolean, boundary, bytes_from_view, client_ref, config_update, core_error,
    destroy_box, filters, parse_acknowledgement_options, publish_command, rumqttc_ack_token,
    rumqttc_acknowledgement_options_t, rumqttc_bytes_view_t, rumqttc_client, rumqttc_completion,
    rumqttc_config, rumqttc_error, rumqttc_publish_options_t, rumqttc_string_view_t,
    rumqttc_subscribe_options_t, rumqttc_subscription_t, rumqttc_unsubscribe_options_t,
    rumqttc_user_property_t, struct_size, subscribe_protocol_options, subscriptions,
    unsubscribe_protocol_options, write_admission, write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{Command, SubscribeCommand, Subscription, UnsubscribeCommand};
use rumqttc_wrapper_core::{
    PendingAdmissionHandle, PendingAdmissionLimits, PendingAdmissionOutcome,
};
use std::{ptr, slice, time::Duration};

pub struct rumqttc_pending_admission {
    inner: PendingAdmissionHandle,
}

#[repr(C)]
pub struct rumqttc_pending_admission_limits_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub max_pending: usize,
    pub max_bytes: usize,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_pending_admission_limits(
    config: *mut rumqttc_config,
    limits: *const rumqttc_pending_admission_limits_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        if limits.is_null() {
            return Err(ErrorHandle::argument("pending limits are NULL"));
        }
        if unsafe { limits.cast::<u32>().read() }
            < struct_size::<rumqttc_pending_admission_limits_t>()
        {
            return Err(ErrorHandle::argument("pending limits record is too small"));
        }
        let limits = unsafe { &*limits };
        if limits.reserved != 0 || limits.max_pending == 0 || limits.max_bytes == 0 {
            return Err(ErrorHandle::argument("invalid pending limits"));
        }
        config.common.pending_admission_limits = PendingAdmissionLimits {
            max_pending: limits.max_pending,
            max_bytes: limits.max_bytes,
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_reset_pending_admission_limits(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.pending_admission_limits = PendingAdmissionLimits::default();
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_pending_admission_snapshot(
    client: *const rumqttc_client,
    pending_out: *mut usize,
    retained_bytes_out: *mut usize,
    max_pending_out: *mut usize,
    max_bytes_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(pending_out, 0);
        write_optional(retained_bytes_out, 0);
        write_optional(max_pending_out, 0);
        write_optional(max_bytes_out, 0);
    }
    boundary(error_out, client.cast_mut(), || {
        if pending_out.is_null()
            && retained_bytes_out.is_null()
            && max_pending_out.is_null()
            && max_bytes_out.is_null()
        {
            return Err(ErrorHandle::argument("pending snapshot requires an output"));
        }
        let snapshot = unsafe { client_ref(client.cast_mut()) }?
            .handle
            .pending_admission_snapshot();
        unsafe {
            write_optional(pending_out, snapshot.pending);
            write_optional(retained_bytes_out, snapshot.retained_bytes);
            write_optional(max_pending_out, snapshot.limits.max_pending);
            write_optional(max_bytes_out, snapshot.limits.max_bytes);
        }
        Ok(())
    })
}

unsafe fn pending_ref<'a>(
    handle: *const rumqttc_pending_admission,
) -> Result<&'a PendingAdmissionHandle, ErrorHandle> {
    if handle.is_null() {
        return Err(ErrorHandle::argument("pending admission is NULL"));
    }
    Ok(&unsafe { &*handle }.inner)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_pending_admission_retain(
    handle: *const rumqttc_pending_admission,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("pending admission output is NULL"));
        }
        let inner = unsafe { pending_ref(handle) }?.clone();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_pending_admission { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_pending_admission_release(handle: *mut rumqttc_pending_admission) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_pending_admission_cancel(
    handle: *const rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { pending_ref(handle) }?.cancel();
        Ok(())
    })
}

fn observe(
    handle: *const rumqttc_pending_admission,
    timeout: Option<Duration>,
    state_out: *mut u32,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(state_out, 0);
        write_optional(operation_id_out, 0);
        write_optional(completion_out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if state_out.is_null() {
            return Err(ErrorHandle::argument("admission state output is NULL"));
        }
        let handle = unsafe { pending_ref(handle) }?;
        let outcome = match timeout {
            Some(timeout) => match handle.wait_timeout(timeout) {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    unsafe { *state_out = u32::from(handle.cancellation_requested()) };
                    return Err(core_error(&error, None));
                }
            },
            None => handle.poll(),
        };
        match outcome {
            None => {
                unsafe { *state_out = u32::from(handle.cancellation_requested()) };
                Err(ErrorHandle::would_block("admission is still pending"))
            }
            Some(PendingAdmissionOutcome::Admitted(admission)) => {
                unsafe { *state_out = 2 };
                write_admission(admission, operation_id_out, completion_out);
                Ok(())
            }
            Some(PendingAdmissionOutcome::Cancelled) => {
                unsafe { *state_out = 3 };
                Ok(())
            }
            Some(PendingAdmissionOutcome::Failed(error)) => {
                unsafe { *state_out = 4 };
                Err(core_error(&error, None))
            }
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_pending_admission_poll(
    handle: *const rumqttc_pending_admission,
    state_out: *mut u32,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe(
        handle,
        None,
        state_out,
        operation_id_out,
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_pending_admission_wait_timeout_ms(
    handle: *const rumqttc_pending_admission,
    timeout_ms: u64,
    state_out: *mut u32,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe(
        handle,
        Some(Duration::from_millis(timeout_ms)),
        state_out,
        operation_id_out,
        completion_out,
        error_out,
    )
}

fn create(
    client: *mut rumqttc_client,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
    preflight: impl FnOnce() -> Result<usize, ErrorHandle>,
    build: impl FnOnce() -> Result<Command, ErrorHandle>,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, client, || {
        if out.is_null() {
            return Err(ErrorHandle::argument("pending admission output is NULL"));
        }
        let client = unsafe { client_ref(client) }?;
        let bytes = preflight()?;
        let reservation = client
            .handle
            .reserve_pending_admission(bytes)
            .map_err(|e| core_error(&e, None))?;
        let command = build()?;
        let inner = client
            .handle
            .defer_admission(reservation, command)
            .map_err(|e| core_error(&e, None))?;
        unsafe { *out = Box::into_raw(Box::new(rumqttc_pending_admission { inner })) };
        Ok(())
    })
}

fn add(a: usize, b: usize) -> Result<usize, ErrorHandle> {
    a.checked_add(b)
        .ok_or_else(|| ErrorHandle::argument("pending command size overflow"))
}

unsafe fn record<'a, T>(value: *const T) -> Result<&'a T, ErrorHandle> {
    if value.is_null() || unsafe { value.cast::<u32>().read() } < struct_size::<T>() {
        return Err(ErrorHandle::argument(
            "missing or undersized command record",
        ));
    }
    Ok(unsafe { &*value })
}

unsafe fn array<'a, T>(values: *const T, count: usize) -> Result<&'a [T], ErrorHandle> {
    if count == 0 {
        return Ok(&[]);
    }
    if values.is_null() || count > isize::MAX as usize / size_of::<T>() {
        return Err(ErrorHandle::argument("invalid command array"));
    }
    Ok(unsafe { slice::from_raw_parts(values, count) })
}

unsafe fn text_charge(view: rumqttc_string_view_t) -> Result<usize, ErrorHandle> {
    if view.len > usize::from(u16::MAX) {
        return Err(ErrorHandle::argument("MQTT string is too long"));
    }
    let raw = rumqttc_bytes_view_t {
        data: view.data.cast(),
        len: view.len,
    };
    let value = std::str::from_utf8(unsafe { bytes_from_view(raw) }?)
        .map_err(|_| ErrorHandle::argument("invalid UTF-8 string"))?;
    if value.contains('\0') {
        return Err(ErrorHandle::argument("MQTT string contains NUL"));
    }
    Ok(value.len())
}

unsafe fn properties_charge(
    values: *const rumqttc_user_property_t,
    count: usize,
) -> Result<usize, ErrorHandle> {
    let mut bytes = count
        .checked_mul(size_of::<(String, String)>())
        .ok_or_else(|| ErrorHandle::argument("property count overflow"))?;
    for p in unsafe { array(values, count) }? {
        let p = unsafe { record(p) }?;
        bytes = add(bytes, unsafe { text_charge(p.name) }?)?;
        bytes = add(bytes, unsafe { text_charge(p.value) }?)?;
    }
    Ok(bytes)
}

unsafe fn publish_charge(
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
) -> Result<usize, ErrorHandle> {
    let mut bytes = add(
        unsafe { text_charge(topic) }?,
        unsafe { bytes_from_view(payload) }?.len(),
    )?;
    if !options.is_null() {
        let options = unsafe { record(options) }?;
        if !options.v5_properties.is_null() {
            let p = unsafe { record(options.v5_properties) }?;
            bytes = add(bytes, unsafe {
                properties_charge(p.user_properties, p.user_property_count)
            }?)?;
            if boolean(p.response_topic_present, "response_topic_present")? {
                bytes = add(bytes, unsafe { text_charge(p.response_topic) }?)?;
            }
            if boolean(p.content_type_present, "content_type_present")? {
                bytes = add(bytes, unsafe { text_charge(p.content_type) }?)?;
            }
            if boolean(p.correlation_data_present, "correlation_data_present")? {
                if p.correlation_data.len > usize::from(u16::MAX) {
                    return Err(ErrorHandle::argument("correlation data is too long"));
                }
                bytes = add(bytes, unsafe { bytes_from_view(p.correlation_data) }?.len())?;
            }
        }
    }
    Ok(bytes)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_publish_deferred(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    create(
        client,
        out,
        error_out,
        || unsafe { publish_charge(topic, payload, options) },
        || unsafe { publish_command(topic, payload, options) }.map(Command::Publish),
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_subscribe_deferred(
    client: *mut rumqttc_client,
    values: *const rumqttc_subscription_t,
    count: usize,
    options: *const rumqttc_subscribe_options_t,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    create(
        client,
        out,
        error_out,
        || {
            let mut bytes = count
                .checked_mul(size_of::<Subscription>())
                .ok_or_else(|| ErrorHandle::argument("filter count overflow"))?;
            for value in unsafe { array(values, count) }? {
                bytes = add(bytes, unsafe { text_charge(record(value)?.filter) }?)?;
            }
            if !options.is_null() {
                let options = unsafe { record(options) }?;
                if !options.v5_properties.is_null() {
                    let p = unsafe { record(options.v5_properties) }?;
                    bytes = add(bytes, unsafe {
                        properties_charge(p.user_properties, p.user_property_count)
                    }?)?;
                }
            }
            Ok(bytes)
        },
        || {
            Ok(Command::Subscribe(SubscribeCommand {
                filters: unsafe { subscriptions(values, count) }?,
                protocol: unsafe { subscribe_protocol_options(options) }?,
            }))
        },
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_unsubscribe_deferred(
    client: *mut rumqttc_client,
    values: *const rumqttc_string_view_t,
    count: usize,
    options: *const rumqttc_unsubscribe_options_t,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    create(
        client,
        out,
        error_out,
        || {
            let mut bytes = count
                .checked_mul(size_of::<String>())
                .ok_or_else(|| ErrorHandle::argument("filter count overflow"))?;
            for value in unsafe { array(values, count) }? {
                bytes = add(bytes, unsafe { text_charge(*value) }?)?;
            }
            if !options.is_null() {
                let options = unsafe { record(options) }?;
                if !options.v5_properties.is_null() {
                    let p = unsafe { record(options.v5_properties) }?;
                    bytes = add(bytes, unsafe {
                        properties_charge(p.user_properties, p.user_property_count)
                    }?)?;
                }
            }
            Ok(bytes)
        },
        || {
            Ok(Command::Unsubscribe(UnsubscribeCommand {
                filters: unsafe { filters(values, count) }?,
                protocol: unsafe { unsubscribe_protocol_options(options) }?,
            }))
        },
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_token_deferred(
    client: *mut rumqttc_client,
    token: *const rumqttc_ack_token,
    options: *const rumqttc_acknowledgement_options_t,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    create(
        client,
        out,
        error_out,
        || {
            unsafe { ack_token::token_ref(token) }?;
            let mut bytes = 0;
            if !options.is_null() {
                let options = unsafe { record(options) }?;
                if !options.v5_options.is_null() {
                    let p = unsafe { record(options.v5_options) }?;
                    bytes = unsafe { properties_charge(p.user_properties, p.user_property_count) }?;
                    if boolean(p.reason_string_present, "reason_string_present")? {
                        bytes = add(bytes, unsafe { text_charge(p.reason_string) }?)?;
                    }
                }
            }
            bytes
                .checked_mul(2)
                .ok_or_else(|| ErrorHandle::argument("ACK size overflow"))
        },
        || {
            let protocol = unsafe { parse_acknowledgement_options(options) }?;
            let token = unsafe { ack_token::token_ref(token) }?
                .lock()
                .map_err(|_| ErrorHandle::internal("token acknowledgement lock is poisoned"))?
                .as_ref()
                .copied()
                .ok_or_else(|| ErrorHandle::state("token has no available acknowledgement"))?;
            Ok(Command::AcknowledgeWithOptions { token, protocol })
        },
    )
}
