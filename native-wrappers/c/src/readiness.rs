//! Additive, feature-discoverable C notification API. OS exports are wait-only.
use super::{
    boundary, destroy_box, rumqttc_client, rumqttc_completion, rumqttc_error,
    rumqttc_pending_admission, struct_size, write_optional,
};
use crate::error::ErrorHandle;
#[cfg(feature = "readiness")]
use rumqttc_wrapper_core::readiness::{NotificationContext, NotificationState, RegistrationKey};
use std::{ffi::c_void, ptr};

pub struct rumqttc_notification_context {
    #[cfg(feature = "readiness")]
    inner: NotificationContext,
}

#[repr(C)]
pub struct rumqttc_notification_options_t {
    pub struct_size: u32,
    pub reserved: u32,
    pub capacity: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct rumqttc_notification_key_t {
    pub slot: u64,
    pub generation: u64,
}

#[repr(C)]
pub struct rumqttc_notification_ready_t {
    pub struct_size: u32,
    pub source_kind: u32,
    pub key: rumqttc_notification_key_t,
    pub tag: u64,
    pub revision: u64,
    pub flags: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct rumqttc_notification_status_t {
    pub struct_size: u32,
    pub state: u32,
    pub signal_error: i32,
    pub reserved: u32,
    pub registrations: usize,
    pub capacity: usize,
}

fn unsupported() -> ErrorHandle {
    ErrorHandle::plain(
        crate::error::CONFIG_ERROR,
        1,
        "readiness feature or platform adapter is unavailable",
    )
}

#[cfg(feature = "readiness")]
unsafe fn context_ref<'a>(
    context: *const rumqttc_notification_context,
) -> Result<&'a NotificationContext, ErrorHandle> {
    if context.is_null() {
        return Err(ErrorHandle::argument("notification context is NULL"));
    }
    Ok(&unsafe { &*context }.inner)
}

#[cfg(feature = "readiness")]
const fn key(value: rumqttc_notification_key_t) -> RegistrationKey {
    RegistrationKey {
        slot: value.slot,
        generation: value.generation,
    }
}
#[cfg(feature = "readiness")]
const fn ffi_key(value: RegistrationKey) -> rumqttc_notification_key_t {
    rumqttc_notification_key_t {
        slot: value.slot,
        generation: value.generation,
    }
}
#[cfg(feature = "readiness")]
fn core(error: &rumqttc_wrapper_core::Error) -> ErrorHandle {
    super::core_error(error, None)
}

