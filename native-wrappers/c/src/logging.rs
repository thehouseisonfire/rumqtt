//! Additive logging placement, retained observers, and callback-borrowed records.
use super::{
    boundary, client_ref, config_ref, core_error, destroy_box, rumqttc_client, rumqttc_config,
    rumqttc_error, rumqttc_string_view_t, struct_size, view_string, write_optional,
};
use crate::error::{ErrorHandle, OK};
use rumqttc_wrapper_core::{
    LogFilter, LogLevel, LogRecord, LogSink, LogValue, LoggingContext, LoggingFlush,
    LoggingOptions, LoggingRegistration, LoggingStatistics,
};
use std::{ffi::c_void, ptr, time::Duration};

pub struct rumqttc_logging_context {
    inner: LoggingContext,
}
pub struct rumqttc_logging_registration {
    inner: LoggingRegistration,
}
pub struct rumqttc_logging_flush {
    inner: LoggingFlush,
}
pub struct rumqttc_log_record {
    inner: RecordStorage,
}
enum RecordStorage {
    Borrowed(*const LogRecord),
    Owned(LogRecord),
}
#[repr(C)]
pub struct rumqttc_logging_options_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub record_capacity: usize,
    pub byte_capacity: usize,
    pub registration_capacity: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_logging_vtable_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub record: Option<unsafe extern "C" fn(*mut c_void, *const rumqttc_log_record) -> u32>,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}
