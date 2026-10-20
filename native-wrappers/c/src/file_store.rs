#![cfg_attr(
    not(feature = "file-session-store"),
    allow(unused_variables, unused_imports)
)]
//! Optional file-store ABI. Owned operations keep mutation and observation separate.
use super::{
    boundary, core_error, destroy_box, error_ref, rumqttc_config, rumqttc_error,
    rumqttc_string_view_t, struct_size, write_optional,
};
use crate::error::ErrorHandle;
#[cfg(feature = "file-session-store")]
use rumqttc_wrapper_core::{
    FileStore, FileStoreContext, FileStoreContextOptions, FileStoreError, FileStoreOperation,
    FileStoreOptions, FileStoreResult, FileStoreValue, ProtocolVersion,
};
use std::{ffi::c_void, ptr, time::Duration};

pub struct rumqttc_file_store_context {
    #[cfg(feature = "file-session-store")]
    inner: FileStoreContext,
}
pub struct rumqttc_file_store {
    #[cfg(feature = "file-session-store")]
    inner: FileStore,
}
pub struct rumqttc_file_store_operation {
    #[cfg(feature = "file-session-store")]
    inner: Operation,
}
#[cfg(feature = "file-session-store")]
#[derive(Clone)]
enum Operation {
    Open(FileStoreOperation<FileStore>),
    Admin(FileStoreOperation<FileStoreResult>),
}
#[repr(C)]
pub struct rumqttc_file_store_context_options_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub worker_threads: usize,
    pub store_capacity: usize,
    pub operation_capacity: usize,
    pub observation_capacity: usize,
    pub byte_capacity: usize,
}
#[repr(C)]
pub struct rumqttc_file_store_options_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub max_checkpoint_size: usize,
    pub max_concurrent_operations: usize,
    pub max_report_entries: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_path_view_t {
    pub data: *const c_void,
    pub len: usize,
    pub encoding: u32,
    pub reserved: u32,
}
#[repr(C)]
pub struct rumqttc_file_store_error_details_t {
    pub struct_size: u32,
    pub kind: u32,
    pub operation: u32,
    pub io_kind: u32,
    pub os_code: i32,
    pub os_code_present: u8,
    pub reserved: [u8; 3],
}
#[repr(C)]
pub struct rumqttc_file_store_result_t {
    pub struct_size: u32,
    pub kind: u32,
    pub present: u8,
    pub size_present: u8,
    pub modified_present: u8,
    pub reserved: u8,
    pub modified_nanos: u32,
    pub size: u64,
    pub modified_seconds: i64,
    pub removed_count: usize,
    pub skipped_count: usize,
    pub failure_count: usize,
}

