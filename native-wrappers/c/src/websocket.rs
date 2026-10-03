//! Deferred WebSocket preparation using the shared callback completion lifecycle.
use super::{
    Arc, AssertUnwindSafe, CallbackCompletion, ErrorHandle, Mutex, OK, ProtocolVersion, boundary,
    bytes_from_view, c_void, catch_unwind, config_ref, config_update, destroy_box, error_detail,
    ptr, rumqttc_bytes_view_t, rumqttc_callback_completion, rumqttc_config, rumqttc_error,
    rumqttc_string_view_t, string_from_view, struct_size, view_bytes, view_string, write_optional,
};
use rumqttc_wrapper_core::{
    WebSocketHandshake, WebSocketHandshakeConfig, WebSocketHandshakeFailure as Failure,
    WebSocketHandshakeFuture, WebSocketHandshakeRequest, WebSocketHandshakeResponse,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[repr(C)]
pub struct rumqttc_websocket_header_t {
    pub struct_size: u32,
    pub name: rumqttc_string_view_t,
    pub value: rumqttc_bytes_view_t,
}

#[repr(C)]
pub struct rumqttc_websocket_request_t {
    pub struct_size: u32,
    pub protocol: u32,
    pub attempt: u64,
    pub remaining_ns: u64,
    pub method: rumqttc_string_view_t,
    pub version: rumqttc_string_view_t,
    pub uri: rumqttc_string_view_t,
    pub path_and_query: rumqttc_string_view_t,
    pub client_id: rumqttc_string_view_t,
    pub broker_host: rumqttc_string_view_t,
    pub broker_port: u32,
    pub tls_authority_present: u8,
    pub reserved_flags: [u8; 3],
    pub dial_target: rumqttc_string_view_t,
    pub tls_authority: rumqttc_string_view_t,
    pub headers: *const rumqttc_websocket_header_t,
    pub header_count: usize,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_websocket_vtable_t {
    pub struct_size: u32,
    pub prepare: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_websocket_request_t,
            *mut rumqttc_callback_completion,
        ),
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}

pub struct rumqttc_websocket_registration {
    authority: Arc<CHandshake>,
}
pub struct rumqttc_websocket_response {
    inner: Mutex<WebSocketHandshakeResponse>,
}

struct Owner {
    vtable: rumqttc_websocket_vtable_t,
    data: usize,
}
// The registration contract requires thread-safe callbacks and destruction.
// Rust only passes the original opaque pointer back to those callbacks.
unsafe impl Send for Owner {}
unsafe impl Sync for Owner {}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe { self.vtable.destroy.expect("validated destructor")(self.data as *mut c_void) };
    }
}
struct CHandshake(Arc<Owner>);
type Reply = Result<WebSocketHandshakeResponse, Failure>;
enum State {
    Pending(tokio::sync::oneshot::Sender<Reply>),
    Completed,
    Cancelled,
}
pub(super) struct WebSocketCompletion {
    _owner: Arc<Owner>,
    state: Mutex<State>,
    hosts: AtomicUsize,
}
impl WebSocketCompletion {
    pub(super) fn retain_host(&self) {
        self.hosts.fetch_add(1, Ordering::Relaxed);
    }
    pub(super) fn release_host(&self) {
        if self.hosts.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.finish(|| Ok(Err(Failure::Abandoned)));
        }
    }
    fn finish(&self, make: impl FnOnce() -> Result<Reply, u32>) -> u32 {
        let (sender, result) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(&*state, State::Pending(sender) if !sender.is_closed()) {
                return crate::error::INVALID_STATE;
            }
            let result = match make() {
                Ok(result) => result,
                Err(status) => return status,
            };
            let State::Pending(sender) = std::mem::replace(&mut *state, State::Completed) else {
                unreachable!()
            };
            drop(state);
            (sender, result)
        };
        if sender.send(result).is_ok() {
            OK
        } else {
            crate::error::INVALID_STATE
        }
    }
}
struct Cancel(Arc<WebSocketCompletion>);
impl Drop for Cancel {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, State::Pending(_)) {
            *state = State::Cancelled;
        }
    }
}
impl WebSocketHandshake for CHandshake {
    fn prepare(&self, context: WebSocketHandshakeRequest) -> WebSocketHandshakeFuture {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let inner = Arc::new(WebSocketCompletion {
            _owner: self.0.clone(),
            state: Mutex::new(State::Pending(sender)),
            hosts: AtomicUsize::new(1),
        });
        let cancel = Cancel(inner.clone());
        let headers: Vec<_> = context
            .headers
            .iter()
            .map(|header| rumqttc_websocket_header_t {
                struct_size: struct_size::<rumqttc_websocket_header_t>(),
                name: view_string(&header.name),
                value: view_bytes(&header.value),
            })
            .collect();
        let request = rumqttc_websocket_request_t {
            struct_size: struct_size::<rumqttc_websocket_request_t>(),
            protocol: match context.protocol {
                ProtocolVersion::V4 => 1,
                ProtocolVersion::V5 => 2,
            },
            attempt: context.attempt,
            remaining_ns: u64::try_from(
                context
                    .deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_nanos(),
            )
            .unwrap_or(u64::MAX),
            method: view_string(&context.method),
            version: view_string(&context.version),
            uri: view_string(&context.uri),
            path_and_query: view_string(&context.path_and_query),
            client_id: view_string(&context.client_id),
            broker_host: view_string(&context.broker_host),
            broker_port: u32::from(context.broker_port),
            tls_authority_present: u8::from(context.tls_authority.is_some()),
            reserved_flags: [0; 3],
            dial_target: view_string(&context.dial_target),
            tls_authority: view_string(context.tls_authority.as_deref().unwrap_or("")),
            headers: headers.as_ptr(),
            header_count: headers.len(),
            reserved: [0; 2],
        };
        let completion = rumqttc_callback_completion {
            inner: CallbackCompletion::WebSocket(inner),
        };
        unsafe {
            self.0.vtable.prepare.expect("validated callback")(
                self.0.data as *mut c_void,
                &raw const request,
                (&raw const completion).cast_mut(),
            );
        }
        Box::pin(async move {
            let _cancel = cancel;
            receiver.await.unwrap_or(Err(Failure::Abandoned))
        })
    }
}

