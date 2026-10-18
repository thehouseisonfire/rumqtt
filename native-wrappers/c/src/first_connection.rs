//! Independent observation of the historical first accepted connection.
use super::{
    boundary, client_ref_for_shutdown, core_error, destroy_box, rumqttc_client, rumqttc_error,
    write_optional,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{
    ConnectionHandle, ConnectionWaitOutcome, Error, ErrorKind, ProtocolVersion,
};
use std::{ptr, time::Duration};

pub struct rumqttc_first_connection {
    inner: ConnectionHandle,
}

pub(super) unsafe fn first_connection_ref<'a>(
    handle: *const rumqttc_first_connection,
) -> Result<&'a ConnectionHandle, ErrorHandle> {
    if handle.is_null() {
        return Err(ErrorHandle::argument("first-connection observer is NULL"));
    }
    Ok(&unsafe { &*handle }.inner)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_first_connection(
    client: *const rumqttc_client,
    out: *mut *mut rumqttc_first_connection,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, client.cast_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("first-connection output is NULL"));
        }
        let inner = unsafe { client_ref_for_shutdown(client.cast_mut()) }?
            .handle
            .connection();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_first_connection { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_first_connection_retain(
    handle: *const rumqttc_first_connection,
    out: *mut *mut rumqttc_first_connection,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("first-connection output is NULL"));
        }
        let inner = unsafe { first_connection_ref(handle) }?.clone();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_first_connection { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_first_connection_release(handle: *mut rumqttc_first_connection) {
    unsafe { destroy_box(handle) };
}

fn observe(
    handle: *const rumqttc_first_connection,
    timeout: Option<Duration>,
    state_out: *mut u32,
    protocol_out: *mut u32,
    session_present_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(state_out, 0);
        write_optional(protocol_out, 0);
        write_optional(session_present_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if state_out.is_null() {
            return Err(ErrorHandle::argument(
                "first-connection state output is NULL",
            ));
        }
        let handle = unsafe { first_connection_ref(handle) }?;
        let outcome = if let Some(timeout) = timeout {
            handle.wait_timeout_outcome(timeout)
        } else {
            let Some(result) = handle.try_wait() else {
                unsafe { *state_out = 1 };
                return Err(ErrorHandle::would_block(
                    "first connection is still pending",
                ));
            };
            ConnectionWaitOutcome::Completed(result)
        };
        match outcome {
            ConnectionWaitOutcome::Completed(Ok(result)) => {
                unsafe {
                    *state_out = 2;
                    write_optional(
                        protocol_out,
                        match result.protocol {
                            ProtocolVersion::V4 => 1,
                            ProtocolVersion::V5 => 2,
                        },
                    );
                    write_optional(session_present_out, u8::from(result.session_present));
                }
                Ok(())
            }
            ConnectionWaitOutcome::Completed(Err(error)) => {
                unsafe { *state_out = 3 };
                Err(core_error(&error, None))
            }
            ConnectionWaitOutcome::DeadlineElapsed => {
                unsafe { *state_out = 1 };
                Err(core_error(
                    &Error::new(ErrorKind::Timeout, "first-connection wait expired"),
                    None,
                ))
            }
            ConnectionWaitOutcome::ObservationRejected(error) => Err(core_error(&error, None)),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_first_connection_poll(
    handle: *const rumqttc_first_connection,
    state_out: *mut u32,
    protocol_out: *mut u32,
    session_present_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe(
        handle,
        None,
        state_out,
        protocol_out,
        session_present_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_first_connection_wait_timeout_ms(
    handle: *const rumqttc_first_connection,
    timeout_ms: u64,
    state_out: *mut u32,
    protocol_out: *mut u32,
    session_present_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe(
        handle,
        Some(Duration::from_millis(timeout_ms)),
        state_out,
        protocol_out,
        session_present_out,
        error_out,
    )
}

#[cfg(test)]
#[path = "first_connection/tests.rs"]
mod tests;
