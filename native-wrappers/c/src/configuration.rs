//! Owned runtime-update builders and observation handles. Existing C records stay unchanged.

use std::sync::Mutex;
use std::time::Duration;
use std::{net::SocketAddr, ptr};

use rumqttc_wrapper_core::{
    BrokerCredentials, ConfigurationSnapshot, ConfigurationUpdateReceipt, FieldUpdate,
    MAX_CONFIGURATION_UPDATE_BYTES, NetworkConfig, RuntimeConfigUpdate, SecretBytes,
};

use super::{
    boolean, boundary, bytes_from_view, client_ref, completion_ref, core_error, destroy_box,
    observe_completion, rumqttc_bytes_view_t, rumqttc_client, rumqttc_completion, rumqttc_error,
    rumqttc_string_view_t, rumqttc_tls_profile, string_from_view, struct_size, write_admission,
};
use crate::error::ErrorHandle;

pub struct rumqttc_runtime_update {
    update: Mutex<RuntimeConfigUpdate>,
}
pub struct rumqttc_configuration_receipt {
    receipt: ConfigurationUpdateReceipt,
}
pub struct rumqttc_configuration_snapshot {
    snapshot: ConfigurationSnapshot,
    local_addresses: [String; 2],
}

#[repr(C)]
pub struct rumqttc_runtime_network_options_t {
    pub struct_size: u32,
    pub present_fields: u32,
    pub send_buffer_size: u32,
    pub receive_buffer_size: u32,
    pub tcp_nodelay: u8,
    pub mptcp: u8,
    pub reserved: [u8; 6],
    pub local_address: rumqttc_string_view_t,
    pub bind_device: rumqttc_string_view_t,
    pub reserved_tail: [u64; 2],
}

#[repr(C)]
pub struct rumqttc_configuration_receipt_status_t {
    pub struct_size: u32,
    pub tuning_state: u32,
    pub connection_state: u32,
    pub reserved: u32,
    pub revision: u64,
}

#[repr(C)]
pub struct rumqttc_configuration_status_t {
    pub struct_size: u32,
    pub flags: u32,
    pub revision: u64,
    pub desired_tuning_revision: u64,
    pub effective_tuning_revision: u64,
    pub desired_connection_revision: u64,
    pub effective_connection_revision: u64,
    pub attempt: u64,
    pub attempt_revision: u64,
    pub successful_connection_revision: u64,
    pub route: u32,
    pub attempt_route: u32,
    pub attempt_outcome: u32,
    pub reserved: u32,
    pub effective_read_batch_size: u64,
    pub effective_age_ns: u64,
    pub observation_age_ns: u64,
    pub snapshot_age_ns: u64,
}

#[repr(C)]
pub struct rumqttc_runtime_tuning_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub max_request_batch: u64,
    pub read_batch_size: u64,
    pub pending_throttle_ns: u64,
}

#[repr(C)]
pub struct rumqttc_connection_profile_summary_t {
    pub struct_size: u32,
    pub flags: u32,
    pub tls_backend: u32,
    pub tls_pin_count: u32,
    pub connection_timeout_ms: u64,
}

fn bounded_view(len: usize) -> Result<(), ErrorHandle> {
    if len > MAX_CONFIGURATION_UPDATE_BYTES {
        return Err(ErrorHandle::argument(
            "configuration input exceeds the update byte limit",
        ));
    }
    Ok(())
}

unsafe fn update_ref<'a>(
    update: *mut rumqttc_runtime_update,
) -> Result<&'a rumqttc_runtime_update, ErrorHandle> {
    unsafe { update.as_ref() }.ok_or_else(|| ErrorHandle::argument("runtime update is NULL"))
}

