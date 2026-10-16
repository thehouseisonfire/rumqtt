//! Detached manual-ACK owners. Core remains authoritative across retransmission events.
use super::{
    AcknowledgementSource, acknowledge_impl, boundary, destroy_box, event_ref,
    rumqttc_acknowledgement_options_t, rumqttc_client, rumqttc_completion, rumqttc_error,
    rumqttc_event,
};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::AckToken;
use std::ptr;
use std::sync::{Arc, Mutex};

#[cfg(test)]
#[path = "ack_token/tests.rs"]
mod tests;

pub struct rumqttc_ack_token {
    // Only detached handles share this availability slot. Tokens from separate retransmission
    // events still share core authority through the copied AckToken identity.
    inner: Arc<Mutex<Option<AckToken>>>,
}

pub(super) unsafe fn token_ref<'a>(
    token: *const rumqttc_ack_token,
) -> Result<&'a Mutex<Option<AckToken>>, ErrorHandle> {
    if token.is_null() {
        return Err(ErrorHandle::argument("ACK token handle is NULL"));
    }
    // SAFETY: Non-null handles must be library-owned and live throughout the call.
    Ok(&unsafe { &*token }.inner)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_take_ack_token(
    event: *mut rumqttc_event,
    out: *mut *mut rumqttc_ack_token,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("ACK token output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let mut slot = event
            .ack
            .lock()
            .map_err(|_| ErrorHandle::internal("event acknowledgement lock is poisoned"))?;
        let token = slot
            .as_ref()
            .copied()
            .ok_or_else(|| ErrorHandle::state("event has no available acknowledgement"))?;
        let handle = Box::new(rumqttc_ack_token {
            inner: Arc::new(Mutex::new(Some(token))),
        });
        *slot = None;
        drop(slot);
        unsafe { *out = Box::into_raw(handle) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_ack_token_retain(
    token: *const rumqttc_ack_token,
    out: *mut *mut rumqttc_ack_token,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("ACK token output is NULL"));
        }
        // Validate without locking: consumed and stale handles can still be retained/released.
        unsafe { token_ref(token) }?;
        let inner = Arc::clone(&unsafe { &*token }.inner);
        unsafe { *out = Box::into_raw(Box::new(rumqttc_ack_token { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_ack_token_release(token: *mut rumqttc_ack_token) {
    unsafe { destroy_box(token) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_acknowledge_token(
    client: *mut rumqttc_client,
    token: *mut rumqttc_ack_token,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        AcknowledgementSource::Token(token),
        ptr::null(),
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_token_tracked(
    client: *mut rumqttc_client,
    token: *mut rumqttc_ack_token,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        AcknowledgementSource::Token(token),
        ptr::null(),
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_acknowledge_token_with_options(
    client: *mut rumqttc_client,
    token: *mut rumqttc_ack_token,
    options: *const rumqttc_acknowledgement_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        AcknowledgementSource::Token(token),
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_token_with_options_tracked(
    client: *mut rumqttc_client,
    token: *mut rumqttc_ack_token,
    options: *const rumqttc_acknowledgement_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(
        client,
        AcknowledgementSource::Token(token),
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}
