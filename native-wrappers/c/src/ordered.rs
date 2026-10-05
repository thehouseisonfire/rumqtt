use std::ptr;
use std::time::Duration;

use super::{
    admit, boundary, client_ref, completion_ref, core_error, error_detail,
    parse_disconnect_options, rumqttc_client, rumqttc_completion, rumqttc_disconnect_options_t,
    rumqttc_error, struct_size, write_admission, write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{Command, Completion};

fn require_feature() -> Result<(), ErrorHandle> {
    if cfg!(feature = "ordered-shutdown") {
        Ok(())
    } else {
        Err(ErrorHandle::plain(
            crate::error::CONFIG_ERROR,
            1,
            "ordered-shutdown feature is disabled",
        ))
    }
}

fn ordered_admit(
    client: *mut rumqttc_client,
    timeout: Option<Duration>,
    options: *const rumqttc_disconnect_options_t,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(operation_id_out, 0);
        write_optional(completion_out, ptr::null_mut());
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() && completion_out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        require_feature()?;
        let protocol = unsafe { parse_disconnect_options(options) }?;
        write_admission(
            admit(
                client,
                Command::OrderedDisconnectWithOptions { timeout, protocol },
            )?,
            operation_id_out,
            completion_out,
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_disconnect_after_queued(
    client: *mut rumqttc_client,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        None,
        ptr::null(),
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_disconnect_after_queued_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        Some(Duration::from_millis(timeout_ms)),
        ptr::null(),
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_disconnect_after_queued_with_options(
    client: *mut rumqttc_client,
    options: *const rumqttc_disconnect_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        None,
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_disconnect_after_queued_with_options_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    options: *const rumqttc_disconnect_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        Some(Duration::from_millis(timeout_ms)),
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_disconnect_after_queued_tracked(
    client: *mut rumqttc_client,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        None,
        ptr::null(),
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_disconnect_after_queued_timeout_ms_tracked(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        Some(Duration::from_millis(timeout_ms)),
        ptr::null(),
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_disconnect_after_queued_with_options_tracked(
    client: *mut rumqttc_client,
    options: *const rumqttc_disconnect_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        None,
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_disconnect_after_queued_with_options_timeout_ms_tracked(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    options: *const rumqttc_disconnect_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    ordered_admit(
        client,
        Some(Duration::from_millis(timeout_ms)),
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_after_queued_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        require_feature()?;
        let protocol = unsafe { parse_disconnect_options(ptr::null()) }?;
        unsafe { client_ref(client) }?
            .close_after_queued(Duration::from_millis(timeout_ms), protocol)
            .map(|_| ())
            .map_err(|error| core_error(&error, None))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_after_queued_with_options_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    options: *const rumqttc_disconnect_options_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        require_feature()?;
        let protocol = unsafe { parse_disconnect_options(options) }?;
        unsafe { client_ref(client) }?
            .close_after_queued(Duration::from_millis(timeout_ms), protocol)
            .map(|_| ())
            .map_err(|error| core_error(&error, None))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_ordered_disconnect_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error
            .failure_details()
            .ordered
            .map(std::num::NonZeroU32::get)
    })
}

/// Cached observation; no Rust Instant or internal queue ownership crosses the ABI.
#[repr(C)]
pub struct rumqttc_ordered_shutdown_diagnostics_t {
    pub struct_size: u32,
    pub phase: u32,
    pub present: u8,
    pub fence_present: u8,
    pub deadline_present: u8,
    pub local_count_present: u8,
    pub reserved: u32,
    pub fence_sequence: u64,
    pub remaining_at_capture_ms: u64,
    pub local_queued_publishes: u64,
    pub snapshot_age_ms: u64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_ordered_shutdown_diagnostics(
    completion: *const rumqttc_completion,
    out: *mut rumqttc_ordered_shutdown_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("ordered diagnostics output is NULL"));
        }
        let declared_size = unsafe { (*out).struct_size };
        unsafe {
            ptr::write_bytes(
                out.cast::<u8>().add(size_of::<u32>()),
                0,
                (declared_size as usize)
                    .min(size_of::<rumqttc_ordered_shutdown_diagnostics_t>())
                    .saturating_sub(size_of::<u32>()),
            );
        }
        if declared_size < struct_size::<rumqttc_ordered_shutdown_diagnostics_t>() {
            return Err(ErrorHandle::argument(
                "ordered diagnostics struct is too small",
            ));
        }
        require_feature()?;
        let outcome = unsafe { completion_ref(completion) }?
            .outcome()
            .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))?;
        let Completion::Diagnostics(snapshot) = outcome
            .result()
            .as_ref()
            .map_err(|error| core_error(error, None))?
        else {
            return Err(ErrorHandle::state("completion is not diagnostics"));
        };
        if let Some(snapshot) = &snapshot.ordered_shutdown {
            unsafe {
                (*out).present = 1;
                (*out).phase = snapshot.phase as u32;
                (*out).fence_present = u8::from(snapshot.fence_sequence.is_some());
                (*out).fence_sequence = snapshot.fence_sequence.unwrap_or(0);
                (*out).deadline_present = u8::from(snapshot.remaining_at_capture.is_some());
                (*out).remaining_at_capture_ms =
                    snapshot.remaining_at_capture.map_or(0, |duration| {
                        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                    });
                (*out).local_count_present = u8::from(snapshot.local_queued_publishes.is_some());
                (*out).local_queued_publishes = snapshot
                    .local_queued_publishes
                    .map_or(0, |count| u64::try_from(count).unwrap_or(u64::MAX));
                (*out).snapshot_age_ms =
                    u64::try_from(snapshot.captured_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            }
        }
        Ok(())
    })
}
