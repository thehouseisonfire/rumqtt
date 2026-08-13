#![allow(non_camel_case_types)]
// The exported operations share one safety contract: every non-null pointer must satisfy the
// ownership, lifetime, alignment, readability, and writability requirements documented in
// `rumqttc.h`. Repeating that contract on every C entry point would obscure the ABI surface.
#![allow(clippy::missing_safety_doc)]
// C callers cannot express Rust's `unsafe` qualifier. Every exported entry
// point validates nullable arguments and confines dereferences to explicit
// unsafe blocks inside the panic boundary.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::slice;
use std::time::Duration;

use bytes::Bytes;
use rumqttc_wrapper_core::{
    Admission, Command, Completion, DiagnosticsSnapshot, IncomingPublish, OutgoingActivity,
    ProtocolVersion, PublishCommand, PublishCompletion, PublishProtocolOptions, QoS,
    SubscribeCommand, SubscribeProtocolOptions, SubscribeResult, Subscription,
    SubscriptionProtocolOptions, UnsubscribeCommand, UnsubscribeProtocolOptions, UnsubscribeResult,
    V5PublishProperties, V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions,
    V5UnsubscribeProperties, WrapperEvent,
};

use crate::client::{ClientError, ClientObject};
use crate::completion::CompletionObject;
use crate::config::{
    ConfigHandle, set_ack_mode, set_connection_timeout, set_event_delivery_timeout, set_keep_alive,
    set_transport_tcp, set_transport_tls, set_transport_websocket, set_transport_wss,
    set_v5_session, set_v311_clean_session, tls_config,
};
use crate::error::{ErrorHandle, OK, TIMEOUT, WOULD_BLOCK};
use crate::event::EventObject;

const ABI_VERSION: u32 = 1;
const MQTT5_NO_SUBSCRIPTION_EXISTED: u8 = 0x11;
const PROTOCOL_OPTIONS_VERSION_NEUTRAL: u32 = 0;
const PROTOCOL_OPTIONS_V5: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_bytes_view_t {
    pub data: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_string_view_t {
    pub data: *const c_char,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct rumqttc_user_property_t {
    pub struct_size: u32,
    pub name: rumqttc_string_view_t,
    pub value: rumqttc_string_view_t,
}

#[repr(C)]
pub struct rumqttc_v5_publish_properties_t {
    pub struct_size: u32,
    pub response_topic: rumqttc_string_view_t,
    pub response_topic_present: u8,
    pub correlation_data_present: u8,
    pub content_type_present: u8,
    pub payload_format_present: u8,
    pub correlation_data: rumqttc_bytes_view_t,
    pub content_type: rumqttc_string_view_t,
    pub payload_format_indicator: u32,
    pub topic_alias: u32,
    pub message_expiry_present: u8,
    pub reserved: [u8; 3],
    pub message_expiry_interval: u32,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_publish_options_t {
    pub struct_size: u32,
    pub qos: u32,
    pub retain: u8,
    pub reserved: [u8; 3],
    pub protocol_options: u32,
    pub v5_properties: *const rumqttc_v5_publish_properties_t,
}

#[repr(C)]
pub struct rumqttc_v5_subscription_options_t {
    pub struct_size: u32,
    pub no_local: u8,
    pub retain_as_published: u8,
    pub reserved: [u8; 2],
    pub retain_forward_rule: u32,
}

#[repr(C)]
pub struct rumqttc_subscription_t {
    pub struct_size: u32,
    pub filter: rumqttc_string_view_t,
    pub qos: u32,
    pub protocol_options: u32,
    pub v5_options: *const rumqttc_v5_subscription_options_t,
}

#[repr(C)]
pub struct rumqttc_v5_subscribe_properties_t {
    pub struct_size: u32,
    pub subscription_identifier_present: u8,
    pub reserved: [u8; 3],
    pub subscription_identifier: u32,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_subscribe_options_t {
    pub struct_size: u32,
    pub protocol_options: u32,
    pub v5_properties: *const rumqttc_v5_subscribe_properties_t,
}

#[repr(C)]
pub struct rumqttc_v5_unsubscribe_properties_t {
    pub struct_size: u32,
    pub user_properties: *const rumqttc_user_property_t,
    pub user_property_count: usize,
}

#[repr(C)]
pub struct rumqttc_unsubscribe_options_t {
    pub struct_size: u32,
    pub protocol_options: u32,
    pub v5_properties: *const rumqttc_v5_unsubscribe_properties_t,
}

#[repr(C)]
pub struct rumqttc_diagnostics_t {
    pub struct_size: u32,
    pub connected: u8,
    pub disconnecting: u8,
    pub outbound_drained: u8,
    pub reserved: u8,
    pub pending_requests: u64,
    pub queued_requests: u64,
    pub inflight_publishes: u32,
    pub max_inflight_publishes: u32,
    pub pending_subscribes: u64,
    pub pending_unsubscribes: u64,
}

/// Opaque C handle.
pub struct rumqttc_config {
    inner: ConfigHandle,
}

/// Opaque C handle.
pub struct rumqttc_client {
    inner: ClientObject,
}

/// Opaque C handle.
pub struct rumqttc_completion {
    inner: CompletionObject,
}

/// Opaque C handle.
pub struct rumqttc_event {
    inner: EventObject,
}

/// Opaque C handle.
pub struct rumqttc_error {
    inner: ErrorHandle,
}

fn boundary(
    error_out: *mut *mut rumqttc_error,
    panic_client: *mut rumqttc_client,
    operation: impl FnOnce() -> Result<(), ErrorHandle>,
) -> u32 {
    if !error_out.is_null() {
        // SAFETY: Non-null output locations are required by the C contract to
        // point to writable caller-owned storage.
        unsafe { *error_out = ptr::null_mut() };
    }
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => OK,
        Ok(Err(error)) => publish_error(error_out, error),
        Err(payload) => {
            if !panic_client.is_null() {
                // SAFETY: The pointer is supplied by the caller as the affected
                // live client handle and is only borrowed for this call.
                unsafe { &(*panic_client).inner }.poison();
            }
            publish_error(
                error_out,
                ErrorHandle::internal(format!(
                    "panic contained at C ABI boundary: {}",
                    crate::panic::message(payload.as_ref())
                )),
            )
        }
    }
}

fn publish_error(error_out: *mut *mut rumqttc_error, error: ErrorHandle) -> u32 {
    let status = error.status;
    if !error_out.is_null() {
        let error = Box::new(rumqttc_error { inner: error });
        // SAFETY: `boundary` initialized this caller-provided output location.
        unsafe { *error_out = Box::into_raw(error) };
    }
    status
}

unsafe fn bytes_from_view(view: rumqttc_bytes_view_t) -> Result<&'static [u8], ErrorHandle> {
    if view.len == 0 {
        return Ok(&[]);
    }
    if view.data.is_null() {
        return Err(ErrorHandle::argument(
            "NULL byte pointer with nonzero length",
        ));
    }
    // SAFETY: The C caller promises the non-null pointer is readable for `len`
    // bytes during this call. The result is copied before returning to C.
    Ok(unsafe { slice::from_raw_parts(view.data, view.len) })
}

unsafe fn string_from_view(view: rumqttc_string_view_t) -> Result<String, ErrorHandle> {
    let bytes = unsafe {
        bytes_from_view(rumqttc_bytes_view_t {
            data: view.data.cast(),
            len: view.len,
        })?
    };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ErrorHandle::argument("string view is not valid UTF-8"))
}

fn boolean(value: u8, name: &str) -> Result<bool, ErrorHandle> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ErrorHandle::argument(format!("{name} must be 0 or 1"))),
    }
}

