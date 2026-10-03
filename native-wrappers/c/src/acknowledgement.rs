use std::ptr;
use std::sync::Arc;

use rumqttc_wrapper_core::{
    AcknowledgementKind, BrokerAcknowledgement, ProtocolVersion, TerminalOutcome,
};

use super::{
    boundary, completion_ref, rumqttc_completion, rumqttc_error, rumqttc_string_view_t,
    struct_size, view_string, write_optional,
};
use crate::error::ErrorHandle;

/// Metadata only; diagnostic strings and properties are accessed as completion-owned views.
#[repr(C)]
pub struct rumqttc_acknowledgement_details_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub packet_kind: u32,
    pub packet_id: u16,
    pub present: u8,
    pub reason_present: u8,
    pub reason: u8,
    pub properties_present: u8,
    pub recovered: u8,
    pub reserved: [u8; 5],
}

unsafe fn outcome(
    completion: *const rumqttc_completion,
) -> Result<Arc<TerminalOutcome>, ErrorHandle> {
    // The C completion still owns the same immutable outcome after this temporary Arc is dropped.
    unsafe { completion_ref(completion) }?
        .outcome()
        .ok_or_else(|| ErrorHandle::would_block("completion is not ready"))
}

fn acknowledgement(outcome: &TerminalOutcome) -> Result<&BrokerAcknowledgement, ErrorHandle> {
    outcome
        .acknowledgement()
        .ok_or_else(|| ErrorHandle::state("completion has no broker acknowledgement"))
}

const fn empty_string() -> rumqttc_string_view_t {
    rumqttc_string_view_t {
        data: ptr::null(),
        len: 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement(
    completion: *const rumqttc_completion,
    out: *mut rumqttc_acknowledgement_details_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("acknowledgement output is NULL"));
        }
        let declared_size = unsafe { (*out).struct_size };
        // Clear only the caller's declared extent, preserving struct_size and future extension bytes.
        unsafe {
            ptr::write_bytes(
                out.cast::<u8>().add(size_of::<u32>()),
                0,
                (declared_size as usize)
                    .min(size_of::<rumqttc_acknowledgement_details_t>())
                    .saturating_sub(size_of::<u32>()),
            );
        }
        if declared_size < struct_size::<rumqttc_acknowledgement_details_t>() {
            return Err(ErrorHandle::argument("acknowledgement struct is too small"));
        }
        let outcome = unsafe { outcome(completion) }?;
        if let Some(ack) = outcome.acknowledgement() {
            unsafe {
                (*out).present = 1;
                (*out).protocol = match ack.protocol() {
                    ProtocolVersion::V4 => 1,
                    ProtocolVersion::V5 => 2,
                };
                (*out).packet_kind = u32::from(ack.kind() as u8);
                (*out).packet_id = ack.packet_id();
                (*out).reason_present = u8::from(ack.reason_code().is_some());
                (*out).reason = ack.reason_code().unwrap_or(0);
                (*out).properties_present = u8::from(ack.properties().is_some());
                (*out).recovered = u8::from(ack.recovered());
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement_result_count(
    completion: *const rumqttc_completion,
    present_out: *mut u8,
    count_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(count_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && count_out.is_null() {
            return Err(ErrorHandle::argument("filter result output is NULL"));
        }
        let outcome = unsafe { outcome(completion) }?;
        let ack = acknowledgement(&outcome)?;
        if !matches!(
            ack.kind(),
            AcknowledgementKind::SubAck | AcknowledgementKind::UnsubAck
        ) {
            return Err(ErrorHandle::state(
                "acknowledgement has no per-filter results",
            ));
        }
        unsafe {
            write_optional(present_out, u8::from(ack.filter_reason_codes().is_some()));
            write_optional(count_out, ack.filter_reason_codes().map_or(0, <[u8]>::len));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement_result_at(
    completion: *const rumqttc_completion,
    index: usize,
    reason_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(reason_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if reason_out.is_null() {
            return Err(ErrorHandle::argument("filter reason output is NULL"));
        }
        let outcome = unsafe { outcome(completion) }?;
        let reasons = acknowledgement(&outcome)?
            .filter_reason_codes()
            .ok_or_else(|| ErrorHandle::state("acknowledgement has no per-filter results"))?;
        let reason = reasons
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("result index is out of bounds"))?;
        unsafe {
            *reason_out = *reason;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement_reason_string(
    completion: *const rumqttc_completion,
    present_out: *mut u8,
    value_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(value_out, empty_string());
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("reason string output is NULL"));
        }
        let outcome = unsafe { outcome(completion) }?;
        let reason = acknowledgement(&outcome)?
            .properties()
            .and_then(|p| p.reason_string.as_deref());
        unsafe {
            write_optional(present_out, u8::from(reason.is_some()));
            write_optional(value_out, reason.map_or_else(empty_string, view_string));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement_user_property_count(
    completion: *const rumqttc_completion,
    count_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(count_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if count_out.is_null() {
            return Err(ErrorHandle::argument("property count output is NULL"));
        }
        let outcome = unsafe { outcome(completion) }?;
        let count = acknowledgement(&outcome)?
            .properties()
            .map_or(0, |p| p.user_properties.len());
        unsafe {
            *count_out = count;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_acknowledgement_user_property_at(
    completion: *const rumqttc_completion,
    index: usize,
    name_out: *mut rumqttc_string_view_t,
    value_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(name_out, empty_string());
        write_optional(value_out, empty_string());
    }
    boundary(error_out, ptr::null_mut(), || {
        if name_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument("property output is NULL"));
        }
        let outcome = unsafe { outcome(completion) }?;
        let properties = acknowledgement(&outcome)?
            .properties()
            .map_or(&[][..], |p| p.user_properties.as_slice());
        let (name, value) = properties
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("property index is out of bounds"))?;
        unsafe {
            write_optional(name_out, view_string(name));
            write_optional(value_out, view_string(value));
        }
        Ok(())
    })
}
