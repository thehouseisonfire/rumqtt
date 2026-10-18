use std::ptr;

use rumqttc_wrapper_core::NativeFailureDetails;

use super::{boundary, error_ref, rumqttc_error, struct_size};
use crate::error::ErrorHandle;

/// Fixed-width owned scalar projection; fields selects independently present values.
#[repr(C)]
#[derive(Default)]
pub struct rumqttc_native_failure_details_t {
    pub struct_size: u32,
    pub present: u32,
    pub domain: u32,
    pub code: u32,
    pub cause_domain: u32,
    pub cause_code: u32,
    pub fields: u32,
    pub io_kind: u32,
    pub os_domain: u32,
    pub os_error: i32,
    pub packet_kind: u32,
    pub packet_id: u32,
    pub http_status: u32,
    pub quantity: u32,
    pub actual: u64,
    pub maximum: u64,
    pub expected: u64,
}

impl rumqttc_native_failure_details_t {
    fn fill(&mut self, value: &NativeFailureDetails) {
        self.present = 1;
        self.domain = value.domain.code();
        self.code = value.code.code();
        if let Some((domain, code)) = value.cause {
            self.fields |= 1;
            self.cause_domain = domain.code();
            self.cause_code = code.code();
        }
        if let Some(kind) = value.io_kind {
            self.fields |= 2;
            self.io_kind = kind.code();
        }
        if let Some((domain, code)) = value.os_error {
            self.fields |= 4;
            self.os_domain = domain.code();
            self.os_error = code;
        }
        if let Some(kind) = value.packet_kind {
            self.fields |= 8;
            self.packet_kind = u32::from(kind);
        }
        if let Some(id) = value.packet_id {
            self.fields |= 16;
            self.packet_id = u32::from(id);
        }
        if let Some(status) = value.http_status {
            self.fields |= 32;
            self.http_status = u32::from(status);
        }
        if let Some(quantity) = value.quantity {
            self.fields |= 64;
            self.quantity = quantity.code();
        }
        for (value, flag, out) in [
            (value.actual, 128, &mut self.actual),
            (value.maximum, 256, &mut self.maximum),
            (value.expected, 512, &mut self.expected),
        ] {
            if let Some(value) = value {
                self.fields |= flag;
                *out = value;
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_native_failure_details(
    error: *const rumqttc_error,
    out: *mut rumqttc_native_failure_details_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("native failure output is NULL"));
        }
        // Read only the size prefix before accepting a full record.
        let size = unsafe { out.cast::<u32>().read() };
        if size < struct_size::<rumqttc_native_failure_details_t>() {
            return Err(ErrorHandle::argument("native failure output is too small"));
        }
        // Initialize known fields even for an invalid error. Preserve future bytes.
        unsafe {
            out.write(rumqttc_native_failure_details_t {
                struct_size: size,
                ..Default::default()
            });
        }
        let error = unsafe { error_ref(error) }?;
        if let Some(value) = &error.failure_details().native {
            unsafe { (*out).fill(value) };
        }
        Ok(())
    })
}

#[cfg(test)]
#[path = "native_failure/tests.rs"]
mod tests;
