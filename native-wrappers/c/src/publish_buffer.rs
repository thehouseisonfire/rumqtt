//! Application storage is owned by Bytes, never reconstructed as a Rust allocation.
use super::{
    Bytes, Command, PublishCommand, admit, boundary, destroy_box, pending_admission,
    publish_command, rumqttc_bytes_view_t, rumqttc_client, rumqttc_completion, rumqttc_error,
    rumqttc_pending_admission, rumqttc_publish_options_t, rumqttc_string_view_t, write_admission,
    write_optional,
};
use crate::error::ErrorHandle;
use std::{ffi::c_void, ptr, slice};

pub type rumqttc_publish_buffer_release_fn =
    Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>;
type Release = unsafe extern "C" fn(*mut c_void, *const u8, usize);

pub struct rumqttc_publish_buffer {
    inner: Bytes,
}

struct Owner {
    data: *const u8,
    len: usize,
    release: Release,
    userdata: *mut c_void,
}

// SAFETY: adoption requires stable immutable storage through final release, and
// permits the callback/context to move to any thread. Only Drop uses the context.
unsafe impl Send for Owner {}

impl AsRef<[u8]> for Owner {
    fn as_ref(&self) -> &[u8] {
        if self.len == 0 {
            &[]
        } else {
            // SAFETY: adoption validated the length; the caller guarantees one
            // stable readable allocation. The borrow is bounded by this owner.
            unsafe { slice::from_raw_parts(self.data, self.len) }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        rumqttc_wrapper_core::with_host_callback(|| {
            // SAFETY: the owner invokes the caller's thread-safe callback once,
            // after all Bytes clones have finished accessing this storage.
            unsafe { (self.release)(self.userdata, self.data, self.len) };
        });
    }
}

// All reported failures precede creation of the owner. Its AsRef cannot panic;
// no fallible work remains after ownership is committed to Bytes.
pub(super) unsafe fn adopt(
    data: rumqttc_bytes_view_t,
    release: rumqttc_publish_buffer_release_fn,
    userdata: *mut c_void,
) -> Result<Bytes, ErrorHandle> {
    let release =
        release.ok_or_else(|| ErrorHandle::argument("buffer release callback is NULL"))?;
    if data.len > isize::MAX as usize || (data.len != 0 && data.data.is_null()) {
        return Err(ErrorHandle::argument("invalid buffer pointer or length"));
    }
    Ok(Bytes::from_owner(Owner {
        data: data.data,
        len: data.len,
        release,
        userdata,
    }))
}

#[cfg(all(test, feature = "publish-buffer-proof"))]
#[path = "publish_buffer/proof.rs"]
mod proof;

unsafe fn buffer_ref<'a>(buffer: *const rumqttc_publish_buffer) -> Result<&'a Bytes, ErrorHandle> {
    if buffer.is_null() {
        return Err(ErrorHandle::argument("publish buffer is NULL"));
    }
    // SAFETY: the caller retains this handle throughout the current operation.
    Ok(&unsafe { &*buffer }.inner)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_publish_buffer_create(
    data: rumqttc_bytes_view_t,
    release: rumqttc_publish_buffer_release_fn,
    userdata: *mut c_void,
    out: *mut *mut rumqttc_publish_buffer,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("publish buffer output is NULL"));
        }
        // Allocate the handle before committing ownership. After adopt succeeds,
        // only infallible assignment and publication of the handle remain.
        let mut handle = Box::new(rumqttc_publish_buffer {
            inner: Bytes::new(),
        });
        handle.inner = unsafe { adopt(data, release, userdata) }?;
        unsafe { *out = Box::into_raw(handle) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_publish_buffer_retain(
    buffer: *const rumqttc_publish_buffer,
    out: *mut *mut rumqttc_publish_buffer,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("publish buffer output is NULL"));
        }
        let inner = unsafe { buffer_ref(buffer) }?.clone();
        unsafe { *out = Box::into_raw(Box::new(rumqttc_publish_buffer { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_publish_buffer_release(buffer: *mut rumqttc_publish_buffer) {
    unsafe { destroy_box(buffer) };
}

pub(super) unsafe fn buffer_command(
    topic: rumqttc_string_view_t,
    buffer: *const rumqttc_publish_buffer,
    options: *const rumqttc_publish_options_t,
) -> Result<PublishCommand, ErrorHandle> {
    let payload = unsafe { buffer_ref(buffer) }?;
    // Reuse exactly the existing metadata parser. Its empty payload conversion
    // allocates nothing. Clone application storage only after parsing succeeds.
    let mut command = unsafe {
        publish_command(
            topic,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
            options,
        )
    }?;
    command.payload = payload.clone();
    Ok(command)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_publish_buffer(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    buffer: *const rumqttc_publish_buffer,
    options: *const rumqttc_publish_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(operation_id_out, 0) };
    boundary(error_out, client, || {
        if operation_id_out.is_null() {
            return Err(ErrorHandle::argument("operation ID output is NULL"));
        }
        let command = unsafe { buffer_command(topic, buffer, options) }?;
        write_admission(
            admit(client, Command::Publish(command))?,
            operation_id_out,
            ptr::null_mut(),
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_publish_buffer_tracked(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    buffer: *const rumqttc_publish_buffer,
    options: *const rumqttc_publish_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(completion_out, ptr::null_mut()) };
    boundary(error_out, client, || {
        if completion_out.is_null() {
            return Err(ErrorHandle::argument("completion output is NULL"));
        }
        let command = unsafe { buffer_command(topic, buffer, options) }?;
        write_admission(
            admit(client, Command::Publish(command))?,
            ptr::null_mut(),
            completion_out,
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_publish_buffer_deferred(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    buffer: *const rumqttc_publish_buffer,
    options: *const rumqttc_publish_options_t,
    out: *mut *mut rumqttc_pending_admission,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    pending_admission::create(
        client,
        out,
        error_out,
        || {
            let bytes = unsafe { buffer_ref(buffer) }?;
            unsafe { pending_admission::publish_charge(topic, bytes.len(), options) }
        },
        || unsafe { buffer_command(topic, buffer, options) }.map(Command::Publish),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn released(userdata: *mut c_void, _: *const u8, _: usize) {
        unsafe { &*userdata.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
        assert!(!rumqttc_wrapper_core::blocking_wait_allowed());
    }

    #[test]
    fn creation_is_transactional_and_independent_handles_release_once() {
        let calls = AtomicUsize::new(0);
        let context = std::ptr::from_ref(&calls).cast_mut().cast();
        let data = [42; 8];
        let mut out = ptr::dangling_mut();
        for (view, callback) in [
            (
                rumqttc_bytes_view_t {
                    data: ptr::null(),
                    len: 1,
                },
                Some(released as Release),
            ),
            (
                rumqttc_bytes_view_t {
                    data: data.as_ptr(),
                    len: usize::MAX,
                },
                Some(released as Release),
            ),
            (
                rumqttc_bytes_view_t {
                    data: data.as_ptr(),
                    len: data.len(),
                },
                None,
            ),
        ] {
            assert_eq!(
                unsafe {
                    rumqttc_publish_buffer_create(
                        view,
                        callback,
                        context,
                        &raw mut out,
                        ptr::null_mut(),
                    )
                },
                1
            );
            assert!(out.is_null());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        assert_eq!(
            unsafe {
                rumqttc_publish_buffer_create(
                    rumqttc_bytes_view_t {
                        data: data.as_ptr(),
                        len: 8,
                    },
                    Some(released),
                    context,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        for pointer in [ptr::null(), data.as_ptr()] {
            assert_eq!(
                unsafe {
                    rumqttc_publish_buffer_create(
                        rumqttc_bytes_view_t {
                            data: pointer,
                            len: 0,
                        },
                        Some(released),
                        context,
                        &raw mut out,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let mut references = Vec::new();
            for _ in 0..8 {
                let mut retained = ptr::null_mut();
                assert_eq!(
                    unsafe {
                        rumqttc_publish_buffer_retain(out, &raw mut retained, ptr::null_mut())
                    },
                    0
                );
                references.push(retained as usize);
            }
            unsafe { rumqttc_publish_buffer_release(out) };
            let threads: Vec<_> = references
                .into_iter()
                .map(|reference| {
                    std::thread::spawn(move || unsafe {
                        rumqttc_publish_buffer_release(reference as *mut rumqttc_publish_buffer);
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        unsafe { rumqttc_publish_buffer_release(ptr::null_mut()) };
    }

    #[test]
    fn metadata_parsing_shares_the_exact_application_pointer() {
        let calls = AtomicUsize::new(0);
        let data = [0, 1, 2, 255];
        let mut out = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_publish_buffer_create(
                    rumqttc_bytes_view_t {
                        data: data.as_ptr(),
                        len: data.len(),
                    },
                    Some(released),
                    std::ptr::from_ref(&calls).cast_mut().cast(),
                    &raw mut out,
                    ptr::null_mut(),
                )
            },
            0
        );
        let command = unsafe {
            buffer_command(
                rumqttc_string_view_t {
                    data: c"a".as_ptr(),
                    len: 1,
                },
                out,
                ptr::null(),
            )
        }
        .unwrap();
        assert_eq!(command.payload.as_ptr(), data.as_ptr());
        let cloned = command.clone();
        unsafe { rumqttc_publish_buffer_release(out) };
        drop(command);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        drop(cloned);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
