//! Additive C execution-context API; configurations snapshot placement independently of protocol values.
use super::{boundary, config_ref, core_error, destroy_box, rumqttc_config, rumqttc_error};
use crate::error::ErrorHandle;
use rumqttc_wrapper_core::{ExecutionContext, ExecutionOptions};
use std::ptr;
use std::time::Duration;

pub struct rumqttc_execution_context {
    inner: ExecutionContext,
}

#[repr(C)]
pub struct rumqttc_execution_options_t {
    pub struct_size: u32,
    pub worker_threads: u32,
    pub max_blocking_threads: u32,
    pub reserved: u32,
    pub client_capacity: usize,
}

unsafe fn context_ref<'a>(
    context: *const rumqttc_execution_context,
) -> Result<&'a ExecutionContext, ErrorHandle> {
    if context.is_null() {
        return Err(ErrorHandle::argument("execution context is NULL"));
    }
    Ok(&unsafe { &*context }.inner)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_new(
    options: *const rumqttc_execution_options_t,
    out: *mut *mut rumqttc_execution_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe {
            *out = ptr::null_mut();
        }
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("execution context output is NULL"));
        }
        let options = if options.is_null() {
            ExecutionOptions::default()
        } else {
            if unsafe { options.cast::<u32>().read() } as usize
                != size_of::<rumqttc_execution_options_t>()
            {
                return Err(ErrorHandle::argument("execution options size is invalid"));
            }
            let options = unsafe { &*options };
            if options.reserved != 0 {
                return Err(ErrorHandle::argument(
                    "execution options reserved field is nonzero",
                ));
            }
            ExecutionOptions {
                client_capacity: options.client_capacity,
                worker_threads: options.worker_threads as usize,
                max_blocking_threads: options.max_blocking_threads as usize,
            }
        };
        let inner = ExecutionContext::new(options).map_err(|error| core_error(&error, None))?;
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_execution_context { inner }));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_retain(
    context: *const rumqttc_execution_context,
    out: *mut *mut rumqttc_execution_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe {
            *out = ptr::null_mut();
        }
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("execution context output is NULL"));
        }
        let inner = unsafe { context_ref(context) }?.clone();
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_execution_context { inner }));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_release(
    context: *mut rumqttc_execution_context,
) {
    unsafe {
        destroy_box(context);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_execution_context(
    config: *mut rumqttc_config,
    context: *const rumqttc_execution_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let context = unsafe { context_ref(context) }?.clone();
        unsafe { config_ref(config) }?
            .set_execution(Some(context))
            .map_err(ErrorHandle::internal)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_execution_context(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { config_ref(config) }?
            .set_execution(None)
            .map_err(ErrorHandle::internal)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_request_shutdown(
    context: *const rumqttc_execution_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { context_ref(context) }?.request_shutdown();
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_state(
    context: *const rumqttc_execution_context,
    out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe {
            *out = 0;
        }
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("execution state output is NULL"));
        }
        unsafe {
            *out = context_ref(context)?.state() as u32;
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_try_join(
    context: *const rumqttc_execution_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if unsafe { context_ref(context) }?
            .try_join()
            .map_err(|error| core_error(&error, None))?
        {
            Ok(())
        } else {
            Err(ErrorHandle::would_block("execution teardown is pending"))
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_execution_context_join_timeout_ms(
    context: *const rumqttc_execution_context,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        unsafe { context_ref(context) }?
            .join(Duration::from_millis(timeout_ms))
            .map_err(|error| core_error(&error, None))
    })
}
