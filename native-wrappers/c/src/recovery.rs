use super::{
    admit, boundary, completion_ref, error_detail, rumqttc_client, rumqttc_completion,
    rumqttc_error, struct_size, write_admission, write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::Command;
use std::ptr;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_recover_session(
    client: *mut rumqttc_client,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(operation_id_out, 0);
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() {
            return Err(ErrorHandle::argument("operation ID output is NULL"));
        }
        write_admission(
            admit(client, Command::RecoverSession)?,
            operation_id_out,
            ptr::null_mut(),
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_recover_session_tracked(
    client: *mut rumqttc_client,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(completion_out, ptr::null_mut());
    }
    boundary(error_out, client, || {
        if completion_out.is_null() {
            return Err(ErrorHandle::argument("completion output is NULL"));
        }
        write_admission(
            admit(client, Command::RecoverSession)?,
            ptr::null_mut(),
            completion_out,
        );
        Ok(())
    })
}

#[repr(C)]
pub struct rumqttc_session_recovery_snapshot_t {
    pub struct_size: u32,
    pub phase: u32,
    pub failure_phase: u32,
    pub abandonment_committed: u8,
    pub checkpoint_cleared: u8,
    pub fresh_established: u8,
    pub session_present_known: u8,
    pub raw_session_present: u8,
    pub reserved: [u8; 7],
}

/// Progress is readable while pending and after operation failure or client destruction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_session_recovery_snapshot(
    completion: *const rumqttc_completion,
    out: *mut rumqttc_session_recovery_snapshot_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("recovery snapshot output is NULL"));
        }
        let declared_size = unsafe { (*out).struct_size };
        unsafe {
            ptr::write_bytes(
                out.cast::<u8>().add(size_of::<u32>()),
                0,
                (declared_size as usize)
                    .min(size_of::<rumqttc_session_recovery_snapshot_t>())
                    .saturating_sub(size_of::<u32>()),
            );
        }
        if declared_size < struct_size::<rumqttc_session_recovery_snapshot_t>() {
            return Err(ErrorHandle::argument(
                "recovery snapshot struct is too small",
            ));
        }
        let snapshot = unsafe { completion_ref(completion) }?
            .recovery_snapshot()
            .ok_or_else(|| ErrorHandle::state("completion is not session recovery"))?;
        unsafe {
            (*out).phase = snapshot.phase as u32;
            (*out).failure_phase = snapshot.failure_phase.map_or(0, |phase| phase as u32);
            (*out).abandonment_committed = u8::from(snapshot.abandonment_committed);
            (*out).checkpoint_cleared = u8::from(snapshot.checkpoint_cleared);
            (*out).fresh_established = u8::from(snapshot.fresh_established);
            (*out).session_present_known = u8::from(snapshot.raw_session_present.is_some());
            (*out).raw_session_present = u8::from(snapshot.raw_session_present.unwrap_or(false));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_session_recovery_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error
            .failure_details()
            .recovery
            .map(std::num::NonZeroU32::get)
    })
}