#[unsafe(no_mangle)]
pub const extern "C" fn rumqttc_notification_capabilities() -> u32 {
    if !cfg!(feature = "readiness") {
        0
    } else if cfg!(unix) {
        1
    } else if cfg!(windows) {
        2
    } else {
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_new(
    options: *const rumqttc_notification_options_t,
    out: *mut *mut rumqttc_notification_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("notification context output is NULL"));
        }
        let capacity = if options.is_null() {
            1024
        } else {
            if unsafe { options.cast::<u32>().read() }
                < struct_size::<rumqttc_notification_options_t>()
            {
                return Err(ErrorHandle::argument(
                    "notification options record is too small",
                ));
            }
            let options = unsafe { &*options };
            if options.reserved != 0 || options.capacity == 0 {
                return Err(ErrorHandle::argument("notification options are invalid"));
            }
            options.capacity
        };
        #[cfg(feature = "readiness")]
        {
            let inner = NotificationContext::new(capacity).map_err(|error| core(&error))?;
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_notification_context { inner }));
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = capacity;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_retain(
    context: *const rumqttc_notification_context,
    out: *mut *mut rumqttc_notification_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("notification context output is NULL"));
        }
        #[cfg(feature = "readiness")]
        {
            let inner = unsafe { context_ref(context) }?.clone();
            unsafe {
                *out = Box::into_raw(Box::new(rumqttc_notification_context { inner }));
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_release(
    context: *mut rumqttc_notification_context,
) {
    unsafe {
        destroy_box(context);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_register_events(
    context: *const rumqttc_notification_context,
    source: *const rumqttc_client,
    tag: u64,
    out: *mut rumqttc_notification_key_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, rumqttc_notification_key_t::default());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("registration key output is NULL"));
        }
        #[cfg(feature = "readiness")]
        {
            let context = unsafe { context_ref(context) }?;
            let registration = context
                .register_events(
                    &unsafe { super::client_ref_for_shutdown(source.cast_mut()) }?.event_readiness,
                    tag,
                )
                .map_err(|error| core(&error))?;
            unsafe {
                *out = ffi_key(registration);
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = (context, source, tag);
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_register_completion(
    context: *const rumqttc_notification_context,
    source: *const rumqttc_completion,
    tag: u64,
    out: *mut rumqttc_notification_key_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, rumqttc_notification_key_t::default());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("registration key output is NULL"));
        }
        #[cfg(feature = "readiness")]
        {
            let context = unsafe { context_ref(context) }?;
            let registration = context
                .register_completion(
                    unsafe { super::completion_ref(source) }?.notification_handle(),
                    tag,
                )
                .map_err(|error| core(&error))?;
            unsafe {
                *out = ffi_key(registration);
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = (context, source, tag);
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_register_pending_admission(
    context: *const rumqttc_notification_context,
    source: *const rumqttc_pending_admission,
    tag: u64,
    out: *mut rumqttc_notification_key_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, rumqttc_notification_key_t::default());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("registration key output is NULL"));
        }
        #[cfg(feature = "readiness")]
        {
            let context = unsafe { context_ref(context) }?;
            let registration = context
                .register_pending_admission(
                    unsafe { super::pending_admission::pending_ref(source) }?,
                    tag,
                )
                .map_err(|error| core(&error))?;
            unsafe {
                *out = ffi_key(registration);
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = (context, source, tag);
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_collect(
    context: *const rumqttc_notification_context,
    entries: *mut rumqttc_notification_ready_t,
    entry_size: usize,
    capacity: usize,
    count_out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(count_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if count_out.is_null()
            || entries.is_null()
            || capacity == 0
            || entry_size < size_of::<rumqttc_notification_ready_t>()
            || !entry_size.is_multiple_of(align_of::<rumqttc_notification_ready_t>())
            || capacity
                .checked_mul(entry_size)
                .is_none_or(|size| size > isize::MAX as usize)
        {
            return Err(ErrorHandle::argument(
                "notification entry buffer is invalid",
            ));
        }
        #[cfg(feature = "readiness")]
        {
            let mut written = 0;
            unsafe { context_ref(context) }?
                .collect(capacity, |entry| {
                    // SAFETY: The caller supplies a writable, aligned buffer of capacity entries.
                    let out = unsafe { entries.byte_add(written * entry_size) };
                    unsafe {
                        out.write(rumqttc_notification_ready_t {
                            struct_size: struct_size::<rumqttc_notification_ready_t>(),
                            source_kind: entry.kind as u32,
                            key: ffi_key(entry.key),
                            tag: entry.tag,
                            revision: entry.revision,
                            flags: u32::from(entry.invalidated),
                            reserved: 0,
                        });
                    }
                    written += 1;
                })
                .map_err(|error| core(&error))?;
            unsafe {
                *count_out = written;
            }
            if written == 0 {
                Err(ErrorHandle::would_block(
                    "no notification sources are ready",
                ))
            } else {
                Ok(())
            }
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_acknowledge(
    context: *const rumqttc_notification_context,
    registration: rumqttc_notification_key_t,
    revision: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        #[cfg(feature = "readiness")]
        {
            unsafe { context_ref(context) }?
                .acknowledge(key(registration), revision)
                .map_err(|error| core(&error))
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = (context, registration, revision);
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_deregister(
    context: *const rumqttc_notification_context,
    registration: rumqttc_notification_key_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        #[cfg(feature = "readiness")]
        {
            unsafe { context_ref(context) }?
                .deregister(key(registration))
                .map_err(|error| core(&error))
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = (context, registration);
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_prepare_wait(
    context: *const rumqttc_notification_context,
    service_again_out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(service_again_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if service_again_out.is_null() {
            return Err(ErrorHandle::argument("prepare-wait output is NULL"));
        }
        #[cfg(feature = "readiness")]
        {
            let ready = unsafe { context_ref(context) }?
                .prepare_wait()
                .map_err(|error| core(&error))?;
            unsafe {
                *service_again_out = u32::from(ready);
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_status(
    context: *const rumqttc_notification_context,
    out: *mut rumqttc_notification_status_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("notification status output is NULL"));
        }
        if unsafe { out.cast::<u32>().read() } < struct_size::<rumqttc_notification_status_t>() {
            return Err(ErrorHandle::argument(
                "notification status record is too small",
            ));
        }
        unsafe {
            out.write(rumqttc_notification_status_t {
                struct_size: struct_size::<rumqttc_notification_status_t>(),
                state: 0,
                signal_error: 0,
                reserved: 0,
                registrations: 0,
                capacity: 0,
            });
        }
        #[cfg(feature = "readiness")]
        {
            let context = unsafe { context_ref(context) }?;
            let status = context.status().map_err(|error| core(&error))?;
            unsafe {
                (*out).state = status.state as u32;
                (*out).signal_error = status.signal_error;
                (*out).registrations = status.registrations;
                (*out).capacity = status.capacity;
            }
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_request_close(
    context: *const rumqttc_notification_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        #[cfg(feature = "readiness")]
        {
            unsafe { context_ref(context) }?.request_close();
            Ok(())
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_context_try_join(
    context: *const rumqttc_notification_context,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        #[cfg(feature = "readiness")]
        {
            if unsafe { context_ref(context) }?.state() == NotificationState::Quiescent {
                Ok(())
            } else {
                Err(ErrorHandle::would_block(
                    "notification signal teardown is pending",
                ))
            }
        }
        #[cfg(not(feature = "readiness"))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_borrowed_posix_fd(
    context: *const rumqttc_notification_context,
    out: *mut i32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, -1);
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "notification resource output is NULL",
            ));
        }
        #[cfg(all(feature = "readiness", unix))]
        {
            use std::os::fd::AsRawFd;
            let context = unsafe { context_ref(context) }?;
            unsafe {
                *out = context.borrowed_fd().as_raw_fd();
            }
            Ok(())
        }
        #[cfg(not(all(feature = "readiness", unix)))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_duplicate_posix_fd(
    context: *const rumqttc_notification_context,
    out: *mut i32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, -1);
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "notification resource output is NULL",
            ));
        }
        #[cfg(all(feature = "readiness", unix))]
        {
            use std::os::fd::IntoRawFd;
            let context = unsafe { context_ref(context) }?;
            unsafe {
                *out = context
                    .duplicate_fd()
                    .map_err(|_| ErrorHandle::internal("notification resource duplication failed"))?
                    .into_raw_fd();
            }
            Ok(())
        }
        #[cfg(not(all(feature = "readiness", unix)))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_borrowed_windows_handle(
    context: *const rumqttc_notification_context,
    out: *mut *mut c_void,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "notification resource output is NULL",
            ));
        }
        #[cfg(all(feature = "readiness", windows))]
        {
            use std::os::windows::io::AsRawHandle;
            let context = unsafe { context_ref(context) }?;
            unsafe {
                *out = context.borrowed_handle().as_raw_handle();
            }
            Ok(())
        }
        #[cfg(not(all(feature = "readiness", windows)))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_notification_duplicate_windows_handle(
    context: *const rumqttc_notification_context,
    out: *mut *mut c_void,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(out, ptr::null_mut());
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "notification resource output is NULL",
            ));
        }
        #[cfg(all(feature = "readiness", windows))]
        {
            use std::os::windows::io::IntoRawHandle;
            let context = unsafe { context_ref(context) }?;
            unsafe {
                *out = context
                    .duplicate_handle()
                    .map_err(|_| ErrorHandle::internal("notification resource duplication failed"))?
                    .into_raw_handle();
            }
            Ok(())
        }
        #[cfg(not(all(feature = "readiness", windows)))]
        {
            let _ = context;
            Err(unsupported())
        }
    })
}

#[cfg(all(test, feature = "readiness"))]
mod tests {
    use super::*;

    #[test]
    fn registration_does_not_acquire_the_client_event_consumer_mutex() {
        let client = rumqttc_client {
            inner: crate::client::ClientObject::start(
                rumqttc_wrapper_core::ClientConfig::v4("readiness-mutex", "127.0.0.1", 65535),
                None,
                None,
            )
            .unwrap(),
        };
        let receive = client.inner.events.lock().unwrap();
        let mut context = ptr::null_mut();
        let mut registration = rumqttc_notification_key_t::default();
        unsafe {
            assert_eq!(
                rumqttc_notification_context_new(ptr::null(), &raw mut context, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_notification_register_events(
                    context,
                    &raw const client,
                    0,
                    &raw mut registration,
                    ptr::null_mut()
                ),
                0
            );
            assert_eq!(
                rumqttc_notification_deregister(context, registration, ptr::null_mut()),
                0
            );
            assert_eq!(
                rumqttc_notification_context_request_close(context, ptr::null_mut()),
                0
            );
            rumqttc_notification_context_release(context);
        }
        drop(receive);
        assert!(
            client
                .inner
                .shutdown_and_join(std::time::Duration::from_secs(5))
                .is_ok()
        );
    }
}