fn qos(value: u32) -> Result<QoS, ErrorHandle> {
    match value {
        0 => Ok(QoS::AtMostOnce),
        1 => Ok(QoS::AtLeastOnce),
        2 => Ok(QoS::ExactlyOnce),
        _ => Err(ErrorHandle::argument("unknown QoS value")),
    }
}

unsafe fn config_ref<'a>(config: *const rumqttc_config) -> Result<&'a ConfigHandle, ErrorHandle> {
    if config.is_null() {
        return Err(ErrorHandle::argument("configuration handle is NULL"));
    }
    // SAFETY: A non-null opaque handle must have been returned by this library
    // and remain alive for the duration of the call.
    Ok(unsafe { &(*config).inner })
}

unsafe fn client_ref<'a>(client: *mut rumqttc_client) -> Result<&'a ClientObject, ErrorHandle> {
    if client.is_null() {
        return Err(ErrorHandle::argument("client handle is NULL"));
    }
    // SAFETY: See `config_ref`; ordinary client calls borrow the handle.
    let client = unsafe { &(*client).inner };
    client.ensure_usable().map_err(ErrorHandle::state)?;
    Ok(client)
}

unsafe fn client_ref_for_shutdown<'a>(
    client: *mut rumqttc_client,
) -> Result<&'a ClientObject, ErrorHandle> {
    if client.is_null() {
        return Err(ErrorHandle::argument("client handle is NULL"));
    }
    Ok(unsafe { &(*client).inner })
}

unsafe fn completion_ref<'a>(
    completion: *const rumqttc_completion,
) -> Result<&'a CompletionObject, ErrorHandle> {
    if completion.is_null() {
        return Err(ErrorHandle::argument("completion handle is NULL"));
    }
    // SAFETY: The opaque completion must be live and library-owned.
    Ok(unsafe { &(*completion).inner })
}

unsafe fn event_ref<'a>(event: *const rumqttc_event) -> Result<&'a EventObject, ErrorHandle> {
    if event.is_null() {
        return Err(ErrorHandle::argument("event handle is NULL"));
    }
    // SAFETY: The opaque event must be live and library-owned.
    Ok(unsafe { &(*event).inner })
}

const fn view_string(value: &str) -> rumqttc_string_view_t {
    rumqttc_string_view_t {
        data: value.as_ptr().cast(),
        len: value.len(),
    }
}

const fn view_bytes(value: &[u8]) -> rumqttc_bytes_view_t {
    rumqttc_bytes_view_t {
        data: value.as_ptr(),
        len: value.len(),
    }
}

unsafe fn write_optional<T>(out: *mut T, value: T) {
    if !out.is_null() {
        unsafe { *out = value };
    }
}

fn struct_size<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("C ABI struct size exceeds uint32_t")
}

fn core_error(error: &rumqttc_wrapper_core::Error, operation_id: Option<u64>) -> ErrorHandle {
    ErrorHandle::from_core(error, operation_id)
}

fn client_error(error: ClientError) -> ErrorHandle {
    match error {
        ClientError::Core(error) => core_error(&error, None),
        ClientError::State(message) => ErrorHandle::state(message),
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::missing_const_for_fn)]
pub extern "C" fn rumqttc_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
#[allow(clippy::missing_const_for_fn)]
pub extern "C" fn rumqttc_library_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_new(
    protocol: u32,
    out: *mut *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        // SAFETY: Required writable output location per the C contract.
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("configuration output is NULL"));
        }
        let inner = ConfigHandle::new(protocol)
            .ok_or_else(|| ErrorHandle::argument("unknown MQTT protocol value"))?;
        // SAFETY: `out` was checked and initialized above.
        unsafe { *out = Box::into_raw(Box::new(rumqttc_config { inner })) };
        Ok(())
    })
}

unsafe fn destroy_box<T>(handle: *mut T) {
    if !handle.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(unsafe { Box::from_raw(handle) });
        }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_destroy(handle: *mut rumqttc_config) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_destroy(handle: *mut rumqttc_completion) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_destroy(handle: *mut rumqttc_event) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_destroy(handle: *mut rumqttc_error) {
    unsafe { destroy_box(handle) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_destroy_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if client.is_null() {
            return Ok(());
        }
        let inner = unsafe { client_ref_for_shutdown(client) }?;
        inner
            .close_now(Duration::from_millis(timeout_ms))
            .map_err(client_error)?;
        drop(unsafe { Box::from_raw(client) });
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_abandon(client: *mut rumqttc_client) {
    if !client.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut client = unsafe { Box::from_raw(client) };
            client.inner.abandon();
            drop(client);
        }));
    }
}