fn available() -> Result<(), ErrorHandle> {
    if cfg!(feature = "websocket") {
        Ok(())
    } else {
        Err(ErrorHandle::plain(
            crate::error::CONFIG_ERROR,
            1,
            "WebSocket feature is disabled",
        ))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_registration_new(
    vtable: *const rumqttc_websocket_vtable_t,
    data: *mut c_void,
    out: *mut *mut rumqttc_websocket_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        available()?;
        if vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("WebSocket vtable or output is NULL"));
        }
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_websocket_vtable_t>() {
            return Err(ErrorHandle::argument("WebSocket vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        if vtable.reserved != [0; 2] || vtable.prepare.is_none() || vtable.destroy.is_none() {
            return Err(ErrorHandle::argument("invalid WebSocket vtable"));
        }
        let registration = rumqttc_websocket_registration {
            authority: Arc::new(CHandshake(Arc::new(Owner {
                vtable,
                data: data as usize,
            }))),
        };
        unsafe { *out = Box::into_raw(Box::new(registration)) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_registration_destroy(
    registration: *mut rumqttc_websocket_registration,
) {
    unsafe { destroy_box(registration) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_websocket_handshake(
    config: *mut rumqttc_config,
    registration: *const rumqttc_websocket_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        available()?;
        if registration.is_null() {
            return Err(ErrorHandle::argument("WebSocket registration is NULL"));
        }
        let authority = unsafe { &*registration }.authority.clone();
        let config = unsafe { config_ref(config) }?;
        config.update_with_error(
            |config| {
                config.common.websocket_handshake = Some(WebSocketHandshakeConfig(authority));
                Ok(())
            },
            || ErrorHandle::internal("configuration lock is poisoned"),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_websocket_handshake(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        available()?;
        config.common.websocket_handshake = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_response_new(
    out: *mut *mut rumqttc_websocket_response,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        available()?;
        if out.is_null() {
            return Err(ErrorHandle::argument("WebSocket response output is NULL"));
        }
        unsafe {
            *out = Box::into_raw(Box::new(rumqttc_websocket_response {
                inner: Mutex::default(),
            }));
        };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_response_destroy(
    response: *mut rumqttc_websocket_response,
) {
    unsafe { destroy_box(response) };
}

fn update_response(
    response: *mut rumqttc_websocket_response,
    error_out: *mut *mut rumqttc_error,
    edit: impl FnOnce(&mut WebSocketHandshakeResponse) -> Result<(), ErrorHandle>,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if response.is_null() {
            return Err(ErrorHandle::argument("WebSocket response is NULL"));
        }
        let mut response = unsafe { &*response }
            .inner
            .lock()
            .map_err(|_| ErrorHandle::internal("WebSocket response lock is poisoned"))?;
        edit(&mut response)
    })
}
fn builder_error(_: Failure) -> ErrorHandle {
    ErrorHandle::argument("invalid or oversized WebSocket response edit")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_response_set_authority(
    response: *mut rumqttc_websocket_response,
    authority: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if authority.len > rumqttc_wrapper_core::MAX_WEBSOCKET_BYTES {
            return Err(builder_error(Failure::ResourceLimit));
        }
        let authority = unsafe { string_from_view(authority) }?;
        response.set_authority(&authority).map_err(builder_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_response_set_path_and_query(
    response: *mut rumqttc_websocket_response,
    path: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if path.len > rumqttc_wrapper_core::MAX_WEBSOCKET_PATH {
            return Err(builder_error(Failure::ResourceLimit));
        }
        let path = unsafe { string_from_view(path) }?;
        response.set_path_and_query(&path).map_err(builder_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_websocket_response_header_edit(
    response: *mut rumqttc_websocket_response,
    operation: u32,
    name: rumqttc_string_view_t,
    value: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if name
            .len
            .checked_add(value.len)
            .is_none_or(|n| n > rumqttc_wrapper_core::MAX_WEBSOCKET_BYTES)
        {
            return Err(builder_error(Failure::ResourceLimit));
        }
        if !(0..=2).contains(&operation) || (operation == 2 && value.len != 0) {
            return Err(builder_error(Failure::InvalidResponse));
        }
        let name = unsafe { string_from_view(name) }?;
        let value = unsafe { bytes_from_view(value) }?;
        match operation {
            0 => response.append_header(&name, value),
            1 => response.replace_header(&name, value),
            2 => response.remove_header(&name),
            _ => unreachable!(),
        }
        .map_err(builder_error)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_websocket_complete(
    completion: *mut rumqttc_callback_completion,
    response: *const rumqttc_websocket_response,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if completion.is_null() {
            return crate::error::INVALID_ARGUMENT;
        }
        let CallbackCompletion::WebSocket(inner) = (unsafe { &(*completion).inner }) else {
            return crate::error::INVALID_STATE;
        };
        inner.finish(|| {
            if response.is_null() {
                return Err(crate::error::INVALID_ARGUMENT);
            }
            let response = unsafe { &*response }
                .inner
                .lock()
                .map_err(|_| crate::error::INTERNAL_ERROR)?;
            Ok(Ok(response.clone()))
        })
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_callback_websocket_reject(
    completion: *mut rumqttc_callback_completion,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if completion.is_null() {
            return crate::error::INVALID_ARGUMENT;
        }
        let CallbackCompletion::WebSocket(inner) = (unsafe { &(*completion).inner }) else {
            return crate::error::INVALID_STATE;
        };
        inner.finish(|| Ok(Err(Failure::Rejected)))
    }))
    .unwrap_or(crate::error::INTERNAL_ERROR)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_websocket_failure(
    error: *const rumqttc_error,
    present_out: *mut u8,
    failure_out: *mut u32,
) -> u32 {
    error_detail(error, present_out, failure_out, |error| {
        error
            .failure_details()
            .websocket
            .map(std::num::NonZeroU32::get)
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "websocket")]
    use super::super::{rumqttc_callback_completion_destroy, rumqttc_callback_completion_retain};
    use super::*;
    #[cfg(feature = "websocket")]
    use std::time::{Duration, Instant};

    #[cfg(feature = "websocket")]
    #[test]
    fn authority_setter_checks_views_and_bounds_and_clears_error_outputs() {
        let mut response = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_websocket_response_new(&raw mut response, ptr::null_mut()) },
            OK
        );
        let mut error = ptr::dangling_mut::<rumqttc_error>();
        let value = b"customer.example:443";
        let valid = rumqttc_string_view_t {
            data: value.as_ptr().cast(),
            len: value.len(),
        };
        assert_eq!(
            unsafe { rumqttc_websocket_response_set_authority(response, valid, &raw mut error) },
            OK
        );
        assert!(error.is_null());
        for invalid in [
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 1,
            },
            rumqttc_string_view_t {
                data: ptr::null(),
                len: rumqttc_wrapper_core::MAX_WEBSOCKET_BYTES + 1,
            },
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
            rumqttc_string_view_t {
                data: b"\xff".as_ptr().cast(),
                len: 1,
            },
        ] {
            assert_eq!(
                unsafe {
                    rumqttc_websocket_response_set_authority(response, invalid, &raw mut error)
                },
                crate::error::INVALID_ARGUMENT
            );
            assert!(!error.is_null());
            unsafe {
                crate::rumqttc_error_destroy(error);
            }
        }
        assert_eq!(
            unsafe {
                rumqttc_websocket_response_set_authority(ptr::null_mut(), valid, ptr::null_mut())
            },
            crate::error::INVALID_ARGUMENT
        );
        unsafe {
            rumqttc_websocket_response_destroy(response);
        }
    }

    #[derive(Default)]
    struct Host {
        #[cfg(feature = "websocket")]
        tokens: Mutex<Vec<usize>>,
        destroyed: AtomicUsize,
    }
    #[cfg(feature = "websocket")]
    unsafe extern "C" fn prepare(
        data: *mut c_void,
        request: *const rumqttc_websocket_request_t,
        completion: *mut rumqttc_callback_completion,
    ) {
        let host = unsafe { &*data.cast::<Arc<Host>>() };
        assert_eq!(unsafe { (*request).attempt }, 1);
        assert_eq!(unsafe { (*request).header_count }, 0);
        let mut token = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_callback_completion_retain(completion, &raw mut token) },
            OK
        );
        host.tokens.lock().unwrap().push(token as usize);
    }
    #[cfg(feature = "websocket")]
    unsafe extern "C" fn destroy(data: *mut c_void) {
        let host = unsafe { Box::from_raw(data.cast::<Arc<Host>>()) };
        host.destroyed.fetch_add(1, Ordering::SeqCst);
    }
    #[cfg(feature = "websocket")]
    fn request() -> WebSocketHandshakeRequest {
        WebSocketHandshakeRequest {
            protocol: ProtocolVersion::V4,
            client_id: "client".into(),
            attempt: 1,
            method: "GET".into(),
            version: "HTTP/1.1".into(),
            uri: "ws://localhost/mqtt".into(),
            path_and_query: "/mqtt".into(),
            headers: vec![],
            broker_host: "localhost".into(),
            broker_port: 80,
            dial_target: "localhost:80".into(),
            tls_authority: None,
            deadline: Instant::now() + Duration::from_secs(2),
        }
    }
    #[cfg(feature = "websocket")]
    fn register(host: &Arc<Host>) -> *mut rumqttc_websocket_registration {
        let vtable = rumqttc_websocket_vtable_t {
            struct_size: struct_size::<rumqttc_websocket_vtable_t>(),
            prepare: Some(prepare),
            destroy: Some(destroy),
            reserved: [0; 2],
        };
        let data = Box::into_raw(Box::new(host.clone())).cast();
        let mut registration = ptr::null_mut();
        assert_eq!(
            unsafe {
                rumqttc_websocket_registration_new(
                    &raw const vtable,
                    data,
                    &raw mut registration,
                    ptr::null_mut(),
                )
            },
            OK
        );
        registration
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn completion_races_cancel_and_retain_owner_until_last_host_handle() {
        let host = Arc::new(Host::default());
        let registration = register(&host);
        let authority = unsafe { &*registration }.authority.clone();
        let future = authority.prepare(request());
        let token = host.tokens.lock().unwrap().pop().unwrap() as *mut rumqttc_callback_completion;
        let mut second = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_callback_completion_retain(token, &raw mut second) },
            OK
        );
        let mut response = ptr::null_mut();
        assert_eq!(
            unsafe { rumqttc_websocket_response_new(&raw mut response, ptr::null_mut()) },
            OK
        );
        let response_address = response as usize;
        let first_address = token as usize;
        let second_address = second as usize;
        let a = std::thread::spawn(move || unsafe {
            rumqttc_callback_websocket_complete(first_address as _, response_address as _)
        });
        let b = std::thread::spawn(move || unsafe {
            rumqttc_callback_websocket_reject(second_address as _)
        });
        let statuses = [a.join().unwrap(), b.join().unwrap()];
        assert!(
            statuses == [OK, crate::error::INVALID_STATE]
                || statuses == [crate::error::INVALID_STATE, OK]
        );
        let _ = future.await;
        assert_eq!(
            unsafe {
                rumqttc_callback_websocket_complete(
                    token,
                    ptr::dangling::<rumqttc_websocket_response>(),
                )
            },
            crate::error::INVALID_STATE
        );
        unsafe {
            rumqttc_websocket_response_destroy(response);
            rumqttc_callback_completion_destroy(token);
            rumqttc_callback_completion_destroy(second);
        }

        let cancelled = authority.prepare(request());
        let token = host.tokens.lock().unwrap().pop().unwrap() as *mut rumqttc_callback_completion;
        drop(cancelled);
        drop(authority);
        unsafe {
            rumqttc_websocket_registration_destroy(registration);
        }
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 0);
        assert_eq!(
            unsafe {
                rumqttc_callback_websocket_complete(
                    token,
                    ptr::dangling::<rumqttc_websocket_response>(),
                )
            },
            crate::error::INVALID_STATE
        );
        unsafe {
            rumqttc_callback_completion_destroy(token);
        }
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "websocket")]
    #[tokio::test]
    async fn releasing_last_host_token_abandons_pending_work() {
        let host = Arc::new(Host::default());
        let registration = register(&host);
        let authority = unsafe { &*registration }.authority.clone();
        let future = authority.prepare(request());
        let token = host.tokens.lock().unwrap().pop().unwrap() as *mut rumqttc_callback_completion;
        unsafe {
            rumqttc_callback_completion_destroy(token);
        }
        assert!(matches!(future.await, Err(Failure::Abandoned)));
        drop(authority);
        unsafe {
            rumqttc_websocket_registration_destroy(registration);
        }
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_registration_leaves_user_data_with_caller_and_clears_outputs() {
        let host = Arc::new(Host::default());
        let data = Box::into_raw(Box::new(host.clone()));
        let mut out = ptr::dangling_mut::<rumqttc_websocket_registration>();
        assert_ne!(
            unsafe {
                rumqttc_websocket_registration_new(
                    ptr::null(),
                    data.cast(),
                    &raw mut out,
                    ptr::null_mut(),
                )
            },
            OK
        );
        assert!(out.is_null());
        assert_eq!(host.destroyed.load(Ordering::SeqCst), 0);
        unsafe {
            drop(Box::from_raw(data));
        }
    }
}