fn status_error(status: u32, message: impl Into<String>) -> ErrorHandle {
    ErrorHandle::plain(
        status,
        match status {
            crate::error::PERSISTENCE_ERROR => 8,
            crate::error::CONFIG_ERROR => 1,
            crate::error::BACKPRESSURE => 3,
            crate::error::INVALID_STATE => 2,
            crate::error::TIMEOUT => 9,
            _ => 0,
        },
        message,
    )
}
fn enabled() -> Result<(), ErrorHandle> {
    if cfg!(feature = "file-session-store") {
        Ok(())
    } else {
        Err(status_error(
            crate::error::CONFIG_ERROR,
            "file session stores require file-session-store",
        ))
    }
}
#[cfg(feature = "file-session-store")]
fn file_error(e: FileStoreError) -> ErrorHandle {
    use rumqttc_wrapper_core::FileStoreFailure as F;
    let status = match e.kind {
        F::Invalid => crate::error::INVALID_ARGUMENT,
        F::Resources => crate::error::BACKPRESSURE,
        F::InUse | F::Closed => crate::error::INVALID_STATE,
        F::Unsupported => crate::error::CONFIG_ERROR,
        _ => crate::error::PERSISTENCE_ERROR,
    };
    status_error(status, e.to_string()).with_file_failure(crate::error::FileFailureDetails {
        kind: e.kind as u32,
        operation: e.operation,
        io_kind: e.io_kind,
        os_code: e.os_code,
    })
}
unsafe fn record<T>(out: *mut T) -> Result<(), ErrorHandle> {
    if out.is_null() {
        return Err(ErrorHandle::argument("file store record is NULL"));
    }
    let size = unsafe { out.cast::<u32>().read() } as usize;
    unsafe { ptr::write_bytes(out.cast::<u8>(), 0, size.min(size_of::<T>())) };
    if size != size_of::<T>() {
        return Err(ErrorHandle::argument("file store record size is invalid"));
    }
    unsafe { out.cast::<u32>().write(struct_size::<T>()) };
    Ok(())
}
#[cfg(feature = "file-session-store")]
unsafe fn path(view: rumqttc_path_view_t) -> Result<std::path::PathBuf, ErrorHandle> {
    if view.reserved != 0
        || view.len == 0
        || view.data.is_null()
        || view.len > isize::MAX as usize / 2
    {
        return Err(ErrorHandle::argument("invalid file store path"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        if view.encoding != 1 {
            return Err(ErrorHandle::argument("Unix paths require native bytes"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(view.data.cast::<u8>(), view.len) };
        if bytes.contains(&0) {
            return Err(ErrorHandle::argument("path contains NUL"));
        }
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        if view.encoding != 2 {
            return Err(ErrorHandle::argument(
                "Windows paths require UTF-16 code units",
            ));
        }
        let units: Vec<_> = (0..view.len)
            .map(|i| unsafe { view.data.cast::<u16>().add(i).read_unaligned() })
            .collect();
        if units.contains(&0) {
            return Err(ErrorHandle::argument("path contains NUL"));
        }
        Ok(std::ffi::OsString::from_wide(&units).into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(status_error(
            crate::error::CONFIG_ERROR,
            "file store paths are unsupported",
        ))
    }
}
#[cfg(feature = "file-session-store")]
unsafe fn context<'a>(
    h: *const rumqttc_file_store_context,
) -> Result<&'a FileStoreContext, ErrorHandle> {
    unsafe { h.as_ref() }
        .map(|v| &v.inner)
        .ok_or_else(|| ErrorHandle::argument("file store context is NULL"))
}
#[cfg(feature = "file-session-store")]
unsafe fn store<'a>(h: *const rumqttc_file_store) -> Result<&'a FileStore, ErrorHandle> {
    unsafe { h.as_ref() }
        .map(|v| &v.inner)
        .ok_or_else(|| ErrorHandle::argument("file store is NULL"))
}
#[cfg(feature = "file-session-store")]
unsafe fn operation<'a>(
    h: *const rumqttc_file_store_operation,
) -> Result<&'a Operation, ErrorHandle> {
    unsafe { h.as_ref() }
        .map(|v| &v.inner)
        .ok_or_else(|| ErrorHandle::argument("file store operation is NULL"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_context_new(
    options: *const rumqttc_file_store_context_options_t,
    out: *mut *mut rumqttc_file_store_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("file store context output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let options = if options.is_null() {
                FileStoreContextOptions::default()
            } else {
                if unsafe { options.cast::<u32>().read() }
                    != struct_size::<rumqttc_file_store_context_options_t>()
                {
                    return Err(ErrorHandle::argument("invalid context options size"));
                }
                let o = unsafe { &*options };
                if o.reserved != 0 {
                    return Err(ErrorHandle::argument("reserved context option is nonzero"));
                }
                FileStoreContextOptions {
                    worker_threads: o.worker_threads,
                    store_capacity: o.store_capacity,
                    operation_capacity: o.operation_capacity,
                    observation_capacity: o.observation_capacity,
                    byte_capacity: o.byte_capacity,
                }
            };
            let inner = FileStoreContext::new(options).map_err(|e| core_error(&e, None))?;
            unsafe { *out = Box::into_raw(Box::new(rumqttc_file_store_context { inner })) };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_context_retain(
    handle: *const rumqttc_file_store_context,
    out: *mut *mut rumqttc_file_store_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("file store retain output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let inner = unsafe { context(handle) }?.clone();
            unsafe { *out = Box::into_raw(Box::new(rumqttc_file_store_context { inner })) };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_context_release(
    handle: *mut rumqttc_file_store_context,
) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_retain(
    handle: *const rumqttc_file_store,
    out: *mut *mut rumqttc_file_store,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("file store retain output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let inner = unsafe { store(handle) }?.clone();
            unsafe { *out = Box::into_raw(Box::new(rumqttc_file_store { inner })) };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_release(handle: *mut rumqttc_file_store) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_retain(
    handle: *const rumqttc_file_store_operation,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("file store retain output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let inner = unsafe { operation(handle) }?.clone();
            unsafe { *out = Box::into_raw(Box::new(rumqttc_file_store_operation { inner })) };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_release(
    handle: *mut rumqttc_file_store_operation,
) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_begin_open(
    handle: *const rumqttc_file_store_context,
    protocol: u32,
    root: rumqttc_path_view_t,
    options: *const rumqttc_file_store_options_t,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("file store open output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let protocol = match protocol {
                1 => ProtocolVersion::V4,
                2 => ProtocolVersion::V5,
                _ => return Err(ErrorHandle::argument("invalid store protocol")),
            };
            let options = if options.is_null() {
                FileStoreOptions::default()
            } else {
                if unsafe { options.cast::<u32>().read() }
                    != struct_size::<rumqttc_file_store_options_t>()
                {
                    return Err(ErrorHandle::argument("invalid store options size"));
                }
                let o = unsafe { &*options };
                if o.reserved != 0 {
                    return Err(ErrorHandle::argument("reserved store option is nonzero"));
                }
                FileStoreOptions {
                    max_checkpoint_size: o.max_checkpoint_size,
                    max_concurrent_operations: o.max_concurrent_operations,
                    max_report_entries: o.max_report_entries,
                }
            };
            let op = unsafe { context(handle) }?
                .open(protocol, unsafe { path(root) }?, options)
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Open(op),
                }))
            };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_file_session_store(
    config: *mut rumqttc_config,
    handle: *const rumqttc_file_store,
    scope: rumqttc_string_view_t,
    timeout_ms: u64,
    max_checkpoint_size: usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        #[cfg(feature = "file-session-store")]
        {
            let store = unsafe { store(handle) }?;
            let scope = unsafe { super::string_from_view(scope) }?;
            let config_store = store
                .configuration(
                    scope,
                    Duration::from_millis(timeout_ms),
                    max_checkpoint_size,
                )
                .map_err(|e| core_error(&e, None))?;
            unsafe { super::config_ref(config) }?.update_with_error(
                |c| {
                    match &mut c.protocol {
                        rumqttc_wrapper_core::ProtocolConfig::V4(v)
                            if store.protocol() == ProtocolVersion::V4 =>
                        {
                            v.session_store = Some(config_store)
                        }
                        rumqttc_wrapper_core::ProtocolConfig::V5(v)
                            if store.protocol() == ProtocolVersion::V5 =>
                        {
                            v.session_store = Some(config_store)
                        }
                        _ => {
                            return Err(ErrorHandle::argument(
                                "store protocol does not match configuration",
                            ));
                        }
                    };
                    Ok(())
                },
                || ErrorHandle::internal("configuration lock is poisoned"),
            )?;
        }
        Ok(())
    })
}
#[cfg(feature = "file-session-store")]
fn observe(op: &Operation, timeout: Option<Duration>) -> Result<Option<()>, ErrorHandle> {
    macro_rules! observe {
        ($value:expr) => {{
            let result = match timeout {
                Some(t) => $value.wait(t).map_err(|e| core_error(&e, None))?,
                None => $value.poll(),
            };
            match result {
                None => Ok(None),
                Some(Ok(_)) => Ok(Some(())),
                Some(Err(e)) => Err(file_error(e)),
            }
        }};
    }
    match op {
        Operation::Open(op) => observe!(op),
        Operation::Admin(op) => observe!(op),
    }
}
fn observe_ffi(
    handle: *const rumqttc_file_store_operation,
    timeout: Option<Duration>,
    state_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(state_out, 0) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if state_out.is_null() {
            return Err(ErrorHandle::argument("operation state output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            match observe(unsafe { operation(handle) }?, timeout) {
                Ok(Some(())) => unsafe { *state_out = 2 },
                Ok(None) => {
                    unsafe { *state_out = 1 };
                    return Err(status_error(
                        if timeout.is_some() {
                            crate::error::TIMEOUT
                        } else {
                            crate::error::WOULD_BLOCK
                        },
                        "file store operation is pending",
                    ));
                }
                Err(e) => {
                    if e.failure_details().file.is_some() {
                        unsafe { *state_out = 3 };
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_poll(
    handle: *const rumqttc_file_store_operation,
    state_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe_ffi(handle, None, state_out, error_out)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_wait_timeout_ms(
    handle: *const rumqttc_file_store_operation,
    timeout_ms: u64,
    state_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    observe_ffi(
        handle,
        Some(Duration::from_millis(timeout_ms)),
        state_out,
        error_out,
    )
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_get_store(
    handle: *const rumqttc_file_store_operation,
    out: *mut *mut rumqttc_file_store,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("store output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let Operation::Open(op) = unsafe { operation(handle) }? else {
                return Err(ErrorHandle::argument("operation is not an open"));
            };
            let value = op
                .poll()
                .ok_or_else(|| ErrorHandle::would_block("store open is pending"))?
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store {
                    inner: (*value).clone(),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_inspect(
    handle: *const rumqttc_file_store,
    scope: rumqttc_string_view_t,
    client_id: rumqttc_string_view_t,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let scope = unsafe { super::string_from_view(scope) }?;
            let id = unsafe { super::string_from_view(client_id) }?;
            let op = unsafe { store(handle) }?
                .inspect(scope, id)
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Admin(op),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_quarantine(
    handle: *const rumqttc_file_store,
    scope: rumqttc_string_view_t,
    client_id: rumqttc_string_view_t,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let scope = unsafe { super::string_from_view(scope) }?;
            let id = unsafe { super::string_from_view(client_id) }?;
            let op = unsafe { store(handle) }?
                .quarantine(scope, id)
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Admin(op),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_clear(
    handle: *const rumqttc_file_store,
    scope: rumqttc_string_view_t,
    client_id: rumqttc_string_view_t,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let scope = unsafe { super::string_from_view(scope) }?;
            let id = unsafe { super::string_from_view(client_id) }?;
            let op = unsafe { store(handle) }?
                .clear(scope, id)
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Admin(op),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_close(
    handle: *const rumqttc_file_store,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let op = unsafe { store(handle) }?.close().map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Admin(op),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_cleanup(
    handle: *const rumqttc_file_store,
    minimum_age_ms: u64,
    out: *mut *mut rumqttc_file_store_operation,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let op = unsafe { store(handle) }?
                .cleanup(Duration::from_millis(minimum_age_ms))
                .map_err(file_error)?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_file_store_operation {
                    inner: Operation::Admin(op),
                }))
            };
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_context_request_shutdown(
    handle: *const rumqttc_file_store_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        #[cfg(feature = "file-session-store")]
        unsafe { context(handle) }?
            .request_shutdown()
            .map_err(file_error)?;
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_context_join_timeout_ms(
    handle: *const rumqttc_file_store_context,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        #[cfg(feature = "file-session-store")]
        if !unsafe { context(handle) }?
            .join(Duration::from_millis(timeout_ms))
            .map_err(|e| core_error(&e, None))?
        {
            return Err(status_error(
                crate::error::TIMEOUT,
                "file store context teardown is pending",
            ));
        }
        Ok(())
    })
}
#[cfg(feature = "file-session-store")]
fn timestamp(time: std::time::SystemTime) -> Option<(i64, u32)> {
    let (seconds, nanos) = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(value) => (i128::from(value.as_secs()), value.subsec_nanos()),
        Err(e) => {
            let value = e.duration();
            let seconds = -i128::from(value.as_secs());
            if value.subsec_nanos() == 0 {
                (seconds, 0)
            } else {
                (seconds - 1, 1_000_000_000 - value.subsec_nanos())
            }
        }
    };
    Some((seconds.try_into().ok()?, nanos))
}
#[cfg(feature = "file-session-store")]
unsafe fn result(
    handle: *const rumqttc_file_store_operation,
) -> Result<FileStoreValue<FileStoreResult>, ErrorHandle> {
    let Operation::Admin(op) = unsafe { operation(handle) }? else {
        return Err(ErrorHandle::argument(
            "operation has no administration result",
        ));
    };
    op.poll()
        .ok_or_else(|| ErrorHandle::would_block("file store operation is pending"))?
        .map_err(file_error)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_result(
    handle: *const rumqttc_file_store_operation,
    out: *mut rumqttc_file_store_result_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { record(out) }?;
        enabled()?;
        #[cfg(feature = "file-session-store")]
        {
            let value = unsafe { result(handle) }?;
            let out = unsafe { &mut *out };
            match &*value {
                FileStoreResult::Inspection {
                    present,
                    size,
                    modified,
                } => {
                    out.kind = 1;
                    out.present = u8::from(*present);
                    if let Some(size) = size {
                        out.size_present = 1;
                        out.size = *size;
                    }
                    if let Some((seconds, nanos)) = modified.and_then(timestamp) {
                        out.modified_present = 1;
                        out.modified_seconds = seconds;
                        out.modified_nanos = nanos;
                    }
                }
                FileStoreResult::Quarantined { .. } => out.kind = 2,
                FileStoreResult::Cleared => out.kind = 3,
                FileStoreResult::Cleanup {
                    removed,
                    skipped,
                    failures,
                } => {
                    out.kind = 4;
                    out.removed_count = removed.len();
                    out.skipped_count = skipped.len();
                    out.failure_count = failures.len();
                }
                FileStoreResult::Closed => out.kind = 5,
            }
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_path_copy(
    handle: *const rumqttc_file_store_operation,
    buffer: *mut u8,
    capacity: usize,
    length_out: *mut usize,
    encoding_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(length_out, 0);
        write_optional(encoding_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if length_out.is_null() || encoding_out.is_null() {
            return Err(ErrorHandle::argument(
                "path length or encoding output is NULL",
            ));
        }
        #[cfg(feature = "file-session-store")]
        {
            let value = unsafe { result(handle) }?;
            let FileStoreResult::Quarantined { path, .. } = &*value else {
                return Err(ErrorHandle::argument("operation has no quarantine path"));
            };
            #[cfg(unix)]
            let (bytes, encoding) = {
                use std::os::unix::ffi::OsStrExt;
                (path.as_os_str().as_bytes().to_vec(), 1)
            };
            #[cfg(windows)]
            let (bytes, encoding) = {
                use std::os::windows::ffi::OsStrExt;
                (
                    path.as_os_str()
                        .encode_wide()
                        .flat_map(u16::to_ne_bytes)
                        .collect::<Vec<_>>(),
                    2,
                )
            };
            #[cfg(not(any(unix, windows)))]
            let (bytes, encoding) = (Vec::<u8>::new(), 0);
            unsafe {
                *length_out = bytes.len();
                *encoding_out = encoding;
            }
            if buffer.is_null() && capacity == 0 {
                return Ok(());
            }
            if buffer.is_null() || capacity < bytes.len() {
                return Err(ErrorHandle::argument("path buffer is too small"));
            }
            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_entry(
    handle: *const rumqttc_file_store_operation,
    category: u32,
    index: usize,
    text_out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(
            text_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    };
    boundary(error_out, ptr::null_mut(), || {
        enabled()?;
        if text_out.is_null() {
            return Err(ErrorHandle::argument("entry output is NULL"));
        }
        #[cfg(feature = "file-session-store")]
        {
            let value = unsafe { result(handle) }?;
            let text = match &*value {
                FileStoreResult::Quarantined { identifier, .. } if category == 0 && index == 0 => {
                    Some(identifier.as_str())
                }
                FileStoreResult::Cleanup {
                    removed,
                    skipped,
                    failures,
                } => match category {
                    1 => removed.get(index).map(String::as_str),
                    2 => skipped.get(index).map(String::as_str),
                    3 => failures.get(index).map(|v| v.0.as_str()),
                    _ => None,
                },
                _ => None,
            }
            .ok_or_else(|| ErrorHandle::argument("invalid result entry"))?;
            unsafe { *text_out = super::view_string(text) };
        }
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_file_store_details(
    handle: *const rumqttc_error,
    out: *mut rumqttc_file_store_error_details_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        unsafe { record(out) }?;
        if let Some(detail) = unsafe { error_ref(handle) }?.failure_details().file {
            let out = unsafe { &mut *out };
            out.kind = detail.kind;
            out.operation = detail.operation;
            out.io_kind = detail.io_kind;
            if let Some(code) = detail.os_code {
                out.os_code_present = 1;
                out.os_code = code;
            }
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_file_store_operation_cleanup_failure(
    handle: *const rumqttc_file_store_operation,
    index: usize,
    out: *mut rumqttc_file_store_error_details_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { record(out) }?;
        enabled()?;
        #[cfg(feature = "file-session-store")]
        {
            let value = unsafe { result(handle) }?;
            let FileStoreResult::Cleanup { failures, .. } = &*value else {
                return Err(ErrorHandle::argument("operation is not cleanup"));
            };
            let (_, detail) = failures
                .get(index)
                .ok_or_else(|| ErrorHandle::argument("cleanup failure index is invalid"))?;
            let out = unsafe { &mut *out };
            out.kind = detail.kind as u32;
            out.operation = detail.operation;
            out.io_kind = detail.io_kind;
            if let Some(code) = detail.os_code {
                out.os_code_present = 1;
                out.os_code = code;
            }
        }
        Ok(())
    })
}
