//! Synchronous application redirect decisions with retained credential-free snapshots.
use super::{
    Arc, ErrorHandle, Mutex, OK, boundary, bytes_from_view, c_void, config_ref, destroy_box,
    event_ref, ptr, rumqttc_bytes_view_t, rumqttc_config, rumqttc_error, rumqttc_event,
    rumqttc_string_view_t, rumqttc_tls_profile, string_from_view, struct_size, view_string,
    write_optional,
};
use rumqttc_wrapper_core::{
    RedirectAuthority, RedirectAuthorityConfig, RedirectClientId,
    RedirectDecisionFailure as Failure, RedirectPolicy, RedirectRequest, RedirectResponse,
    RedirectScheme, RedirectSession, RedirectTargetConfig, SecretBytes, TransportConfig,
    WrapperEvent,
};
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_redirect_vtable_t {
    pub struct_size: u32,
    pub decide: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const rumqttc_redirect_request,
            *mut rumqttc_redirect_response,
        ) -> u32,
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_redirect_request_info_t {
    pub struct_size: u32,
    pub source: u32,
    pub reason: u32,
    pub attempt: u64,
    pub remaining_ns: u64,
    pub client_id: rumqttc_string_view_t,
    pub store_scope: rumqttc_string_view_t,
    pub reference_count: usize,
    pub reserved: [u64; 2],
}
#[repr(C)]
pub struct rumqttc_redirect_reference_t {
    pub struct_size: u32,
    pub kind: u32,
    pub scheme: u32,
    pub port: u32,
    pub raw: rumqttc_string_view_t,
    pub host: rumqttc_string_view_t,
    pub websocket_resource: rumqttc_string_view_t,
    pub srv_owner: rumqttc_string_view_t,
    pub reserved: [u64; 2],
}
pub struct rumqttc_redirect_registration {
    config: RedirectAuthorityConfig,
}
pub struct rumqttc_redirect_request {
    inner: Arc<RedirectRequest>,
}
struct ResponseState {
    response: RedirectResponse,
    failure: Option<Failure>,
}
/// Borrowed only while the callback is active. No retain or deferred completion operation.
pub struct rumqttc_redirect_response {
    inner: Mutex<ResponseState>,
}
struct Owner {
    vtable: rumqttc_redirect_vtable_t,
    data: usize,
}
// Only the host dereferences its pointer; registrations require thread-safe callbacks/destruction.
unsafe impl Send for Owner {}
unsafe impl Sync for Owner {}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe { self.vtable.destroy.expect("validated destructor")(self.data as *mut c_void) };
    }
}
impl RedirectAuthority for Owner {
    fn decide(&self, request: Arc<RedirectRequest>) -> Result<RedirectResponse, Failure> {
        let mut response = rumqttc_redirect_response {
            inner: Mutex::new(ResponseState {
                response: RedirectResponse::reject(request.clone()),
                failure: None,
            }),
        };
        let request = rumqttc_redirect_request { inner: request };
        let status = unsafe {
            self.vtable.decide.expect("validated callback")(
                self.data as *mut c_void,
                &raw const request,
                &raw mut response,
            )
        };
        let state = response.inner.into_inner().map_err(|_| Failure::Panic)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        if status != OK {
            return Err(Failure::Callback);
        }
        Ok(state.response)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_registration_new(
    vtable: *const rumqttc_redirect_vtable_t,
    data: *mut c_void,
    max_attempts: u32,
    decision_timeout_ms: u64,
    out: *mut *mut rumqttc_redirect_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if vtable.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("redirect vtable or output is NULL"));
        }
        if unsafe { (*vtable).struct_size } < struct_size::<rumqttc_redirect_vtable_t>() {
            return Err(ErrorHandle::argument("redirect vtable is too small"));
        }
        let vtable = unsafe { *vtable };
        let timeout = Duration::from_millis(decision_timeout_ms);
        if vtable.reserved != [0; 2]
            || vtable.decide.is_none()
            || vtable.destroy.is_none()
            || max_attempts == 0
            || timeout.is_zero()
            || std::time::Instant::now().checked_add(timeout).is_none()
        {
            return Err(ErrorHandle::argument(
                "invalid redirect registration or bounds",
            ));
        }
        // Ownership transfers only after all fallible input validation succeeds.
        let config = RedirectAuthorityConfig {
            authority: Arc::new(Owner {
                vtable,
                data: data as usize,
            }),
            max_attempts: max_attempts as usize,
            decision_timeout: timeout,
        };
        unsafe { *out = Box::into_raw(Box::new(rumqttc_redirect_registration { config })) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_registration_destroy(
    registration: *mut rumqttc_redirect_registration,
) {
    unsafe { destroy_box(registration) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_redirect_authority(
    config: *mut rumqttc_config,
    registration: *const rumqttc_redirect_registration,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if registration.is_null() {
            return Err(ErrorHandle::argument("redirect registration is NULL"));
        }
        let policy = RedirectPolicy::Application(unsafe { (*registration).config.clone() });
        unsafe { config_ref(config) }?.update_with_error(
            |config| {
                let rumqttc_wrapper_core::ProtocolConfig::V5(v5) = &mut config.protocol else {
                    return Err(ErrorHandle::argument("redirects require MQTT 5"));
                };
                v5.redirect_policy = policy;
                Ok(())
            },
            || ErrorHandle::state("configuration lock is poisoned"),
        )
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_request_retain(
    request: *const rumqttc_redirect_request,
    out: *mut *mut rumqttc_redirect_request,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe { write_optional(out, ptr::null_mut()) };
    boundary(error_out, ptr::null_mut(), || {
        if request.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("redirect request or output is NULL"));
        }
        let inner = unsafe { (*request).inner.clone() };
        unsafe { *out = Box::into_raw(Box::new(rumqttc_redirect_request { inner })) };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_request_destroy(request: *mut rumqttc_redirect_request) {
    unsafe { destroy_box(request) };
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_request_info(
    request: *const rumqttc_redirect_request,
    out: *mut rumqttc_redirect_request_info_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if request.is_null() || out.is_null() {
            return Err(ErrorHandle::argument("redirect request or output is NULL"));
        }
        if unsafe { (*out).struct_size } < struct_size::<rumqttc_redirect_request_info_t>() {
            return Err(ErrorHandle::argument(
                "redirect request output is too small",
            ));
        }
        let request = unsafe { &(*request).inner };
        unsafe {
            *out = rumqttc_redirect_request_info_t {
                struct_size: struct_size::<rumqttc_redirect_request_info_t>(),
                source: match request.source {
                    rumqttc_wrapper_core::RedirectSource::ConnAck => 1,
                    rumqttc_wrapper_core::RedirectSource::Disconnect => 2,
                },
                reason: match request.reason {
                    rumqttc_wrapper_core::RedirectReason::UseAnotherServer => 1,
                    rumqttc_wrapper_core::RedirectReason::ServerMoved => 2,
                },
                attempt: request.attempt as u64,
                remaining_ns: u64::try_from(
                    request
                        .deadline
                        .saturating_duration_since(std::time::Instant::now())
                        .as_nanos(),
                )
                .unwrap_or(u64::MAX),
                client_id: view_string(&request.client_id),
                store_scope: view_string(&request.store_scope),
                reference_count: request.references.len(),
                reserved: [0; 2],
            }
        };
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_request_reference(
    request: *const rumqttc_redirect_request,
    index: usize,
    out: *mut rumqttc_redirect_reference_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if request.is_null() || out.is_null() {
            return Err(ErrorHandle::argument(
                "redirect reference input/output is NULL",
            ));
        }
        if unsafe { (*out).struct_size } < struct_size::<rumqttc_redirect_reference_t>() {
            return Err(ErrorHandle::argument(
                "redirect reference output is too small",
            ));
        }
        let request = unsafe { &(*request).inner };
        let r = request
            .references
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("redirect reference index is out of bounds"))?;
        unsafe {
            *out = rumqttc_redirect_reference_t {
                struct_size: struct_size::<rumqttc_redirect_reference_t>(),
                kind: if r.srv_owner.is_some() {
                    3
                } else if r.scheme.is_some() {
                    2
                } else {
                    1
                },
                scheme: match r.scheme {
                    None => 0,
                    Some(RedirectScheme::Mqtt) => 1,
                    Some(RedirectScheme::Mqtts) => 2,
                    Some(RedirectScheme::Ws) => 3,
                    Some(RedirectScheme::Wss) => 4,
                },
                port: r.port.map_or(0, u32::from),
                raw: view_string(&r.raw),
                host: view_string(&r.host),
                websocket_resource: view_string(r.websocket_resource.as_deref().unwrap_or("")),
                srv_owner: view_string(r.srv_owner.as_deref().unwrap_or("")),
                reserved: [0; 2],
            }
        };
        Ok(())
    })
}
fn update_response(
    response: *mut rumqttc_redirect_response,
    error_out: *mut *mut rumqttc_error,
    update: impl FnOnce(&mut RedirectResponse) -> Result<(), ErrorHandle>,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if response.is_null() {
            return Err(ErrorHandle::argument("redirect response is NULL"));
        }
        let mut state = unsafe { &(*response).inner }
            .lock()
            .map_err(|_| ErrorHandle::state("redirect response lock is poisoned"))?;
        let mut proposed = state.response.clone();
        let result = update(&mut proposed);
        if let Err(error) = result {
            state.failure = Some(Failure::InvalidResponse);
            return Err(error);
        }
        state.response = proposed;
        drop(state);
        Ok(())
    })
}
fn target_mut(response: &mut RedirectResponse) -> Result<&mut RedirectTargetConfig, ErrorHandle> {
    response
        .target_mut()
        .ok_or_else(|| ErrorHandle::state("select a redirect reference first"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_follow(
    response: *mut rumqttc_redirect_response,
    request: *const rumqttc_redirect_request,
    index: usize,
    transport: u32,
    tls: *const rumqttc_tls_profile,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if request.is_null() || !Arc::ptr_eq(response.request(), unsafe { &(*request).inner }) {
            return Err(ErrorHandle::argument(
                "redirect selection belongs to another request",
            ));
        }
        let transport = match transport {
            0 if tls.is_null() => TransportConfig::Tcp,
            2 if tls.is_null() => TransportConfig::WebSocket,
            1 | 3 if !tls.is_null() => {
                let tls = unsafe { (*tls).config.clone() };
                if transport == 1 {
                    TransportConfig::Tls(tls)
                } else {
                    TransportConfig::Wss(tls)
                }
            }
            _ => return Err(ErrorHandle::argument("invalid redirect transport/profile")),
        };
        *response = RedirectResponse::follow(
            response.request().clone(),
            index,
            RedirectTargetConfig::new(transport),
        )
        .map_err(|_| ErrorHandle::argument("invalid redirect selection"))?;
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_reject(
    response: *mut rumqttc_redirect_response,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        *response = RedirectResponse::reject(response.request().clone());
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_set_client_id(
    response: *mut rumqttc_redirect_response,
    policy: u32,
    id: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if id.len > 65535 {
            return Err(ErrorHandle::argument("redirect client ID is oversized"));
        }
        let target = target_mut(response)?;
        target.client_id = match policy {
            0 if id.len == 0 => RedirectClientId::Fresh,
            1 if id.len == 0 => RedirectClientId::Reuse,
            2 => RedirectClientId::Replace(unsafe { string_from_view(id) }?),
            _ => return Err(ErrorHandle::argument("invalid redirect client-ID policy")),
        };
        target
            .validate()
            .map_err(|_| ErrorHandle::argument("invalid redirect client ID"))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_set_credentials(
    response: *mut rumqttc_redirect_response,
    username_present: u8,
    username: rumqttc_string_view_t,
    password_present: u8,
    password: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if username_present > 1
            || password_present > 1
            || (username_present == 0 && username.len != 0)
            || (password_present == 0 && password.len != 0)
            || username.len > 65535
            || password.len > 65535
        {
            return Err(ErrorHandle::argument("invalid redirect credentials"));
        }
        let target = target_mut(response)?;
        target.username = if username_present == 1 {
            Some(unsafe { string_from_view(username) }?)
        } else {
            None
        };
        target.password = if password_present == 1 {
            Some(SecretBytes::new(
                unsafe { bytes_from_view(password) }?.to_vec(),
            ))
        } else {
            None
        };
        target
            .validate()
            .map_err(|_| ErrorHandle::argument("invalid redirect credentials"))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_set_reuse(
    response: *mut rumqttc_redirect_response,
    authentication_authority: u8,
    network_credentials: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if authentication_authority > 1 || network_credentials > 1 {
            return Err(ErrorHandle::argument("invalid redirect reuse flags"));
        }
        let target = target_mut(response)?;
        target.reuse_authentication_authority = authentication_authority == 1;
        target.reuse_network_credentials = network_credentials == 1;
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_redirect_response_set_session(
    response: *mut rumqttc_redirect_response,
    policy: u32,
    scope: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    update_response(response, error_out, |response| {
        if scope.len > 65535 {
            return Err(ErrorHandle::argument("redirect scope is oversized"));
        }
        let target = target_mut(response)?;
        target.session = match policy {
            0 if scope.len == 0 => RedirectSession::Isolated,
            1 => RedirectSession::Reuse {
                store_scope: unsafe { string_from_view(scope) }?,
            },
            _ => return Err(ErrorHandle::argument("invalid redirect session policy")),
        };
        target
            .validate()
            .map_err(|_| ErrorHandle::argument("invalid redirect session scope"))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_redirect_selected_reference(
    event: *const rumqttc_event,
    present_out: *mut u8,
    out: *mut rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(
            out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(error_out, ptr::null_mut(), || {
        if present_out.is_null() && out.is_null() {
            return Err(ErrorHandle::argument("redirect reference output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Redirect(event) = &event.event else {
            return Err(ErrorHandle::state("event is not a redirect"));
        };
        unsafe {
            write_optional(present_out, u8::from(event.selected_reference.is_some()));
            write_optional(
                out,
                view_string(event.selected_reference.as_deref().unwrap_or("")),
            );
        }
        Ok(())
    })
}