fn mutate(
    update: *mut rumqttc_runtime_update,
    change: impl FnOnce(&mut RuntimeConfigUpdate),
) -> Result<(), ErrorHandle> {
    let update = unsafe { update_ref(update) }?;
    let mut locked = update
        .update
        .lock()
        .map_err(|_| ErrorHandle::internal("runtime update lock is poisoned"))?;
    let mut candidate = locked.clone();
    change(&mut candidate);
    candidate
        .validate_retained_size()
        .map_err(|error| core_error(&error, None))?;
    let old = std::mem::replace(&mut *locked, candidate);
    drop(locked);
    drop(old);
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_new(
    out: *mut *mut rumqttc_runtime_update,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("runtime update output is NULL"));
        }
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_runtime_update {
                update: Mutex::default(),
            }));
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_destroy(update: *mut rumqttc_runtime_update) {
    unsafe { destroy_box(update) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_max_request_batch(
    update: *mut rumqttc_runtime_update,
    count: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        mutate(update, |update| {
            update.max_request_batch = FieldUpdate::Replace(count as usize);
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_read_batch_size(
    update: *mut rumqttc_runtime_update,
    count: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        mutate(update, |update| {
            update.read_batch_size = FieldUpdate::Replace(count as usize);
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_pending_throttle_ns(
    update: *mut rumqttc_runtime_update,
    nanoseconds: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        mutate(update, |update| {
            update.pending_throttle = FieldUpdate::Replace(Duration::from_nanos(nanoseconds));
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_connection_timeout_ms(
    update: *mut rumqttc_runtime_update,
    milliseconds: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        mutate(update, |update| {
            update.connection_timeout = FieldUpdate::Replace(Duration::from_millis(milliseconds));
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_credentials(
    update: *mut rumqttc_runtime_update,
    username_present: u8,
    username: rumqttc_string_view_t,
    password_present: u8,
    password: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let username_present = boolean(username_present, "username_present")?;
        let password_present = boolean(password_present, "password_present")?;
        bounded_view(username.len)?;
        bounded_view(password.len)?;
        let username = if username_present {
            Some(unsafe { string_from_view(username) }?)
        } else {
            None
        };
        let password = if password_present {
            Some(SecretBytes::new(
                unsafe { bytes_from_view(password) }?.to_vec(),
            ))
        } else {
            None
        };
        mutate(update, |update| {
            update.credentials = FieldUpdate::Replace(BrokerCredentials { username, password });
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_broker_tls_profile(
    update: *mut rumqttc_runtime_update,
    profile: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let profile = unsafe { profile.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("TLS profile is NULL"))?;
        mutate(update, |update| {
            update.broker_tls = FieldUpdate::Replace(profile.config.clone());
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_set_network(
    update: *mut rumqttc_runtime_update,
    options: *const rumqttc_runtime_network_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if options.is_null()
            || unsafe { ptr::addr_of!((*options).struct_size).read() }
                < struct_size::<rumqttc_runtime_network_options_t>()
        {
            return Err(ErrorHandle::argument(
                "network options are NULL or too small",
            ));
        }
        let options = unsafe { &*options };
        if options.present_fields & !15 != 0
            || options.reserved != [0; 6]
            || options.reserved_tail != [0; 2]
        {
            return Err(ErrorHandle::argument(
                "invalid network flags or reserved fields",
            ));
        }
        bounded_view(options.local_address.len)?;
        bounded_view(options.bind_device.len)?;
        let local_address = if options.present_fields & 4 != 0 {
            Some(
                unsafe { string_from_view(options.local_address) }?
                    .parse::<SocketAddr>()
                    .map_err(|_| {
                        ErrorHandle::argument("local address must be a numeric socket address")
                    })?,
            )
        } else {
            None
        };
        let network = NetworkConfig {
            tcp_send_buffer_size: (options.present_fields & 1 != 0)
                .then_some(options.send_buffer_size),
            tcp_receive_buffer_size: (options.present_fields & 2 != 0)
                .then_some(options.receive_buffer_size),
            tcp_nodelay: boolean(options.tcp_nodelay, "tcp_nodelay")?,
            mptcp: boolean(options.mptcp, "mptcp")?,
            local_address,
            bind_device: if options.present_fields & 8 != 0 {
                Some(unsafe { string_from_view(options.bind_device) }?)
            } else {
                None
            },
        };
        mutate(update, |update| {
            update.network = FieldUpdate::Replace(network);
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_runtime_update_field_action(
    update: *mut rumqttc_runtime_update,
    field: u32,
    action: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if !(1..=7).contains(&field) || !matches!(action, 0 | 2) {
            return Err(ErrorHandle::argument(
                "unknown field or action; use a setter for replacement",
            ));
        }
        mutate(update, |update| {
            macro_rules! assign {
                ($value:expr) => {
                    $value = if action == 0 {
                        FieldUpdate::Unchanged
                    } else {
                        FieldUpdate::Clear
                    }
                };
            }
            match field {
                1 => assign!(update.max_request_batch),
                2 => assign!(update.read_batch_size),
                3 => assign!(update.pending_throttle),
                4 => assign!(update.credentials),
                5 => assign!(update.broker_tls),
                6 => assign!(update.network),
                7 => assign!(update.connection_timeout),
                _ => unreachable!(),
            }
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_update_configuration_tracked(
    client: *mut rumqttc_client,
    update: *const rumqttc_runtime_update,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if completion_out.is_null() {
            return Err(ErrorHandle::argument("completion output is NULL"));
        }
        let update = unsafe { update.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("runtime update is NULL"))?;
        let value = update
            .update
            .lock()
            .map_err(|_| ErrorHandle::internal("runtime update lock is poisoned"))?
            .clone();
        let client = unsafe { client_ref(client) }?;
        let admission = client
            .handle
            .try_configuration_update(value)
            .map_err(|error| core_error(&error, None))?;
        write_admission(admission, ptr::null_mut(), completion_out);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_configuration_receipt(
    completion: *const rumqttc_completion,
    out: *mut *mut rumqttc_configuration_receipt,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("receipt output is NULL"));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let value = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::would_block("update has not staged"))?;
        let rumqttc_wrapper_core::Completion::ConfigurationStaged(receipt) = value else {
            return Err(ErrorHandle::state(
                "completion is not a configuration update",
            ));
        };
        unsafe { *out = Box::into_raw(Box::new(rumqttc_configuration_receipt { receipt })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_receipt_destroy(
    receipt: *mut rumqttc_configuration_receipt,
) {
    unsafe { destroy_box(receipt) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_receipt_status(
    receipt: *const rumqttc_configuration_receipt,
    out: *mut rumqttc_configuration_receipt_status_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let size = unsafe { initialize_receipt_status(out) }?;
        let receipt =
            unsafe { receipt.as_ref() }.ok_or_else(|| ErrorHandle::argument("receipt is NULL"))?;
        let (tuning, connection) = receipt.receipt.activation();
        unsafe {
            *out = rumqttc_configuration_receipt_status_t {
                struct_size: size,
                tuning_state: tuning as u32,
                connection_state: connection as u32,
                revision: receipt.receipt.revision,
                reserved: 0,
            }
        };
        Ok(())
    })
}

unsafe fn initialize_receipt_status(
    out: *mut rumqttc_configuration_receipt_status_t,
) -> Result<u32, ErrorHandle> {
    if out.is_null() {
        return Err(ErrorHandle::argument("receipt status output is NULL"));
    }
    let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
    if size < struct_size::<rumqttc_configuration_receipt_status_t>() {
        return Err(ErrorHandle::argument("receipt status output is too small"));
    }
    unsafe {
        *out = rumqttc_configuration_receipt_status_t {
            struct_size: size,
            tuning_state: 0,
            connection_state: 0,
            reserved: 0,
            revision: 0,
        }
    };
    Ok(size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_configuration_snapshot(
    client: *mut rumqttc_client,
    out: *mut *mut rumqttc_configuration_snapshot,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "configuration snapshot output is NULL",
            ));
        }
        let client = unsafe { client_ref(client) }?;
        let snapshot = client
            .handle
            .configuration_snapshot()
            .map_err(|error| core_error(&error, None))?;
        let local_addresses =
            [&snapshot.desired_connection, &snapshot.effective_connection].map(|profile| {
                profile
                    .network
                    .local_address
                    .map_or_else(String::new, |address| address.to_string())
            });
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_configuration_snapshot {
                snapshot,
                local_addresses,
            }));
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_snapshot_destroy(
    snapshot: *mut rumqttc_configuration_snapshot,
) {
    unsafe { destroy_box(snapshot) };
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_snapshot_status(
    snapshot: *const rumqttc_configuration_snapshot,
    out: *mut rumqttc_configuration_status_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("configuration status output is NULL"));
        }
        let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
        if size < struct_size::<rumqttc_configuration_status_t>() {
            return Err(ErrorHandle::argument(
                "configuration status output is too small",
            ));
        }
        // All fields are integers, so zero is a valid initialized failure output.
        unsafe {
            ptr::write_bytes(out, 0, 1);
            (*out).struct_size = size;
        }
        let value = &unsafe { snapshot.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("configuration snapshot is NULL"))?
            .snapshot;
        unsafe {
            *out = rumqttc_configuration_status_t {
                struct_size: size,
                flags: u32::from(value.closed)
                    | (u32::from(value.connection.attempt_revision.is_some()) << 1)
                    | (u32::from(value.connection.successful_revision.is_some()) << 2),
                revision: value.revision,
                desired_tuning_revision: value.desired_tuning_revision,
                effective_tuning_revision: value.effective_tuning_revision,
                desired_connection_revision: value.desired_connection_revision,
                effective_connection_revision: value.effective_connection_revision,
                attempt: value.connection.attempt,
                attempt_revision: value.connection.attempt_revision.unwrap_or(0),
                successful_connection_revision: value.connection.successful_revision.unwrap_or(0),
                route: value.connection.route as u32,
                attempt_route: value.connection.attempt_route as u32,
                attempt_outcome: value.connection.outcome as u32,
                reserved: 0,
                effective_read_batch_size: value.effective_read_batch_size as u64,
                effective_age_ns: nanos(value.effective_captured_at.elapsed()),
                observation_age_ns: nanos(value.connection.captured_at.elapsed()),
                snapshot_age_ns: nanos(value.captured_at.elapsed()),
            }
        };
        Ok(())
    })
}

fn selection(value: u32) -> Result<usize, ErrorHandle> {
    match value {
        0 => Ok(0),
        1 => Ok(1),
        _ => Err(ErrorHandle::argument(
            "selection must be desired (0) or effective (1)",
        )),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_snapshot_tuning(
    snapshot: *const rumqttc_configuration_snapshot,
    selected: u32,
    out: *mut rumqttc_runtime_tuning_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("tuning output is NULL"));
        }
        let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
        if size < struct_size::<rumqttc_runtime_tuning_t>() {
            return Err(ErrorHandle::argument("tuning output is too small"));
        }
        unsafe {
            *out = rumqttc_runtime_tuning_t {
                struct_size: size,
                reserved: 0,
                max_request_batch: 0,
                read_batch_size: 0,
                pending_throttle_ns: 0,
            }
        };
        let selected = selection(selected)?;
        let value = &unsafe { snapshot.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("configuration snapshot is NULL"))?
            .snapshot;
        let tuning = [value.desired_tuning, value.effective_tuning][selected];
        unsafe {
            (*out).max_request_batch = tuning.max_request_batch as u64;
            (*out).read_batch_size = tuning.read_batch_size as u64;
            (*out).pending_throttle_ns = nanos(tuning.pending_throttle);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_snapshot_profile(
    snapshot: *const rumqttc_configuration_snapshot,
    selected: u32,
    out: *mut rumqttc_connection_profile_summary_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("profile output is NULL"));
        }
        let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
        if size < struct_size::<rumqttc_connection_profile_summary_t>() {
            return Err(ErrorHandle::argument("profile output is too small"));
        }
        unsafe {
            *out = rumqttc_connection_profile_summary_t {
                struct_size: size,
                flags: 0,
                tls_backend: 0,
                tls_pin_count: 0,
                connection_timeout_ms: 0,
            }
        };
        let selected = selection(selected)?;
        let value = &unsafe { snapshot.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("configuration snapshot is NULL"))?
            .snapshot;
        let profile = [&value.desired_connection, &value.effective_connection][selected];
        unsafe {
            (*out).flags = u32::from(profile.username_present)
                | (u32::from(profile.password_present) << 1)
                | (u32::from(profile.tls_backend.is_some()) << 2)
                | (u32::from(profile.tls_identity_present) << 3);
            (*out).tls_backend =
                u32::from(profile.tls_backend == Some(rumqttc_wrapper_core::TlsBackend::Native));
            (*out).tls_pin_count = u32::try_from(profile.tls_pin_count).unwrap_or(u32::MAX);
            (*out).connection_timeout_ms =
                u64::try_from(profile.connection_timeout.as_millis()).unwrap_or(u64::MAX);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_configuration_snapshot_network(
    snapshot: *const rumqttc_configuration_snapshot,
    selected: u32,
    out: *mut rumqttc_runtime_network_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("network output is NULL"));
        }
        let size = unsafe { ptr::addr_of!((*out).struct_size).read() };
        if size < struct_size::<rumqttc_runtime_network_options_t>() {
            return Err(ErrorHandle::argument("network output is too small"));
        }
        unsafe {
            *out = rumqttc_runtime_network_options_t {
                struct_size: size,
                present_fields: 0,
                send_buffer_size: 0,
                receive_buffer_size: 0,
                tcp_nodelay: 0,
                mptcp: 0,
                reserved: [0; 6],
                local_address: rumqttc_string_view_t {
                    data: ptr::null(),
                    len: 0,
                },
                bind_device: rumqttc_string_view_t {
                    data: ptr::null(),
                    len: 0,
                },
                reserved_tail: [0; 2],
            }
        };
        let selected = selection(selected)?;
        let value = unsafe { snapshot.as_ref() }
            .ok_or_else(|| ErrorHandle::argument("configuration snapshot is NULL"))?;
        let network = &[
            &value.snapshot.desired_connection,
            &value.snapshot.effective_connection,
        ][selected]
            .network;
        let view = |text: &str| rumqttc_string_view_t {
            data: text.as_ptr().cast(),
            len: text.len(),
        };
        unsafe {
            (*out).present_fields = u32::from(network.tcp_send_buffer_size.is_some())
                | (u32::from(network.tcp_receive_buffer_size.is_some()) << 1)
                | (u32::from(network.local_address.is_some()) << 2)
                | (u32::from(network.bind_device.is_some()) << 3);
            (*out).send_buffer_size = network.tcp_send_buffer_size.unwrap_or(0);
            (*out).receive_buffer_size = network.tcp_receive_buffer_size.unwrap_or(0);
            (*out).tcp_nodelay = u8::from(network.tcp_nodelay);
            (*out).mptcp = u8::from(network.mptcp);
            if network.local_address.is_some() {
                (*out).local_address = view(&value.local_addresses[selected]);
            }
            if let Some(device) = &network.bind_device {
                (*out).bind_device = view(device);
            }
        }
        Ok(())
    })
}
