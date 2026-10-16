use rumqttc::*;
use std::{
    ffi::c_void,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
const OK: u32 = 0;
const CONFIG_ERROR: u32 = 3;
const TIMEOUT: u32 = 5;
const WOULD_BLOCK: u32 = 11;

fn assert_logging_wait_error(mut call: impl FnMut(*mut *mut rumqttc_error) -> u32, expected: u32) {
    let mut error = ptr::dangling_mut();
    assert_eq!(call(&raw mut error), expected);
    assert!(
        !error.is_null(),
        "non-OK observation must publish an owned error"
    );
    unsafe {
        let mut status = u32::MAX;
        let mut kind = u32::MAX;
        let mut retryable = 99;
        let mut ambiguous = 99;
        assert_eq!(rumqttc_error_status(error, &raw mut status), OK);
        assert_eq!(rumqttc_error_kind(error, &raw mut kind), OK);
        assert_eq!(
            rumqttc_error_flags(error, &raw mut retryable, &raw mut ambiguous),
            OK
        );
        assert_eq!(status, expected);
        assert_eq!(kind, if expected == TIMEOUT { 9 } else { 0 });
        assert_eq!(retryable, u8::from(expected == TIMEOUT));
        assert_eq!(ambiguous, 0);
        rumqttc_error_destroy(error);
    }
    assert_eq!(call(ptr::null_mut()), expected);
}
#[test]
fn disabled_logging_rejects_creation_and_initializes_outputs() {
    if rumqttc_library_capabilities() & (1 << 18) != 0 {
        return;
    }
    unsafe {
        let mut out = ptr::dangling_mut();
        let mut error = ptr::null_mut();
        assert_eq!(
            rumqttc_logging_context_new(ptr::null(), &raw mut out, &raw mut error),
            CONFIG_ERROR
        );
        assert!(out.is_null());
        assert!(!error.is_null());
        rumqttc_error_destroy(error);
        assert_eq!(
            rumqttc_config_set_logging(ptr::null_mut(), ptr::null(), ptr::null_mut()),
            CONFIG_ERROR
        );
    }
}
struct Owner {
    records: Mutex<Vec<usize>>,
    drops: AtomicU64,
}
unsafe extern "C" fn observe(data: *mut c_void, record: *const rumqttc_log_record) -> u32 {
    let owner = unsafe { &*data.cast::<Owner>() };
    let mut copy = ptr::null_mut();
    assert_eq!(
        unsafe { rumqttc_log_record_copy(record, &raw mut copy, ptr::null_mut()) },
        OK
    );
    owner.records.lock().unwrap().push(copy as usize);
    OK
}
unsafe extern "C" fn destroy(data: *mut c_void) {
    unsafe { &*data.cast::<Owner>() }
        .drops
        .fetch_add(1, Ordering::Relaxed);
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Verify the complete C registration and copied-record ownership lifecycle"
)]
fn retained_ffi_registration_and_record_copy_survive_client_and_logging_teardown() {
    if rumqttc_library_capabilities() & (1 << 18) == 0 {
        return;
    }
    unsafe {
        let owner = Box::new(Owner {
            records: Mutex::new(Vec::new()),
            drops: AtomicU64::new(0),
        });
        let data = (&raw const *owner).cast_mut().cast();
        let mut context = ptr::null_mut();
        let mut registration = ptr::null_mut();
        let mut config = ptr::null_mut();
        let mut client = ptr::null_mut();
        assert_eq!(
            rumqttc_logging_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
            OK
        );
        let mut vtable = rumqttc_logging_vtable_t {
            struct_size: u32::try_from(size_of::<rumqttc_logging_vtable_t>()).unwrap(),
            reserved: 1,
            record: Some(observe),
            destroy: Some(destroy),
        };
        assert_ne!(
            rumqttc_logging_registration_new(
                context,
                ptr::null(),
                &raw const vtable,
                data,
                &raw mut registration,
                ptr::null_mut()
            ),
            OK
        );
        assert!(registration.is_null());
        assert_eq!(owner.drops.load(Ordering::Relaxed), 0);
        vtable.reserved = 0;
        assert_eq!(
            rumqttc_logging_registration_new(
                context,
                ptr::null(),
                &raw const vtable,
                data,
                &raw mut registration,
                ptr::null_mut()
            ),
            OK
        );
        for protocol in [1, 2] {
            assert_eq!(
                rumqttc_config_new(protocol, &raw mut config, ptr::null_mut()),
                OK
            );
            let text = |s: &str| rumqttc_string_view_t {
                data: s.as_ptr().cast(),
                len: s.len(),
            };
            assert_eq!(
                rumqttc_config_set_broker(config, text("127.0.0.1"), 65535, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_set_client_id(config, text("private-client"), ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_config_set_logging(config, registration, ptr::null_mut()),
                OK
            );
            assert_eq!(
                rumqttc_client_start(config, &raw mut client, ptr::null_mut()),
                OK
            );
            rumqttc_config_destroy(config);
            assert_eq!(
                rumqttc_client_destroy_timeout_ms(client, 5000, ptr::null_mut()),
                OK
            );
        }
        let mut flush = ptr::null_mut();
        assert_eq!(
            rumqttc_logging_context_flush(context, &raw mut flush, ptr::null_mut()),
            OK
        );
        assert_eq!(
            rumqttc_logging_flush_wait_timeout_ms(flush, 5000, ptr::null_mut()),
            OK
        );
        assert_eq!(
            rumqttc_logging_context_request_shutdown(context, ptr::null_mut()),
            OK
        );
        // Keeping the registration guarantees pending teardown without timing assumptions.
        for (timeout_ms, status) in [(0, WOULD_BLOCK), (1, TIMEOUT)] {
            assert_logging_wait_error(
                |error| rumqttc_logging_context_join_timeout_ms(context, timeout_ms, error),
                status,
            );
        }
        let mut joined = 99;
        let mut error = ptr::dangling_mut();
        assert_eq!(
            rumqttc_logging_context_try_join(context, &raw mut joined, &raw mut error),
            OK
        );
        assert_eq!(joined, 0);
        assert!(error.is_null());
        rumqttc_logging_registration_release(registration);
        error = ptr::dangling_mut();
        assert_eq!(
            rumqttc_logging_context_join_timeout_ms(context, 5000, &raw mut error),
            OK
        );
        assert!(error.is_null());
        rumqttc_logging_flush_release(flush);
        rumqttc_logging_context_release(context);
        assert_eq!(owner.drops.load(Ordering::Relaxed), 1);
        let records = owner.records.lock().unwrap();
        assert_ne!(records.len(), 0);
        for &record in records.iter() {
            let record = record as *mut rumqttc_log_record;
            let mut info: rumqttc_log_record_info_t = std::mem::zeroed();
            info.struct_size = u32::try_from(size_of::<rumqttc_log_record_info_t>()).unwrap();
            assert_eq!(
                rumqttc_log_record_info(record, &raw mut info, ptr::null_mut()),
                OK
            );
            assert_ne!(info.client_id, 0);
            for index in 0..info.field_count {
                let mut field: rumqttc_log_field_t = std::mem::zeroed();
                field.struct_size = u32::try_from(size_of::<rumqttc_log_field_t>()).unwrap();
                assert_eq!(
                    rumqttc_log_record_field(record, index, &raw mut field, ptr::null_mut()),
                    OK
                );
                assert!((1..=5).contains(&field.kind));
            }
            rumqttc_log_record_release(record);
        }
    }
}