fn config_update(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
    update: impl FnOnce(&mut rumqttc_wrapper_core::ClientConfig) -> Result<(), ErrorHandle>,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        // SAFETY: Validated and borrowed only for this call.
        let config = unsafe { config_ref(config) }?;
        let mut inner = config
            .inner
            .lock()
            .map_err(|_| ErrorHandle::state("configuration lock is poisoned"))?;
        update(&mut inner)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_broker(
    config: *mut rumqttc_config,
    host: rumqttc_string_view_t,
    port: u16,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    // SAFETY: Input is copied inside this call.
    let host = unsafe { string_from_view(host) };
    boundary(error_out, ptr::null_mut(), || {
        let host = host?;
        if port == 0 {
            return Err(ErrorHandle::argument("broker port must be nonzero"));
        }
        // SAFETY: Validated opaque handle.
        unsafe { config_ref(config) }?
            .update(|config| {
                config.common.broker_host = host;
                config.common.broker_port = port;
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_client_id(
    config: *mut rumqttc_config,
    client_id: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let client_id = unsafe { string_from_view(client_id) };
    boundary(error_out, ptr::null_mut(), || {
        let client_id = client_id?;
        unsafe { config_ref(config) }?
            .update(|config| {
                config.common.client_id = client_id;
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_username(
    config: *mut rumqttc_config,
    username: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let username = unsafe { string_from_view(username) };
    boundary(error_out, ptr::null_mut(), || {
        let username = username?;
        unsafe { config_ref(config) }?
            .update(|config| {
                config.common.username = Some(username);
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_username(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.username = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_password(
    config: *mut rumqttc_config,
    password: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let password = unsafe { bytes_from_view(password) }.map(<[u8]>::to_vec);
    boundary(error_out, ptr::null_mut(), || {
        let password = password?;
        unsafe { config_ref(config) }?
            .update(|config| {
                config.common.password = Some(Bytes::from(password));
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_clear_password(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.password = None;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_tcp(
    config: *mut rumqttc_config,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_transport_tcp(config);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_tls(
    config: *mut rumqttc_config,
    ca: rumqttc_bytes_view_t,
    certificate: rumqttc_bytes_view_t,
    private_key: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let ca = unsafe { bytes_from_view(ca) }.map(<[u8]>::to_vec);
    let certificate = unsafe { bytes_from_view(certificate) }.map(<[u8]>::to_vec);
    let private_key = unsafe { bytes_from_view(private_key) }.map(<[u8]>::to_vec);
    boundary(error_out, ptr::null_mut(), || {
        let tls = tls_config(ca?, certificate?, private_key?);
        unsafe { config_ref(config) }?
            .update(|config| {
                set_transport_tls(config, tls);
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_websocket(
    config: *mut rumqttc_config,
    url: rumqttc_string_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let url = unsafe { string_from_view(url) };
    boundary(error_out, ptr::null_mut(), || {
        let url = url?;
        unsafe { config_ref(config) }?
            .update(|config| {
                set_transport_websocket(config, url);
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_transport_wss(
    config: *mut rumqttc_config,
    url: rumqttc_string_view_t,
    ca: rumqttc_bytes_view_t,
    certificate: rumqttc_bytes_view_t,
    private_key: rumqttc_bytes_view_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let url = unsafe { string_from_view(url) };
    let ca = unsafe { bytes_from_view(ca) }.map(<[u8]>::to_vec);
    let certificate = unsafe { bytes_from_view(certificate) }.map(<[u8]>::to_vec);
    let private_key = unsafe { bytes_from_view(private_key) }.map(<[u8]>::to_vec);
    boundary(error_out, ptr::null_mut(), || {
        let tls = tls_config(ca?, certificate?, private_key?);
        let url = url?;
        unsafe { config_ref(config) }?
            .update(|config| {
                set_transport_wss(config, url, tls);
                Ok(())
            })
            .map_err(ErrorHandle::state)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_keep_alive_seconds(
    config: *mut rumqttc_config,
    value: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_keep_alive(config, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_connection_timeout_seconds(
    config: *mut rumqttc_config,
    value: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_connection_timeout(config, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_event_delivery_timeout_ms(
    config: *mut rumqttc_config,
    value: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_event_delivery_timeout(config, value);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_request_capacity(
    config: *mut rumqttc_config,
    capacity: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.request_channel_capacity = capacity as usize;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_event_capacity(
    config: *mut rumqttc_config,
    capacity: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.event_buffer_capacity = capacity as usize;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_ack_mode(
    config: *mut rumqttc_config,
    mode: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        set_ack_mode(config, mode).map_err(ErrorHandle::argument)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_incoming_packet_limit(
    config: *mut rumqttc_config,
    bytes: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    config_update(config, error_out, |config| {
        config.common.incoming_packet_size_limit = bytes;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_emit_outgoing_events(
    config: *mut rumqttc_config,
    enabled: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let enabled = boolean(enabled, "enabled");
    config_update(config, error_out, |config| {
        config.common.emit_outgoing_events = enabled?;
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v311_clean_session(
    config: *mut rumqttc_config,
    clean_session: u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let clean_session = boolean(clean_session, "clean_session");
    config_update(config, error_out, |config| {
        set_v311_clean_session(config, clean_session?).map_err(ErrorHandle::argument)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_config_set_v5_session(
    config: *mut rumqttc_config,
    clean_start: u8,
    expiry_present: u8,
    expiry_seconds: u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    let clean_start = boolean(clean_start, "clean_start");
    let expiry_present = boolean(expiry_present, "expiry_present");
    config_update(config, error_out, |config| {
        set_v5_session(config, clean_start?, expiry_present?, expiry_seconds)
            .map_err(ErrorHandle::argument)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_start(
    config: *const rumqttc_config,
    out: *mut *mut rumqttc_client,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = ptr::null_mut() };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("client output is NULL"));
        }
        let config = unsafe { config_ref(config) }?
            .clone_config()
            .map_err(ErrorHandle::state)?;
        let inner = ClientObject::start(config).map_err(|error| core_error(&error, None))?;
        unsafe { *out = Box::into_raw(Box::new(rumqttc_client { inner })) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        let client = unsafe { client_ref(client) }?;
        match client
            .close(Duration::from_millis(timeout_ms))
            .map_err(client_error)?
        {
            Ok(_) => Ok(()),
            Err(error) => Err(core_error(&error, None)),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_close_now_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, client, || {
        let client = unsafe { client_ref_for_shutdown(client) }?;
        client
            .close_now(Duration::from_millis(timeout_ms))
            .map_err(client_error)
    })
}

unsafe fn parse_v5_properties(
    properties: *const rumqttc_v5_publish_properties_t,
) -> Result<Option<V5PublishProperties>, ErrorHandle> {
    if properties.is_null() {
        return Ok(None);
    }
    let properties = unsafe { &*properties };
    if properties.struct_size < struct_size::<rumqttc_v5_publish_properties_t>() {
        return Err(ErrorHandle::argument("v5 properties struct is too small"));
    }
    let response_topic = boolean(properties.response_topic_present, "response_topic_present")?
        .then(|| unsafe { string_from_view(properties.response_topic) })
        .transpose()?;
    let correlation_data = boolean(
        properties.correlation_data_present,
        "correlation_data_present",
    )?
    .then(|| unsafe { bytes_from_view(properties.correlation_data) }.map(Bytes::copy_from_slice))
    .transpose()?;
    let content_type = boolean(properties.content_type_present, "content_type_present")?
        .then(|| unsafe { string_from_view(properties.content_type) })
        .transpose()?;
    let payload_format_indicator =
        boolean(properties.payload_format_present, "payload_format_present")?
            .then(|| {
                u8::try_from(properties.payload_format_indicator).map_err(|_| {
                    ErrorHandle::argument("payload format indicator does not fit in uint8_t")
                })
            })
            .transpose()?;
    let message_expiry_interval =
        boolean(properties.message_expiry_present, "message_expiry_present")?
            .then_some(properties.message_expiry_interval);
    let user_properties = unsafe {
        parse_user_properties(properties.user_properties, properties.user_property_count)
    }?;
    let topic_alias = match properties.topic_alias {
        0 => None,
        alias => Some(
            u16::try_from(alias)
                .map_err(|_| ErrorHandle::argument("topic alias exceeds uint16_t"))?,
        ),
    };
    Ok(Some(V5PublishProperties {
        response_topic,
        correlation_data,
        content_type,
        payload_format_indicator,
        topic_alias,
        subscription_identifiers: Vec::new(),
        message_expiry_interval,
        user_properties,
    }))
}

unsafe fn publish_command(
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
) -> Result<PublishCommand, ErrorHandle> {
    let topic = unsafe { string_from_view(topic) }?;
    let payload = Bytes::copy_from_slice(unsafe { bytes_from_view(payload) }?);
    let (qos, retain, protocol) = if options.is_null() {
        (
            QoS::AtMostOnce,
            false,
            PublishProtocolOptions::VersionNeutral,
        )
    } else {
        let options = unsafe { &*options };
        if options.struct_size < struct_size::<rumqttc_publish_options_t>() {
            return Err(ErrorHandle::argument("publish options struct is too small"));
        }
        let properties = unsafe { parse_v5_properties(options.v5_properties) }?;
        let protocol = match (options.protocol_options, properties) {
            (PROTOCOL_OPTIONS_VERSION_NEUTRAL, None) => PublishProtocolOptions::VersionNeutral,
            (PROTOCOL_OPTIONS_VERSION_NEUTRAL, Some(_)) => {
                return Err(ErrorHandle::argument(
                    "version-neutral publish options cannot contain MQTT 5 properties",
                ));
            }
            (PROTOCOL_OPTIONS_V5, Some(properties)) => PublishProtocolOptions::V5(properties),
            (PROTOCOL_OPTIONS_V5, None) => {
                return Err(ErrorHandle::argument(
                    "MQTT 5 publish options require a v5 properties struct",
                ));
            }
            _ => {
                return Err(ErrorHandle::argument(
                    "unknown publish protocol-options selector",
                ));
            }
        };
        (
            qos(options.qos)?,
            boolean(options.retain, "retain")?,
            protocol,
        )
    };
    Ok(PublishCommand {
        topic,
        payload,
        qos,
        retain,
        protocol,
    })
}

unsafe fn parse_user_properties(
    properties: *const rumqttc_user_property_t,
    count: usize,
) -> Result<Vec<(String, String)>, ErrorHandle> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if properties.is_null() {
        return Err(ErrorHandle::argument(
            "NULL user-property pointer with nonzero count",
        ));
    }
    unsafe { slice::from_raw_parts(properties, count) }
        .iter()
        .map(|property| {
            if property.struct_size < struct_size::<rumqttc_user_property_t>() {
                return Err(ErrorHandle::argument("user-property struct is too small"));
            }
            Ok((unsafe { string_from_view(property.name) }?, unsafe {
                string_from_view(property.value)
            }?))
        })
        .collect()
}

fn admit(client: *mut rumqttc_client, command: Command) -> Result<Admission, ErrorHandle> {
    let client = unsafe { client_ref(client) }?;
    client
        .handle
        .try_admit(command)
        .map_err(|error| core_error(&error, None))
}

fn write_admission(
    admission: Admission,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
) {
    if !operation_id_out.is_null() {
        unsafe { *operation_id_out = admission.operation_id.get() };
    }
    if !completion_out.is_null() {
        unsafe {
            *completion_out = Box::into_raw(Box::new(rumqttc_completion {
                inner: CompletionObject::new(admission.completion),
            }));
        }
    }
}

fn publish_impl(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !operation_id_out.is_null() {
        unsafe { *operation_id_out = 0 };
    }
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() && completion_out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        let command = unsafe { publish_command(topic, payload, options) }?;
        write_admission(
            admit(client, Command::Publish(command))?,
            operation_id_out,
            completion_out,
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_publish(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    publish_impl(
        client,
        topic,
        payload,
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_publish_tracked(
    client: *mut rumqttc_client,
    topic: rumqttc_string_view_t,
    payload: rumqttc_bytes_view_t,
    options: *const rumqttc_publish_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    publish_impl(
        client,
        topic,
        payload,
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

unsafe fn subscriptions(
    values: *const rumqttc_subscription_t,
    count: usize,
) -> Result<Vec<Subscription>, ErrorHandle> {
    if count == 0 {
        return Err(ErrorHandle::argument(
            "at least one subscription is required",
        ));
    }
    if values.is_null() {
        return Err(ErrorHandle::argument("subscription pointer is NULL"));
    }
    unsafe { slice::from_raw_parts(values, count) }
        .iter()
        .map(|value| {
            if value.struct_size < struct_size::<rumqttc_subscription_t>() {
                return Err(ErrorHandle::argument("subscription struct is too small"));
            }
            Ok(Subscription {
                filter: unsafe { string_from_view(value.filter) }?,
                qos: qos(value.qos)?,
                protocol: match value.protocol_options {
                    PROTOCOL_OPTIONS_VERSION_NEUTRAL if value.v5_options.is_null() => {
                        SubscriptionProtocolOptions::VersionNeutral
                    }
                    PROTOCOL_OPTIONS_VERSION_NEUTRAL => {
                        return Err(ErrorHandle::argument(
                            "version-neutral subscription cannot contain MQTT 5 options",
                        ));
                    }
                    PROTOCOL_OPTIONS_V5 if value.v5_options.is_null() => {
                        return Err(ErrorHandle::argument(
                            "MQTT 5 subscription requires a v5 options struct",
                        ));
                    }
                    PROTOCOL_OPTIONS_V5 => {
                        let options = unsafe { &*value.v5_options };
                        if options.struct_size < struct_size::<rumqttc_v5_subscription_options_t>()
                        {
                            return Err(ErrorHandle::argument(
                                "v5 subscription-options struct is too small",
                            ));
                        }
                        let retain_forward_rule = match options.retain_forward_rule {
                            0 => V5RetainForwardRule::OnEverySubscribe,
                            1 => V5RetainForwardRule::OnNewSubscribe,
                            2 => V5RetainForwardRule::Never,
                            _ => {
                                return Err(ErrorHandle::argument(
                                    "unknown retain-forward-rule value",
                                ));
                            }
                        };
                        SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                            no_local: boolean(options.no_local, "no_local")?,
                            retain_as_published: boolean(
                                options.retain_as_published,
                                "retain_as_published",
                            )?,
                            retain_forward_rule,
                        })
                    }
                    _ => {
                        return Err(ErrorHandle::argument(
                            "unknown subscription protocol-options selector",
                        ));
                    }
                },
            })
        })
        .collect()
}

unsafe fn subscribe_protocol_options(
    options: *const rumqttc_subscribe_options_t,
) -> Result<SubscribeProtocolOptions, ErrorHandle> {
    if options.is_null() {
        return Ok(SubscribeProtocolOptions::VersionNeutral);
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_subscribe_options_t>() {
        return Err(ErrorHandle::argument(
            "subscribe-options struct is too small",
        ));
    }
    match options.protocol_options {
        PROTOCOL_OPTIONS_VERSION_NEUTRAL if options.v5_properties.is_null() => {
            Ok(SubscribeProtocolOptions::VersionNeutral)
        }
        PROTOCOL_OPTIONS_VERSION_NEUTRAL => Err(ErrorHandle::argument(
            "version-neutral subscribe options cannot contain MQTT 5 properties",
        )),
        PROTOCOL_OPTIONS_V5 if options.v5_properties.is_null() => Err(ErrorHandle::argument(
            "MQTT 5 subscribe options require a v5 properties struct",
        )),
        PROTOCOL_OPTIONS_V5 => {
            let properties = unsafe { &*options.v5_properties };
            if properties.struct_size < struct_size::<rumqttc_v5_subscribe_properties_t>() {
                return Err(ErrorHandle::argument(
                    "v5 subscribe-properties struct is too small",
                ));
            }
            let subscription_identifier = boolean(
                properties.subscription_identifier_present,
                "subscription_identifier_present",
            )?
            .then_some(properties.subscription_identifier as usize);
            Ok(SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier,
                user_properties: unsafe {
                    parse_user_properties(
                        properties.user_properties,
                        properties.user_property_count,
                    )
                }?,
            }))
        }
        _ => Err(ErrorHandle::argument(
            "unknown subscribe protocol-options selector",
        )),
    }
}

fn subscribe_impl(
    client: *mut rumqttc_client,
    values: *const rumqttc_subscription_t,
    count: usize,
    options: *const rumqttc_subscribe_options_t,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !operation_id_out.is_null() {
        unsafe { *operation_id_out = 0 };
    }
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() && completion_out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        let command = SubscribeCommand {
            filters: unsafe { subscriptions(values, count) }?,
            protocol: unsafe { subscribe_protocol_options(options) }?,
        };
        write_admission(
            admit(client, Command::Subscribe(command))?,
            operation_id_out,
            completion_out,
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_subscribe(
    client: *mut rumqttc_client,
    subscriptions: *const rumqttc_subscription_t,
    count: usize,
    options: *const rumqttc_subscribe_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    subscribe_impl(
        client,
        subscriptions,
        count,
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_subscribe_tracked(
    client: *mut rumqttc_client,
    subscriptions: *const rumqttc_subscription_t,
    count: usize,
    options: *const rumqttc_subscribe_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    subscribe_impl(
        client,
        subscriptions,
        count,
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

unsafe fn filters(
    values: *const rumqttc_string_view_t,
    count: usize,
) -> Result<Vec<String>, ErrorHandle> {
    if count == 0 {
        return Err(ErrorHandle::argument(
            "at least one topic filter is required",
        ));
    }
    if values.is_null() {
        return Err(ErrorHandle::argument("topic-filter pointer is NULL"));
    }
    unsafe { slice::from_raw_parts(values, count) }
        .iter()
        .copied()
        .map(|view| unsafe { string_from_view(view) })
        .collect()
}

unsafe fn unsubscribe_protocol_options(
    options: *const rumqttc_unsubscribe_options_t,
) -> Result<UnsubscribeProtocolOptions, ErrorHandle> {
    if options.is_null() {
        return Ok(UnsubscribeProtocolOptions::VersionNeutral);
    }
    let options = unsafe { &*options };
    if options.struct_size < struct_size::<rumqttc_unsubscribe_options_t>() {
        return Err(ErrorHandle::argument(
            "unsubscribe-options struct is too small",
        ));
    }
    match options.protocol_options {
        PROTOCOL_OPTIONS_VERSION_NEUTRAL if options.v5_properties.is_null() => {
            Ok(UnsubscribeProtocolOptions::VersionNeutral)
        }
        PROTOCOL_OPTIONS_VERSION_NEUTRAL => Err(ErrorHandle::argument(
            "version-neutral unsubscribe options cannot contain MQTT 5 properties",
        )),
        PROTOCOL_OPTIONS_V5 if options.v5_properties.is_null() => Err(ErrorHandle::argument(
            "MQTT 5 unsubscribe options require a v5 properties struct",
        )),
        PROTOCOL_OPTIONS_V5 => {
            let properties = unsafe { &*options.v5_properties };
            if properties.struct_size < struct_size::<rumqttc_v5_unsubscribe_properties_t>() {
                return Err(ErrorHandle::argument(
                    "v5 unsubscribe-properties struct is too small",
                ));
            }
            Ok(UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties {
                user_properties: unsafe {
                    parse_user_properties(
                        properties.user_properties,
                        properties.user_property_count,
                    )
                }?,
            }))
        }
        _ => Err(ErrorHandle::argument(
            "unknown unsubscribe protocol-options selector",
        )),
    }
}

fn unsubscribe_impl(
    client: *mut rumqttc_client,
    values: *const rumqttc_string_view_t,
    count: usize,
    options: *const rumqttc_unsubscribe_options_t,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !operation_id_out.is_null() {
        unsafe { *operation_id_out = 0 };
    }
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() && completion_out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        write_admission(
            admit(
                client,
                Command::Unsubscribe(UnsubscribeCommand {
                    filters: unsafe { filters(values, count) }?,
                    protocol: unsafe { unsubscribe_protocol_options(options) }?,
                }),
            )?,
            operation_id_out,
            completion_out,
        );
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_unsubscribe(
    client: *mut rumqttc_client,
    filters: *const rumqttc_string_view_t,
    count: usize,
    options: *const rumqttc_unsubscribe_options_t,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsubscribe_impl(
        client,
        filters,
        count,
        options,
        operation_id_out,
        ptr::null_mut(),
        error_out,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_unsubscribe_tracked(
    client: *mut rumqttc_client,
    filters: *const rumqttc_string_view_t,
    count: usize,
    options: *const rumqttc_unsubscribe_options_t,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsubscribe_impl(
        client,
        filters,
        count,
        options,
        ptr::null_mut(),
        completion_out,
        error_out,
    )
}

fn acknowledge_impl(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    operation_id_out: *mut u64,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !operation_id_out.is_null() {
        unsafe { *operation_id_out = 0 };
    }
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if operation_id_out.is_null() && completion_out.is_null() {
            return Err(ErrorHandle::argument("operation output is NULL"));
        }
        let _client = unsafe { client_ref(client) }?;
        let event = unsafe { event_ref(event) }?;
        let mut token = event
            .ack
            .lock()
            .map_err(|_| ErrorHandle::state("event acknowledgement lock is poisoned"))?;
        let ack = token
            .take()
            .ok_or_else(|| ErrorHandle::state("event has no available acknowledgement"))?;
        let admission = match admit(client, Command::Acknowledge(ack)) {
            Ok(admission) => admission,
            Err(error) => {
                *token = Some(ack);
                return Err(error);
            }
        };
        drop(token);
        write_admission(admission, operation_id_out, completion_out);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_try_acknowledge(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    operation_id_out: *mut u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(client, event, operation_id_out, ptr::null_mut(), error_out)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_acknowledge_tracked(
    client: *mut rumqttc_client,
    event: *mut rumqttc_event,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    acknowledge_impl(client, event, ptr::null_mut(), completion_out, error_out)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_diagnostics_tracked(
    client: *mut rumqttc_client,
    completion_out: *mut *mut rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !completion_out.is_null() {
        unsafe { *completion_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if completion_out.is_null() {
            return Err(ErrorHandle::argument("completion output is NULL"));
        }
        write_admission(
            admit(client, Command::Diagnostics)?,
            ptr::null_mut(),
            completion_out,
        );
        Ok(())
    })
}

fn observe_completion(
    completion: &CompletionObject,
    timeout: Option<Duration>,
) -> Result<Option<Completion>, ErrorHandle> {
    let result = match timeout {
        Some(timeout) => Some(completion.wait(timeout).map_err(ErrorHandle::state)?),
        None => completion.poll().map_err(ErrorHandle::state)?,
    };
    match result {
        None => Ok(None),
        Some(Ok(result)) => Ok(Some(result)),
        Some(Err(error)) => Err(core_error(&error, Some(completion.operation_id))),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_poll(
    completion: *const rumqttc_completion,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let completion = unsafe { completion_ref(completion) }?;
        if observe_completion(completion, None)?.is_none() {
            return Err(ErrorHandle::plain(
                WOULD_BLOCK,
                crate::error::ERROR_NONE,
                "operation is still pending",
            )
            .with_operation(completion.operation_id));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_wait_timeout_ms(
    completion: *const rumqttc_completion,
    timeout_ms: u64,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        let completion = unsafe { completion_ref(completion) }?;
        observe_completion(completion, Some(Duration::from_millis(timeout_ms))).map(|_| ())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_operation_id(
    completion: *const rumqttc_completion,
    out: *mut u64,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("operation ID output is NULL"));
        }
        let completion = unsafe { completion_ref(completion) }?;
        unsafe { *out = completion.operation_id };
        Ok(())
    })
}

const fn completion_kind(completion: &Completion) -> u32 {
    match completion {
        Completion::Publish(PublishCompletion::Qos0Flushed) => 1,
        Completion::Publish(PublishCompletion::Qos1Acknowledged) => 2,
        Completion::Publish(PublishCompletion::Qos2Completed) => 3,
        Completion::Subscribe(_) => 4,
        Completion::Unsubscribe(_) => 5,
        Completion::Acknowledged => 6,
        Completion::Diagnostics(_) => 7,
        Completion::GracefulShutdown => 8,
        Completion::ImmediateShutdown => 9,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_kind(
    completion: *const rumqttc_completion,
    out: *mut u32,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("completion-kind output is NULL"));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let terminal = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::state("completion is not ready"))?;
        unsafe { *out = completion_kind(&terminal) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_result_count(
    completion: *const rumqttc_completion,
    out: *mut usize,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("result-count output is NULL"));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let terminal = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::state("completion is not ready"))?;
        let count = match terminal {
            Completion::Subscribe(result) => result.results.len(),
            Completion::Unsubscribe(result) => result.results.as_ref().map_or(0, Vec::len),
            _ => return Err(ErrorHandle::state("completion has no per-filter results")),
        };
        unsafe { *out = count };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_result_at(
    completion: *const rumqttc_completion,
    index: usize,
    success_out: *mut u8,
    qos_out: *mut u32,
    reason_present_out: *mut u8,
    reason_out: *mut u8,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    unsafe {
        write_optional(success_out, 0);
        write_optional(qos_out, 0);
        write_optional(reason_present_out, 0);
        write_optional(reason_out, 0);
    }
    boundary(error_out, ptr::null_mut(), || {
        if success_out.is_null()
            && qos_out.is_null()
            && reason_present_out.is_null()
            && reason_out.is_null()
        {
            return Err(ErrorHandle::argument(
                "at least one per-filter result output is required",
            ));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let terminal = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::state("completion is not ready"))?;
        let (success, granted_qos, reason) = match terminal {
            Completion::Subscribe(result) => match result.results.get(index) {
                Some(SubscribeResult::Granted(qos)) => (true, *qos as u32, None),
                Some(SubscribeResult::Rejected(reason)) => (false, 0, Some(reason.code)),
                None => return Err(ErrorHandle::argument("result index is out of bounds")),
            },
            Completion::Unsubscribe(result) => {
                match result.results.as_ref().and_then(|v| v.get(index)) {
                    Some(UnsubscribeResult::Success) => (true, 0, None),
                    Some(UnsubscribeResult::NoSubscriptionExisted) => {
                        (true, 0, Some(MQTT5_NO_SUBSCRIPTION_EXISTED))
                    }
                    Some(UnsubscribeResult::Rejected(reason)) => (false, 0, Some(reason.code)),
                    None => return Err(ErrorHandle::argument("result index is unavailable")),
                }
            }
            _ => return Err(ErrorHandle::state("completion has no per-filter results")),
        };
        unsafe {
            write_optional(success_out, u8::from(success));
            write_optional(qos_out, granted_qos);
            write_optional(reason_present_out, u8::from(reason.is_some()));
            write_optional(reason_out, reason.unwrap_or(0));
        }
        Ok(())
    })
}

fn fill_diagnostics(
    value: &DiagnosticsSnapshot,
    out: &mut rumqttc_diagnostics_t,
) -> Result<(), ErrorHandle> {
    if out.struct_size < struct_size::<rumqttc_diagnostics_t>() {
        return Err(ErrorHandle::argument("diagnostics struct is too small"));
    }
    out.connected = u8::from(value.connected);
    out.disconnecting = u8::from(value.disconnecting);
    out.outbound_drained = u8::from(value.outbound_drained);
    out.reserved = 0;
    out.pending_requests = value.pending_requests as u64;
    out.queued_requests = value.queued_requests as u64;
    out.inflight_publishes = u32::from(value.inflight_publishes);
    out.max_inflight_publishes = u32::from(value.max_inflight_publishes);
    out.pending_subscribes = value.pending_subscribes as u64;
    out.pending_unsubscribes = value.pending_unsubscribes as u64;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_completion_diagnostics(
    completion: *const rumqttc_completion,
    out: *mut rumqttc_diagnostics_t,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    boundary(error_out, ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("diagnostics output is NULL"));
        }
        let completion = unsafe { completion_ref(completion) }?;
        let terminal = observe_completion(completion, None)?
            .ok_or_else(|| ErrorHandle::state("completion is not ready"))?;
        let Completion::Diagnostics(value) = terminal else {
            return Err(ErrorHandle::state("completion is not a diagnostics result"));
        };
        fill_diagnostics(&value, unsafe { &mut *out })
    })
}

fn event_recv_impl(
    client: *mut rumqttc_client,
    timeout: Option<Duration>,
    event_out: *mut *mut rumqttc_event,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !event_out.is_null() {
        unsafe { *event_out = ptr::null_mut() };
    }
    boundary(error_out, client, || {
        if event_out.is_null() {
            return Err(ErrorHandle::argument("event output is NULL"));
        }
        let client = unsafe { client_ref(client) }?;
        let event = client.recv(timeout).map_err(client_error)?;
        let Some(event) = event else {
            let status = if timeout.is_some() {
                TIMEOUT
            } else {
                WOULD_BLOCK
            };
            return Err(ErrorHandle::plain(
                status,
                crate::error::ERROR_NONE,
                "no event is available",
            ));
        };
        unsafe {
            *event_out = Box::into_raw(Box::new(rumqttc_event {
                inner: EventObject::new(event),
            }));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_event_try_recv(
    client: *mut rumqttc_client,
    event_out: *mut *mut rumqttc_event,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    event_recv_impl(client, None, event_out, error_out)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_client_event_recv_timeout_ms(
    client: *mut rumqttc_client,
    timeout_ms: u64,
    event_out: *mut *mut rumqttc_event,
    error_out: *mut *mut rumqttc_error,
) -> u32 {
    event_recv_impl(
        client,
        Some(Duration::from_millis(timeout_ms)),
        event_out,
        error_out,
    )
}

const fn event_kind(event: &WrapperEvent) -> u32 {
    match event {
        WrapperEvent::Connected { .. } => 1,
        WrapperEvent::Disconnected { .. } => 2,
        WrapperEvent::IncomingPublish(_) => 3,
        WrapperEvent::Outgoing(_) => 4,
        WrapperEvent::GracefulShutdownCompleted => 5,
        WrapperEvent::DriverTerminated(_) => 6,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_kind(event: *const rumqttc_event, out: *mut u32) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("event-kind output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        unsafe { *out = event_kind(&event.event) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_connected(
    event: *const rumqttc_event,
    protocol_out: *mut u32,
    session_present_out: *mut u8,
) -> u32 {
    unsafe {
        write_optional(protocol_out, 0);
        write_optional(session_present_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if protocol_out.is_null() && session_present_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one connected-event output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Connected {
            protocol,
            session_present,
        } = event.event
        else {
            return Err(ErrorHandle::state("event is not a connected event"));
        };
        let protocol = match protocol {
            ProtocolVersion::V311 => 1,
            ProtocolVersion::V5 => 2,
        };
        unsafe {
            write_optional(protocol_out, protocol);
            write_optional(session_present_out, u8::from(session_present));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_disconnected(
    event: *const rumqttc_event,
    phase_out: *mut u32,
    event_error_out: *mut *mut rumqttc_error,
) -> u32 {
    if !phase_out.is_null() {
        unsafe { *phase_out = 0 };
    }
    if !event_error_out.is_null() {
        unsafe { *event_error_out = ptr::null_mut() };
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if phase_out.is_null() && event_error_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one disconnect-event output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let (phase, error) = match &event.event {
            WrapperEvent::Disconnected { phase, error } => (*phase as u32 + 1, error),
            WrapperEvent::DriverTerminated(error) => (0, error),
            _ => return Err(ErrorHandle::state("event has no disconnect error")),
        };
        unsafe {
            write_optional(phase_out, phase);
            if !event_error_out.is_null() {
                *event_error_out = Box::into_raw(Box::new(rumqttc_error {
                    inner: core_error(error, None),
                }));
            }
        }
        Ok(())
    })
}

fn incoming(event: &EventObject) -> Result<&IncomingPublish, ErrorHandle> {
    match &event.event {
        WrapperEvent::IncomingPublish(publish) => Ok(publish),
        _ => Err(ErrorHandle::state("event is not an incoming publish")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_publish(
    event: *const rumqttc_event,
    topic_out: *mut rumqttc_string_view_t,
    payload_out: *mut rumqttc_bytes_view_t,
    qos_out: *mut u32,
    retain_out: *mut u8,
    duplicate_out: *mut u8,
    ack_available_out: *mut u8,
) -> u32 {
    unsafe {
        write_optional(
            topic_out,
            rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(
            payload_out,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
        write_optional(qos_out, 0);
        write_optional(retain_out, 0);
        write_optional(duplicate_out, 0);
        write_optional(ack_available_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if topic_out.is_null()
            && payload_out.is_null()
            && qos_out.is_null()
            && retain_out.is_null()
            && duplicate_out.is_null()
            && ack_available_out.is_null()
        {
            return Err(ErrorHandle::argument(
                "at least one publish-event output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let publish = incoming(event)?;
        let topic = std::str::from_utf8(&publish.topic)
            .map_err(|_| ErrorHandle::internal("incoming MQTT topic is not UTF-8"))?;
        let ack_available = event
            .ack
            .lock()
            .map_err(|_| ErrorHandle::state("event acknowledgement lock is poisoned"))?
            .is_some();
        unsafe {
            write_optional(topic_out, view_string(topic));
            write_optional(payload_out, view_bytes(&publish.payload));
            write_optional(qos_out, publish.qos as u32);
            write_optional(retain_out, u8::from(publish.retain));
            write_optional(duplicate_out, u8::from(publish.duplicate));
            write_optional(ack_available_out, u8::from(ack_available));
        }
        Ok(())
    })
}

fn v5_properties(event: &EventObject) -> Result<&V5PublishProperties, ErrorHandle> {
    incoming(event)?
        .v5_properties
        .as_ref()
        .ok_or_else(|| ErrorHandle::state("event has no MQTT 5 publish properties"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_response_topic(
    event: *const rumqttc_event,
    present_out: *mut u8,
    out: *mut rumqttc_string_view_t,
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
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one property output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let value = &v5_properties(event)?.response_topic;
        let view = value.as_deref().map_or_else(
            || rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
            view_string,
        );
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(out, view);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_correlation_data(
    event: *const rumqttc_event,
    present_out: *mut u8,
    out: *mut rumqttc_bytes_view_t,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(
            out,
            rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
        );
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one property output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let value = &v5_properties(event)?.correlation_data;
        let view = value.as_deref().map_or_else(
            || rumqttc_bytes_view_t {
                data: ptr::null(),
                len: 0,
            },
            view_bytes,
        );
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(out, view);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_content_type(
    event: *const rumqttc_event,
    present_out: *mut u8,
    out: *mut rumqttc_string_view_t,
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
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one property output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let value = &v5_properties(event)?.content_type;
        let view = value.as_deref().map_or_else(
            || rumqttc_string_view_t {
                data: ptr::null(),
                len: 0,
            },
            view_string,
        );
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(out, view);
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_scalar(
    event: *const rumqttc_event,
    property: u32,
    present_out: *mut u8,
    out: *mut u64,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one scalar-property output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let properties = v5_properties(event)?;
        let value = match property {
            1 => properties.payload_format_indicator.map(u64::from),
            2 => properties.topic_alias.map(u64::from),
            3 => properties.message_expiry_interval.map(u64::from),
            _ => return Err(ErrorHandle::argument("unknown scalar property selector")),
        };
        unsafe {
            write_optional(present_out, u8::from(value.is_some()));
            write_optional(out, value.unwrap_or(0));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_subscription_identifier_count(
    event: *const rumqttc_event,
    out: *mut usize,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "subscription-identifier count output is NULL",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        unsafe { *out = v5_properties(event)?.subscription_identifiers.len() };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_subscription_identifier_at(
    event: *const rumqttc_event,
    index: usize,
    out: *mut u64,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument(
                "subscription-identifier output is NULL",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let value = v5_properties(event)?
            .subscription_identifiers
            .get(index)
            .ok_or_else(|| {
                ErrorHandle::argument("subscription-identifier index is out of bounds")
            })?;
        unsafe { *out = *value as u64 };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_user_property_count(
    event: *const rumqttc_event,
    out: *mut usize,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("user-property count output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        unsafe { *out = v5_properties(event)?.user_properties.len() };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_v5_user_property_at(
    event: *const rumqttc_event,
    index: usize,
    name_out: *mut rumqttc_string_view_t,
    value_out: *mut rumqttc_string_view_t,
) -> u32 {
    let empty = rumqttc_string_view_t {
        data: ptr::null(),
        len: 0,
    };
    unsafe {
        write_optional(name_out, empty);
        write_optional(value_out, empty);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if name_out.is_null() && value_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one user-property output is required",
            ));
        }
        let event = unsafe { event_ref(event) }?;
        let (name, value) = v5_properties(event)?
            .user_properties
            .get(index)
            .ok_or_else(|| ErrorHandle::argument("user-property index is out of bounds"))?;
        unsafe {
            write_optional(name_out, view_string(name));
            write_optional(value_out, view_string(value));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_event_outgoing_kind(
    event: *const rumqttc_event,
    out: *mut u32,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("outgoing-kind output is NULL"));
        }
        let event = unsafe { event_ref(event) }?;
        let WrapperEvent::Outgoing(activity) = &event.event else {
            return Err(ErrorHandle::state("event is not an outgoing event"));
        };
        let kind = match activity {
            OutgoingActivity::Publish => 1,
            OutgoingActivity::Subscribe => 2,
            OutgoingActivity::Unsubscribe => 3,
            OutgoingActivity::Acknowledgement => 4,
            OutgoingActivity::Ping => 5,
            OutgoingActivity::Disconnect => 6,
            OutgoingActivity::AwaitAcknowledgement => 7,
            OutgoingActivity::Other => 8,
        };
        unsafe { *out = kind };
        Ok(())
    })
}

unsafe fn error_ref<'a>(error: *const rumqttc_error) -> Result<&'a ErrorHandle, ErrorHandle> {
    if error.is_null() {
        return Err(ErrorHandle::argument("error handle is NULL"));
    }
    Ok(unsafe { &(*error).inner })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_status(error: *const rumqttc_error, out: *mut u32) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("error accessor output is NULL"));
        }
        unsafe { *out = error_ref(error)?.status };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_kind(error: *const rumqttc_error, out: *mut u32) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("error accessor output is NULL"));
        }
        unsafe { *out = error_ref(error)?.kind };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_message(
    error: *const rumqttc_error,
    out: *mut rumqttc_string_view_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("error string output is NULL"));
        }
        unsafe { *out = view_string(&error_ref(error)?.message) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_source_chain(
    error: *const rumqttc_error,
    out: *mut rumqttc_string_view_t,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if out.is_null() {
            return Err(ErrorHandle::argument("error string output is NULL"));
        }
        unsafe { *out = view_string(&error_ref(error)?.source_chain) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_flags(
    error: *const rumqttc_error,
    retryable_out: *mut u8,
    ambiguous_out: *mut u8,
) -> u32 {
    unsafe {
        write_optional(retryable_out, 0);
        write_optional(ambiguous_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if retryable_out.is_null() && ambiguous_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one error flag output is required",
            ));
        }
        let error = unsafe { error_ref(error) }?;
        unsafe {
            write_optional(retryable_out, u8::from(error.retryable));
            write_optional(ambiguous_out, u8::from(error.ambiguous));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_broker_reason(
    error: *const rumqttc_error,
    present_out: *mut u8,
    reason_out: *mut u8,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(reason_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && reason_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one broker-reason output is required",
            ));
        }
        let error = unsafe { error_ref(error) }?;
        unsafe {
            write_optional(present_out, u8::from(error.broker_reason.is_some()));
            write_optional(reason_out, error.broker_reason.unwrap_or(0));
        }
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_error_operation_id(
    error: *const rumqttc_error,
    present_out: *mut u8,
    operation_id_out: *mut u64,
) -> u32 {
    unsafe {
        write_optional(present_out, 0);
        write_optional(operation_id_out, 0);
    }
    boundary(ptr::null_mut(), ptr::null_mut(), || {
        if present_out.is_null() && operation_id_out.is_null() {
            return Err(ErrorHandle::argument(
                "at least one operation-ID output is required",
            ));
        }
        let error = unsafe { error_ref(error) }?;
        unsafe {
            write_optional(present_out, u8::from(error.operation_id.is_some()));
            write_optional(operation_id_out, error.operation_id.unwrap_or(0));
        }
        Ok(())
    })
}

unsafe fn copy_out(
    source: *const u8,
    source_len: usize,
    buffer: *mut c_void,
    capacity: usize,
    required_out: *mut usize,
) -> Result<(), ErrorHandle> {
    if required_out.is_null() {
        return Err(ErrorHandle::argument("required-length output is NULL"));
    }
    unsafe { *required_out = source_len };
    if capacity < source_len {
        return Err(ErrorHandle::argument("copy-out buffer is too small"));
    }
    if source_len == 0 {
        return Ok(());
    }
    if source.is_null() {
        return Err(ErrorHandle::argument(
            "NULL source pointer with nonzero length",
        ));
    }
    if buffer.is_null() {
        return Err(ErrorHandle::argument("copy-out buffer is NULL"));
    }
    unsafe { ptr::copy(source, buffer.cast(), source_len) };
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_bytes_copy(
    view: rumqttc_bytes_view_t,
    buffer: *mut u8,
    capacity: usize,
    required_out: *mut usize,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || unsafe {
        copy_out(view.data, view.len, buffer.cast(), capacity, required_out)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rumqttc_string_copy(
    view: rumqttc_string_view_t,
    buffer: *mut c_char,
    capacity: usize,
    required_out: *mut usize,
) -> u32 {
    boundary(ptr::null_mut(), ptr::null_mut(), || unsafe {
        copy_out(
            view.data.cast(),
            view.len,
            buffer.cast(),
            capacity,
            required_out,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_inconsistent_empty_views() {
        let view = rumqttc_bytes_view_t {
            data: ptr::null(),
            len: 1,
        };
        assert!(unsafe { bytes_from_view(view) }.is_err());
    }

    #[test]
    fn accepts_null_zero_length_view() {
        let view = rumqttc_bytes_view_t {
            data: ptr::null(),
            len: 0,
        };
        assert_eq!(unsafe { bytes_from_view(view) }.unwrap(), &[]);
    }

    #[test]
    fn absent_payload_format_ignores_its_value() {
        let mut properties: rumqttc_v5_publish_properties_t = unsafe { std::mem::zeroed() };
        properties.struct_size = struct_size::<rumqttc_v5_publish_properties_t>();
        properties.payload_format_present = 0;
        properties.payload_format_indicator = u32::MAX;

        let parsed = unsafe { parse_v5_properties(&raw const properties) }
            .unwrap()
            .unwrap();
        assert_eq!(parsed.payload_format_indicator, None);
    }

    #[test]
    fn parses_discriminated_subscription_extensions() {
        let user_property = rumqttc_user_property_t {
            struct_size: struct_size::<rumqttc_user_property_t>(),
            name: view_string("key"),
            value: view_string("value"),
        };
        let filter_options = rumqttc_v5_subscription_options_t {
            struct_size: struct_size::<rumqttc_v5_subscription_options_t>(),
            no_local: 1,
            retain_as_published: 1,
            reserved: [0; 2],
            retain_forward_rule: 2,
        };
        let subscription = rumqttc_subscription_t {
            struct_size: struct_size::<rumqttc_subscription_t>(),
            filter: view_string("a/#"),
            qos: 1,
            protocol_options: PROTOCOL_OPTIONS_V5,
            v5_options: &raw const filter_options,
        };
        let parsed = unsafe { subscriptions(&raw const subscription, 1) }.unwrap();
        assert!(matches!(
            parsed[0].protocol,
            SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                no_local: true,
                retain_as_published: true,
                retain_forward_rule: V5RetainForwardRule::Never,
            })
        ));

        let properties = rumqttc_v5_subscribe_properties_t {
            struct_size: struct_size::<rumqttc_v5_subscribe_properties_t>(),
            subscription_identifier_present: 1,
            reserved: [0; 3],
            subscription_identifier: 7,
            user_properties: &raw const user_property,
            user_property_count: 1,
        };
        let options = rumqttc_subscribe_options_t {
            struct_size: struct_size::<rumqttc_subscribe_options_t>(),
            protocol_options: PROTOCOL_OPTIONS_V5,
            v5_properties: &raw const properties,
        };
        assert_eq!(
            unsafe { subscribe_protocol_options(&raw const options) }.unwrap(),
            SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: Some(7),
                user_properties: vec![("key".into(), "value".into())],
            })
        );
    }

    #[test]
    fn rejects_unknown_or_inconsistent_protocol_option_selectors() {
        let options = rumqttc_subscribe_options_t {
            struct_size: struct_size::<rumqttc_subscribe_options_t>(),
            protocol_options: 99,
            v5_properties: ptr::null(),
        };
        assert!(unsafe { subscribe_protocol_options(&raw const options) }.is_err());

        let properties: rumqttc_v5_unsubscribe_properties_t = unsafe { std::mem::zeroed() };
        let options = rumqttc_unsubscribe_options_t {
            struct_size: struct_size::<rumqttc_unsubscribe_options_t>(),
            protocol_options: PROTOCOL_OPTIONS_VERSION_NEUTRAL,
            v5_properties: &raw const properties,
        };
        assert!(unsafe { unsubscribe_protocol_options(&raw const options) }.is_err());
    }

    #[test]
    fn copy_out_reports_required_size_before_capacity_error() {
        let mut required = 0;
        let result = unsafe { copy_out(b"abc".as_ptr(), 3, ptr::null_mut(), 0, &raw mut required) };
        assert!(result.is_err());
        assert_eq!(required, 3);
    }

    #[test]
    fn copy_out_allows_overlapping_ranges() {
        let mut bytes = *b"abcdef";
        let mut required = 0;
        let bytes_ptr = bytes.as_mut_ptr();
        let result = unsafe {
            copy_out(
                bytes_ptr.cast_const(),
                4,
                bytes_ptr.add(1).cast(),
                4,
                &raw mut required,
            )
        };
        assert!(result.is_ok());
        assert_eq!(required, 4);
        assert_eq!(&bytes, b"aabcdf");

        let mut same_buffer = *b"same";
        let same_buffer_ptr = same_buffer.as_mut_ptr();
        let result = unsafe {
            copy_out(
                same_buffer_ptr.cast_const(),
                same_buffer.len(),
                same_buffer_ptr.cast(),
                same_buffer.len(),
                &raw mut required,
            )
        };
        assert!(result.is_ok());
        assert_eq!(&same_buffer, b"same");
    }

    #[test]
    fn panic_is_contained_and_reported_as_an_owned_internal_error() {
        let mut error = ptr::null_mut();
        let status = boundary(
            &raw mut error,
            ptr::null_mut(),
            || -> Result<(), ErrorHandle> { panic!("injected boundary panic") },
        );
        assert_eq!(status, crate::error::INTERNAL_ERROR);
        assert!(!error.is_null());
        assert_eq!(unsafe { (*error).inner.kind }, 11);
        unsafe { destroy_box(error) };
    }
}