#[repr(C)]
pub struct rumqttc_logging_filter_t {
    pub struct_size: u32,
    pub level: u32,
    pub targets: *const rumqttc_string_view_t,
    pub target_count: usize,
}
#[repr(C)]
pub struct rumqttc_logging_statistics_t {
    pub struct_size: u32,
    pub failed: u32,
    pub accepted: u64,
    pub delivered: u64,
    pub overload: u64,
    pub rejected: u64,
    pub suppressed: u64,
    pub failures: u64,
    pub discarded: u64,
}
#[repr(C)]
pub struct rumqttc_log_record_info_t {
    pub struct_size: u32,
    pub level: u32,
    pub client_id: u64,
    pub name: rumqttc_string_view_t,
    pub target: rumqttc_string_view_t,
    pub message: rumqttc_string_view_t,
    pub field_count: usize,
    pub message_present: u8,
    pub truncated: u8,
    pub reserved: [u8; 6],
}
#[repr(C)]
pub struct rumqttc_log_field_t {
    pub struct_size: u32,
    pub kind: u32,
    pub name: rumqttc_string_view_t,
    pub text: rumqttc_string_view_t,
    pub signed_value: i64,
    pub unsigned_value: u64,
    pub float_value: f64,
    pub bool_value: u8,
    pub reserved: [u8; 7],
}
struct Sink {
    vtable: rumqttc_logging_vtable_t,
    data: usize,
}
// The pointer is opaque to Rust. The C contract requires thread-safe callbacks/destruction.
unsafe impl Send for Sink {}
unsafe impl Sync for Sink {}
impl LogSink for Sink {
    fn record(&self, record: &LogRecord) -> bool {
        let borrowed = rumqttc_log_record {
            inner: RecordStorage::Borrowed(ptr::from_ref(record)),
        };
        unsafe {
            self.vtable.record.expect("validated logging callback")(
                self.data as *mut c_void,
                &raw const borrowed,
            ) == OK
        }
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        unsafe {
            self.vtable.destroy.expect("validated logging destructor")(self.data as *mut c_void);
        };
    }
}
unsafe fn context<'a>(
    value: *const rumqttc_logging_context,
) -> Result<&'a LoggingContext, ErrorHandle> {
    if value.is_null() {
        return Err(ErrorHandle::argument("logging context is NULL"));
    }
    Ok(&unsafe { &*value }.inner)
}
unsafe fn registration<'a>(
    value: *const rumqttc_logging_registration,
) -> Result<&'a LoggingRegistration, ErrorHandle> {
    if value.is_null() {
        return Err(ErrorHandle::argument("logging registration is NULL"));
    }
    Ok(&unsafe { &*value }.inner)
}
unsafe fn record<'a>(value: *const rumqttc_log_record) -> Result<&'a LogRecord, ErrorHandle> {
    if value.is_null() {
        return Err(ErrorHandle::argument("log record is NULL"));
    }
    match &unsafe { &*value }.inner {
        RecordStorage::Owned(record) => Ok(record),
        // Borrowed adapters exist only for a synchronous callback's duration.
        RecordStorage::Borrowed(record) => Ok(unsafe { &**record }),
    }
}
fn enabled() -> Result<(), ErrorHandle> {
    if cfg!(feature = "tracing") {
        Ok(())
    } else {
        Err(core_error(
            &rumqttc_wrapper_core::Error::new(
                rumqttc_wrapper_core::ErrorKind::Configuration,
                "logging requires the tracing feature",
            ),
            None,
        ))
    }
}
unsafe fn initialize<T>(out: *mut T) -> Result<(), ErrorHandle> {
    if out.is_null() {
        return Err(ErrorHandle::argument("logging record output is NULL"));
    }
    let declared = unsafe { out.cast::<u32>().read() };
    // Initialize only the caller-declared prefix. Do not overwrite a future extension.
    unsafe {
        ptr::write_bytes(out.cast::<u8>(), 0, (declared as usize).min(size_of::<T>()));
    }
    if declared < struct_size::<T>() {
        return Err(ErrorHandle::argument("logging record output is too small"));
    }
    unsafe {
        out.cast::<u32>().write(struct_size::<T>());
    }
    Ok(())
}
fn log_level(level: u32) -> Result<LogLevel, ErrorHandle> {
    match level {
        0 => Ok(LogLevel::Off),
        1 => Ok(LogLevel::Error),
        2 => Ok(LogLevel::Warn),
        3 => Ok(LogLevel::Info),
        4 => Ok(LogLevel::Debug),
        5 => Ok(LogLevel::Trace),
        _ => Err(ErrorHandle::argument("invalid logging level")),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_new(
    options: *const rumqttc_logging_options_t,
    out: *mut *mut rumqttc_logging_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging context output is NULL"));
        }
        enabled()?;
        let options = if options.is_null() {
            LoggingOptions::default()
        } else {
            if unsafe { options.cast::<u32>().read() } < struct_size::<rumqttc_logging_options_t>()
            {
                return Err(ErrorHandle::argument("logging options are too small"));
            }
            let options = unsafe { &*options };
            if options.reserved != 0 {
                return Err(ErrorHandle::argument(
                    "logging options reserved field is nonzero",
                ));
            }
            LoggingOptions {
                record_capacity: options.record_capacity,
                byte_capacity: options.byte_capacity,
                registration_capacity: options.registration_capacity,
            }
        };
        let inner = LoggingContext::new(options).map_err(|e| core_error(&e, None))?;
        unsafe { *out = Box::into_raw(Box::new(rumqttc_logging_context { inner })) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_registration_new(
    context_ptr: *const rumqttc_logging_context,
    filter: *const rumqttc_logging_filter_t,
    vtable: *const rumqttc_logging_vtable_t,
    data: *mut c_void,
    out: *mut *mut rumqttc_logging_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() || vtable.is_null() {
            return Err(ErrorHandle::argument("logging vtable or output is NULL"));
        }
        enabled()?;
        let context = unsafe { context(context_ptr) }?;
        if unsafe { vtable.cast::<u32>().read() } < struct_size::<rumqttc_logging_vtable_t>() {
            return Err(ErrorHandle::argument("logging vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        if vtable.reserved != 0 || vtable.record.is_none() || vtable.destroy.is_none() {
            return Err(ErrorHandle::argument("invalid logging vtable"));
        }
        let filter = if filter.is_null() {
            LogFilter::default()
        } else {
            if unsafe { filter.cast::<u32>().read() } < struct_size::<rumqttc_logging_filter_t>() {
                return Err(ErrorHandle::argument("logging filter is too small"));
            }
            let filter = unsafe { &*filter };
            if filter.target_count > 16 || (filter.target_count != 0 && filter.targets.is_null()) {
                return Err(ErrorHandle::argument("invalid logging target list"));
            }
            let mut targets = Vec::with_capacity(filter.target_count);
            for index in 0..filter.target_count {
                let value = unsafe { filter.targets.add(index).read() };
                if value.len == 0 || value.len > 128 {
                    return Err(ErrorHandle::argument("invalid logging target length"));
                }
                targets.push(unsafe { super::string_from_view(value) }?);
            }
            LogFilter {
                level: log_level(filter.level)?,
                targets,
            }
        };
        let inner = context
            .register_with(filter, || {
                Box::new(Sink {
                    vtable,
                    data: data as usize,
                })
            })
            .map_err(|e| core_error(&e, None))?;
        unsafe { *out = Box::into_raw(Box::new(rumqttc_logging_registration { inner })) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_logging(
    config: *mut rumqttc_config,
    value: *const rumqttc_logging_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        let inner = unsafe { registration(value) }?.clone();
        inner
            .validate_attachment()
            .map_err(|e| core_error(&e, None))?;
        unsafe { config_ref(config) }?
            .set_logging(Some(inner))
            .map_err(ErrorHandle::internal)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_logging(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        unsafe { config_ref(config) }?
            .set_logging(None)
            .map_err(ErrorHandle::internal)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_set_logging(
    client: *mut rumqttc_client,
    value: *const rumqttc_logging_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        enabled()?;
        let inner = unsafe { registration(value) }?.clone();
        unsafe { client_ref(client) }?
            .handle
            .set_logging(Some(inner))
            .map_err(|e| core_error(&e, None))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_clear_logging(
    client: *mut rumqttc_client,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        enabled()?;
        unsafe { client_ref(client) }?
            .handle
            .set_logging(None)
            .map_err(|e| core_error(&e, None))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_request_shutdown(
    value: *const rumqttc_logging_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { context(value) }?.request_shutdown();
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_state(
    value: *const rumqttc_logging_context,
    out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, 0) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging state output is NULL"));
        }
        unsafe { *out = context(value)?.state() as u32 };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_join_timeout_ms(
    value: *const rumqttc_logging_context,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let complete = unsafe { context(value) }?
            .join(Duration::from_millis(timeout_ms))
            .map_err(|e| core_error(&e, None))?;
        logging_wait_result(complete, timeout_ms, "logging teardown is still pending")
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_flush(
    value: *const rumqttc_logging_context,
    out: *mut *mut rumqttc_logging_flush,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging flush output is NULL"));
        }
        let inner = unsafe { context(value) }?.flush();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_logging_flush { inner })) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_flush_wait_timeout_ms(
    value: *const rumqttc_logging_flush,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if value.is_null() {
            return Err(ErrorHandle::argument("logging flush is NULL"));
        }
        let complete = unsafe { &*value }
            .inner
            .wait(Duration::from_millis(timeout_ms))
            .map_err(|e| core_error(&e, None))?;
        logging_wait_result(complete, timeout_ms, "logging flush is still pending")
    })
}
fn logging_wait_result(
    complete: bool,
    timeout_ms: u64,
    message: &'static str,
) -> Result<(), ErrorHandle> {
    match (complete, timeout_ms) {
        (true, _) => Ok(()),
        (false, 0) => Err(ErrorHandle::would_block(message)),
        (false, _) => Err(core_error(
            &rumqttc_wrapper_core::Error::new(rumqttc_wrapper_core::ErrorKind::Timeout, message),
            None,
        )),
    }
}
unsafe fn statistics(
    out: *mut rumqttc_logging_statistics_t,
    stats: LoggingStatistics,
    failed: bool,
) {
    unsafe {
        *out = rumqttc_logging_statistics_t {
            struct_size: struct_size::<rumqttc_logging_statistics_t>(),
            failed: u32::from(failed),
            accepted: stats.accepted,
            delivered: stats.delivered,
            overload: stats.overload,
            rejected: stats.rejected,
            suppressed: stats.suppressed,
            failures: stats.failures,
            discarded: stats.discarded,
        }
    };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_statistics(
    value: *const rumqttc_logging_context,
    out: *mut rumqttc_logging_statistics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { initialize(out) }?;
        let stats = unsafe { context(value) }?.statistics();
        unsafe { statistics(out, stats, false) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_registration_statistics(
    value: *const rumqttc_logging_registration,
    out: *mut rumqttc_logging_statistics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { initialize(out) }?;
        let inner = unsafe { registration(value) }?;
        unsafe { statistics(out, inner.statistics(), inner.failed()) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_log_record_info(
    value: *const rumqttc_log_record,
    out: *mut rumqttc_log_record_info_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { initialize(out) }?;
        let record = unsafe { record(value) }?;
        unsafe {
            *out = rumqttc_log_record_info_t {
                struct_size: struct_size::<rumqttc_log_record_info_t>(),
                level: record.level as u32,
                client_id: record.client_id,
                name: view_string(&record.name),
                target: view_string(&record.target),
                message: view_string(record.message.as_deref().unwrap_or("")),
                field_count: record.fields.len(),
                message_present: u8::from(record.message.is_some()),
                truncated: u8::from(record.truncated),
                reserved: [0; 6],
            }
        };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_log_record_field(
    value: *const rumqttc_log_record,
    index: usize,
    out: *mut rumqttc_log_field_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { initialize(out) }?;
        let record = unsafe { record(value) }?;
        let field = record
            .fields
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("log field index is out of bounds"))?;
        let mut result = rumqttc_log_field_t {
            struct_size: struct_size::<rumqttc_log_field_t>(),
            kind: 0,
            name: view_string(&field.name),
            text: view_string(""),
            signed_value: 0,
            unsigned_value: 0,
            float_value: 0.0,
            bool_value: 0,
            reserved: [0; 7],
        };
        match &field.value {
            LogValue::Bool(v) => {
                result.kind = 1;
                result.bool_value = u8::from(*v);
            }
            LogValue::Signed(v) => {
                result.kind = 2;
                result.signed_value = *v;
            }
            LogValue::Unsigned(v) => {
                result.kind = 3;
                result.unsigned_value = *v;
            }
            LogValue::Float(v) => {
                result.kind = 4;
                result.float_value = *v;
            }
            LogValue::Text(v) => {
                result.kind = 5;
                result.text = view_string(v);
            }
        }
        unsafe { *out = result };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_log_record_copy(
    value: *const rumqttc_log_record,
    out: *mut *mut rumqttc_log_record,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("log record copy output is NULL"));
        }
        let inner = unsafe { record(value) }?.clone();
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_log_record {
                inner: RecordStorage::Owned(inner),
            }));
        };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_retain(
    value: *const rumqttc_logging_context,
    out: *mut *mut rumqttc_logging_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging retain output is NULL"));
        }
        let inner = unsafe { context(value) }?.clone();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_logging_context { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_registration_retain(
    value: *const rumqttc_logging_registration,
    out: *mut *mut rumqttc_logging_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging retain output is NULL"));
        }
        let inner = unsafe { registration(value) }?.clone();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_logging_registration { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_release(value: *mut rumqttc_logging_context) {
    unsafe { destroy_box(value) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_registration_release(
    value: *mut rumqttc_logging_registration,
) {
    unsafe { destroy_box(value) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_flush_release(value: *mut rumqttc_logging_flush) {
    unsafe { destroy_box(value) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_log_record_release(value: *mut rumqttc_log_record) {
    unsafe { destroy_box(value) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_context_try_join(
    value: *const rumqttc_logging_context,
    out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, 0) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("logging join output is NULL"));
        }
        let complete = unsafe { context(value) }?
            .join(Duration::ZERO)
            .map_err(|e| core_error(&e, None))?;
        unsafe { *out = u8::from(complete) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_logging_flush_poll(
    value: *const rumqttc_logging_flush,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { rumqttc_logging_flush_wait_timeout_ms(value, 0, error_out) }
}
